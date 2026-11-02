//! Java parsing via tree-sitter: package, top-level types and imports.

use tree_sitter::{Node, Parser};

use crate::lang::{
    finish_symbols, line_of, symbol_from_node, Call, Import, ImportKind, Member, MemberKind,
    Receiver, Source, Symbol, SymbolKind,
};

pub struct JavaParser {
    parser: Parser,
}

const COMMENTS: &[&str] = &["line_comment", "block_comment"];

const TYPE_DECLS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

impl JavaParser {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .expect("tree-sitter-java grammar failed to load");
        Self { parser }
    }

    /// Parses a Java source file. Returns `None` only when the parser bails out
    /// (invalid encoding or cancelled parse). Files with syntax errors still
    /// yield whatever declarations tree-sitter could recover.
    pub fn parse(&mut self, src: &str) -> Option<Source> {
        let tree = self.parser.parse(src, None)?;
        let root = tree.root_node();
        let mut out = Source::default();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "package_declaration" => out.package = decl_name(child, src),
                "import_declaration" => {
                    if let Some(import) = parse_import(child, src) {
                        out.imports.push(import);
                    }
                }
                kind if TYPE_DECLS.contains(&kind) => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let text = text_of(name, src);
                        if !text.is_empty() {
                            let mut symbol =
                                symbol_from_node(child, &text, SymbolKind::Type, COMMENTS, src);
                            collect_members(child, src, &mut symbol);
                            out.symbols.push(symbol);
                            out.types.push(text);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut uses = Vec::new();
        collect_type_uses(root, src, &mut uses);
        finish_symbols(&mut out, &uses);
        Some(out)
    }
}

impl Default for JavaParser {
    fn default() -> Self {
        Self::new()
    }
}

/// First `identifier` / `scoped_identifier` below `node` (works for package and
/// import declarations, whose name node is a direct child).
fn decl_name<'a>(node: Node<'a>, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|c| matches!(c.kind(), "identifier" | "scoped_identifier"))
        .map(|c| text_of(c, src))
        .filter(|s| !s.is_empty());
    found
}

fn parse_import(node: Node<'_>, src: &str) -> Option<Import> {
    let mut is_static = false;
    let mut wildcard = false;
    let mut path: Option<String> = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "static" => is_static = true,
            "asterisk" => wildcard = true,
            "identifier" | "scoped_identifier" => {
                let text = text_of(child, src);
                if !text.is_empty() {
                    path = Some(text);
                }
            }
            _ => {}
        }
    }

    let path = path?;
    let kind = if wildcard {
        ImportKind::Wildcard
    } else if is_static {
        ImportKind::Static
    } else {
        ImportKind::Type
    };
    Some(Import { path, kind })
}

/// Members of a type: methods (prunable), constructors (kept), fields and
/// supertypes (for receiver typing and `super`/inherited calls).
fn collect_members(class: Node<'_>, src: &str, symbol: &mut Symbol) {
    symbol.supertypes = supertypes(class, src);
    let Some(body) = class.child_by_field_name("body") else {
        return;
    };
    // Enums keep their members inside `enum_body_declarations`; classes,
    // interfaces and records expose them directly.
    let mut scopes = vec![body];
    while let Some(scope) = scopes.pop() {
        let mut cursor = scope.walk();
        for member in scope.named_children(&mut cursor) {
            if member.kind() == "enum_body_declarations" {
                scopes.push(member);
                continue;
            }
            match member.kind() {
                "field_declaration" => symbol.fields.extend(field_names(member, src)),
                "method_declaration"
                | "constructor_declaration"
                | "compact_constructor_declaration" => {
                    let Some(name) = member.child_by_field_name("name") else {
                        continue;
                    };
                    let start = crate::lang::leading_comment_start(member, COMMENTS);
                    let body_start = member
                        .child_by_field_name("body")
                        .map(|body| body.start_byte());
                    symbol.members.push(Member {
                        name: text_of(name, src),
                        kind: if member.kind() == "method_declaration" {
                            MemberKind::Method
                        } else {
                            MemberKind::Constructor
                        },
                        start,
                        end: member.end_byte(),
                        line: line_of(src, start),
                        body_start,
                        params: parameter_names(member, src),
                        locals: local_names(member, src),
                        calls: calls_in(member, src),
                    });
                }
                _ => {}
            }
        }
    }
}

fn supertypes(class: Node<'_>, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for field in ["superclass", "interfaces"] {
        if let Some(node) = class.child_by_field_name(field) {
            collect_base_types(node, src, &mut out);
        }
    }
    out
}

fn collect_base_types(node: Node<'_>, src: &str, out: &mut Vec<String>) {
    if let Some(name) = base_type_name(node, src) {
        out.push(name);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_base_types(child, src, out);
    }
}

/// Raw (non-generic) simple name of a type node: `List` for `List<Foo>`,
/// `Foo` for `pkg.Foo`, the element type for arrays.
fn base_type_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" => {
            let text = text_of(node, src);
            (!text.is_empty()).then_some(text)
        }
        "scoped_type_identifier" | "qualified_name" => node
            .child_by_field_name("name")
            .and_then(|name| base_type_name(name, src)),
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|ty| base_type_name(ty, src)),
        "array_type" => node
            .child_by_field_name("element")
            .and_then(|element| base_type_name(element, src)),
        _ => None,
    }
}

fn field_names(node: Node<'_>, src: &str) -> Vec<(String, String)> {
    let Some(ty) = node
        .child_by_field_name("type")
        .and_then(|ty| base_type_name(ty, src))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            if let Some(name) = child.child_by_field_name("name") {
                out.push((text_of(name, src), ty.clone()));
            }
        }
    }
    out
}

fn parameter_names(member: Node<'_>, src: &str) -> Vec<(String, String)> {
    let Some(parameters) = member.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        let ty = parameter
            .child_by_field_name("type")
            .and_then(|ty| base_type_name(ty, src));
        let name = match parameter.kind() {
            "formal_parameter" => parameter.child_by_field_name("name"),
            "spread_parameter" => {
                let mut inner = parameter.walk();
                let mut found = None;
                for child in parameter.named_children(&mut inner) {
                    if child.kind() == "variable_declarator" {
                        found = Some(child);
                        break;
                    }
                }
                found.and_then(|child| child.child_by_field_name("name"))
            }
            _ => None,
        };
        if let (Some(ty), Some(name)) = (ty, name) {
            out.push((text_of(name, src), ty));
        }
    }
    out
}

fn local_names(member: Node<'_>, src: &str) -> Vec<(String, String)> {
    let Some(body) = member.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "local_variable_declaration" => {
                let ty = node
                    .child_by_field_name("type")
                    .and_then(|ty| base_type_name(ty, src));
                if let Some(ty) = ty {
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        if child.kind() == "variable_declarator" {
                            if let Some(name) = child.child_by_field_name("name") {
                                out.push((text_of(name, src), ty.clone()));
                            }
                        }
                    }
                }
            }
            "enhanced_for_statement" | "catch_formal_parameter" => {
                let ty = node
                    .child_by_field_name("type")
                    .and_then(|ty| base_type_name(ty, src));
                if let (Some(ty), Some(name)) = (ty, node.child_by_field_name("name")) {
                    out.push((text_of(name, src), ty));
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

fn calls_in(member: Node<'_>, src: &str) -> Vec<Call> {
    let mut out = Vec::new();
    let mut stack = vec![member];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "method_invocation" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let receiver = node
                        .child_by_field_name("object")
                        .map_or(Receiver::Bare, |object| receiver_of(object, src));
                    out.push(Call {
                        name: text_of(name, src),
                        receiver,
                    });
                }
            }
            "method_reference" => {
                let mut cursor = node.walk();
                let named: Vec<Node> = node.named_children(&mut cursor).collect();
                if let (Some(first), Some(last)) = (named.first(), named.last()) {
                    let name = text_of(*last, src);
                    if !name.is_empty() && name != "new" {
                        out.push(Call {
                            name,
                            receiver: receiver_of(*first, src),
                        });
                    }
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

fn receiver_of(node: Node<'_>, src: &str) -> Receiver {
    match node.kind() {
        "this" => Receiver::This,
        "super" => Receiver::Super,
        "identifier" | "type_identifier" => Receiver::Name(text_of(node, src)),
        "field_access" | "scoped_identifier" | "qualified_name" => last_receiver_name(node)
            .map(|name| Receiver::Name(text_of(name, src)))
            .unwrap_or(Receiver::Unknown),
        "object_creation_expression" => node
            .child_by_field_name("type")
            .and_then(|ty| base_type_name(ty, src))
            .map(Receiver::Name)
            .unwrap_or(Receiver::Unknown),
        _ => Receiver::Unknown,
    }
}

/// Walks the whole tree collecting every `type_identifier` (class names in
/// type positions, class literals, generics, casts, `new` expressions).
fn collect_type_uses(root: Node<'_>, src: &str, out: &mut Vec<(String, usize)>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "type_identifier" => {
                let text = text_of(node, src);
                if !text.is_empty() {
                    out.push((text, node.start_byte()));
                }
            }
            // `Helpers.helper()` and `Helpers.CONST` put the receiver in an
            // expression position, so it is not a `type_identifier`. Record the
            // last segment of the receiver chain; lowercase variable receivers
            // simply never match a type name in the index.
            "method_invocation" | "field_access" | "method_reference" => {
                if let Some(object) = node.child_by_field_name("object") {
                    push_receiver(object, src, out);
                } else if node.kind() == "method_reference" {
                    if let Some(first) = node.named_child(0) {
                        push_receiver(first, src, out);
                    }
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// Last name of a receiver chain: `Helpers` in `Helpers.helper()`,
/// `LocalHelpers` in `pkg.LocalHelpers.helper()`. `this`/`super` and literals
/// produce nothing.
fn push_receiver(node: Node<'_>, src: &str, out: &mut Vec<(String, usize)>) {
    if let Some(last) = last_receiver_name(node) {
        let text = text_of(last, src);
        if !text.is_empty() && !matches!(text.as_str(), "this" | "super") {
            out.push((text, last.start_byte()));
        }
    }
}

fn last_receiver_name(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "identifier" | "type_identifier" => Some(node),
        "field_access" | "scoped_identifier" | "qualified_name" => node
            .child_by_field_name("field")
            .or_else(|| node.child_by_field_name("name"))
            .filter(|name| matches!(name.kind(), "identifier" | "type_identifier"))
            .or_else(|| {
                node.child_by_field_name("object")
                    .and_then(last_receiver_name)
            }),
        "method_invocation" => node
            .child_by_field_name("name")
            .filter(|name| name.kind() == "identifier"),
        _ => None,
    }
}

fn text_of<'a>(node: Node<'a>, src: &str) -> String {
    src.get(node.byte_range()).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Source {
        JavaParser::new().parse(src).expect("parsed")
    }

    #[test]
    fn package_types_and_imports() {
        let src = r#"
            package com.example.app;

            import java.util.List;
            import static java.util.Collections.emptyList;
            import com.example.util.*;
            import com.example.model.User;

            public class Service implements Runnable {
                public void run() {}
            }
            interface Extra {}
            enum Color { RED }
            record Point(int x, int y) {}
        "#;
        let parsed = parse(src);
        assert_eq!(parsed.package.as_deref(), Some("com.example.app"));
        assert_eq!(parsed.types, vec!["Service", "Extra", "Color", "Point"]);
        assert_eq!(
            parsed
                .imports
                .iter()
                .map(|i| (i.path.as_str(), i.kind))
                .collect::<Vec<_>>(),
            vec![
                ("java.util.List", ImportKind::Type),
                ("java.util.Collections.emptyList", ImportKind::Static),
                ("com.example.util", ImportKind::Wildcard),
                ("com.example.model.User", ImportKind::Type),
            ]
        );
    }

    #[test]
    fn enum_methods_are_members_too() {
        let parsed =
            parse("enum E { A, B; int used() { return helper(); } int helper() { return 1; } }");
        let symbol = &parsed.symbols[0];
        assert_eq!(symbol.name, "E");
        let used = symbol
            .members
            .iter()
            .find(|member| member.name == "used")
            .expect("used");
        assert!(used.calls.iter().any(|call| call.name == "helper"));
        assert!(symbol.members.iter().any(|member| member.name == "helper"));
    }

    #[test]
    fn extracts_members_fields_supertypes_and_calls() {
        let parsed = parse(
            r#"
            package app;

            public class Service extends Base implements Api {
                private Repo repo;
                private int count;

                public Service(Repo repo) { this.repo = repo; }

                // Kept method.
                int used(Helper h) {
                    Local local = new Local();
                    local.ping();
                    h.pong();
                    repo.find();
                    Helper.staticCall();
                    this.internal();
                    super.parent();
                    return count;
                }

                int omitted() { return 1; }
                void internal() { }
                abstract void nothing();
            }
            "#,
        );
        let service = &parsed.symbols[0];
        assert_eq!(service.name, "Service");
        assert_eq!(service.supertypes, vec!["Base", "Api"]);
        assert_eq!(
            service.fields,
            vec![("repo".to_string(), "Repo".to_string())]
        );

        let used = service
            .members
            .iter()
            .find(|member| member.name == "used")
            .expect("used");
        assert_eq!(used.kind, MemberKind::Method);
        assert_eq!(used.params, vec![("h".to_string(), "Helper".to_string())]);
        assert_eq!(
            used.locals,
            vec![("local".to_string(), "Local".to_string())]
        );
        let receivers: Vec<(&str, &Receiver)> = used
            .calls
            .iter()
            .map(|call| (call.name.as_str(), &call.receiver))
            .collect();
        for (name, receiver) in [
            ("ping", Receiver::Name("local".to_string())),
            ("pong", Receiver::Name("h".to_string())),
            ("find", Receiver::Name("repo".to_string())),
            ("staticCall", Receiver::Name("Helper".to_string())),
            ("internal", Receiver::This),
            ("parent", Receiver::Super),
        ] {
            assert!(
                receivers
                    .iter()
                    .any(|(call_name, call_receiver)| *call_name == name
                        && **call_receiver == receiver),
                "missing {name}: {receivers:?}"
            );
        }
        assert!(used.body_start.is_some());

        let nothing = service
            .members
            .iter()
            .find(|member| member.name == "nothing")
            .expect("nothing");
        assert_eq!(nothing.body_start, None);
        assert!(used.start < nothing.start);
    }

    #[test]
    fn static_call_and_field_receivers_are_uses() {
        let parsed = parse(
            r#"
            package app;
            class Caller {
                void run() {
                    LocalHelpers.helper();
                    LocalHelpers.CONST;
                    pkg.LocalHelpers.helper();
                    this.self();
                }
            }
            "#,
        );
        assert!(
            parsed.uses.iter().any(|name| name == "LocalHelpers"),
            "{:?}",
            parsed.uses
        );
        // Method names and `this` are not type references.
        assert!(!parsed.uses.iter().any(|name| name == "helper"));
        assert!(!parsed.uses.iter().any(|name| name == "self"));
        assert!(!parsed.uses.iter().any(|name| name == "this"));
    }

    #[test]
    fn bare_calls_add_no_uses_and_variable_receivers_are_plain_names() {
        let parsed = parse("class A { void run() { helper(); repo.find(); } }");
        assert!(!parsed.uses.iter().any(|name| name == "helper"));
        assert!(parsed.uses.iter().any(|name| name == "repo"));
    }

    #[test]
    fn method_reference_receivers_are_uses() {
        let parsed = parse("class A { Object o = LocalHelpers::helper; }");
        assert!(
            parsed.uses.iter().any(|name| name == "LocalHelpers"),
            "{:?}",
            parsed.uses
        );
        assert!(!parsed.uses.iter().any(|name| name == "helper"));
    }

    #[test]
    fn default_package_and_syntax_errors() {
        let parsed = parse("class Broken { void x( { }");
        assert_eq!(parsed.package, None);
        assert_eq!(parsed.types, vec!["Broken"]);
    }
}
