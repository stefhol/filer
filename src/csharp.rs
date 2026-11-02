//! C# parsing via tree-sitter: namespaces (including file-scoped ones), `using`
//! directives and top-level type declarations.

use tree_sitter::{Node, Parser};

use crate::lang::{
    finish_symbols, leading_comment_start, line_of, symbol_from_node, Call, Import, ImportKind,
    Member, MemberKind, Receiver, Source, Symbol, SymbolKind,
};

const COMMENTS: &[&str] = &["comment"];

pub struct CSharpParser {
    parser: Parser,
}

const TYPE_DECLS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "struct_declaration",
    "enum_declaration",
    "record_declaration",
    "delegate_declaration",
];

impl CSharpParser {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .expect("tree-sitter-c-sharp grammar failed to load");
        Self { parser }
    }

    pub fn parse(&mut self, src: &str) -> Option<Source> {
        let tree = self.parser.parse(src, None)?;
        let root = tree.root_node();

        let mut out = Source::default();
        let mut prefix = Vec::new();
        walk_scope(root, src, &mut prefix, &mut out);
        if out.package.is_none() && !prefix.is_empty() {
            out.package = Some(prefix.join("."));
        }

        collect_usings(root, src, &mut out.imports);

        let mut uses = Vec::new();
        collect_type_uses(root, src, &mut uses);
        finish_symbols(&mut out, &uses);

        Some(out)
    }
}

impl Default for CSharpParser {
    fn default() -> Self {
        Self::new()
    }
}

fn walk_scope(node: Node<'_>, src: &str, prefix: &mut Vec<String>, out: &mut Source) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "namespace_declaration" => {
                let depth = prefix.len();
                push_package(prefix, child, src);
                if let Some(body) = child.child_by_field_name("body") {
                    walk_scope(body, src, prefix, out);
                }
                prefix.truncate(depth);
            }
            // `namespace A.B;` applies to every following declaration.
            "file_scoped_namespace_declaration" => push_package(prefix, child, src),
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
                if out.package.is_none() && !prefix.is_empty() {
                    out.package = Some(prefix.join("."));
                }
            }
            _ => {}
        }
    }
}

fn push_package(prefix: &mut Vec<String>, node: Node<'_>, src: &str) {
    let name = node
        .child_by_field_name("name")
        .map(|name| text_of(name, src))
        .unwrap_or_default();
    prefix.extend(
        name.split('.')
            .filter(|segment| !segment.is_empty())
            .map(str::to_string),
    );
}

/// Methods, constructors, fields and supertypes of one type, with the calls
/// each member makes and the receiver types the resolver needs.
fn collect_members(class: Node<'_>, src: &str, symbol: &mut Symbol) {
    symbol.supertypes = supertypes(class, src);

    // Positional record parameters become init properties.
    let mut cursor = class.walk();
    for child in class.named_children(&mut cursor) {
        if child.kind() == "parameter_list" {
            for (name, ty) in parameter_names(Some(child), src) {
                symbol.fields.push((name, ty));
            }
        }
    }

    let Some(body) = class.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        match member.kind() {
            "method_declaration" => {
                let Some(name) = member.child_by_field_name("name") else {
                    continue;
                };
                let name = text_of(name, src);
                if name.is_empty() {
                    continue;
                }
                let start = leading_comment_start(member, COMMENTS);
                let body_start = member
                    .child_by_field_name("body")
                    .map(|body| body.start_byte());
                symbol.members.push(Member {
                    name,
                    kind: MemberKind::Method,
                    start,
                    end: member.end_byte(),
                    line: line_of(src, start),
                    body_start,
                    params: parameter_names(member.child_by_field_name("parameters"), src),
                    locals: local_names(member, src),
                    calls: calls_in(member, src),
                });
            }
            "constructor_declaration" => {
                let start = leading_comment_start(member, COMMENTS);
                let body_start = member
                    .child_by_field_name("body")
                    .map(|body| body.start_byte());
                let name = member
                    .child_by_field_name("name")
                    .map(|name| text_of(name, src))
                    .unwrap_or_else(|| symbol.name.clone());
                symbol.members.push(Member {
                    name,
                    kind: MemberKind::Constructor,
                    start,
                    end: member.end_byte(),
                    line: line_of(src, start),
                    body_start,
                    params: parameter_names(member.child_by_field_name("parameters"), src),
                    locals: Vec::new(),
                    calls: calls_in(member, src),
                });
            }
            "field_declaration" => {
                symbol.fields.extend(field_names(member, src));
            }
            // Auto-properties and expression-bodied properties type receivers
            // but are not methods; their bodies are not part of a call path.
            "property_declaration" => {
                if let (Some(ty), Some(name)) = (
                    member
                        .child_by_field_name("type")
                        .and_then(|ty| type_name(ty, src)),
                    member.child_by_field_name("name"),
                ) {
                    symbol.fields.push((text_of(name, src), ty));
                }
            }
            _ => {}
        }
    }
}

fn field_names(node: Node<'_>, src: &str) -> Vec<(String, String)> {
    let mut cursor = node.walk();
    let declaration = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "variable_declaration");
    let Some(declaration) = declaration else {
        return Vec::new();
    };
    let Some(ty) = declaration
        .child_by_field_name("type")
        .and_then(|ty| type_name(ty, src))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut inner = declaration.walk();
    for declarator in declaration.named_children(&mut inner) {
        if declarator.kind() == "variable_declarator" {
            if let Some(name) = declarator.child_by_field_name("name") {
                out.push((text_of(name, src), ty.clone()));
            }
        }
    }
    out
}

fn parameter_names(parameters: Option<Node<'_>>, src: &str) -> Vec<(String, String)> {
    let Some(parameters) = parameters else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() != "parameter" {
            continue;
        }
        if let (Some(ty), Some(name)) = (
            parameter
                .child_by_field_name("type")
                .and_then(|ty| type_name(ty, src)),
            parameter.child_by_field_name("name"),
        ) {
            out.push((text_of(name, src), ty));
        }
    }
    out
}

fn local_names(member: Node<'_>, src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![member];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "local_declaration_statement" => {
                let mut cursor = node.walk();
                let declaration = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "variable_declaration");
                if let Some(declaration) = declaration {
                    if let Some(ty) = declaration
                        .child_by_field_name("type")
                        .and_then(|ty| type_name(ty, src))
                    {
                        let mut inner = declaration.walk();
                        for declarator in declaration.named_children(&mut inner) {
                            if declarator.kind() == "variable_declarator" {
                                if let Some(name) = declarator.child_by_field_name("name") {
                                    out.push((text_of(name, src), ty.clone()));
                                }
                            }
                        }
                    }
                }
            }
            "foreach_statement" => {
                if let (Some(ty), Some(name)) = (
                    node.child_by_field_name("type")
                        .and_then(|ty| type_name(ty, src)),
                    node.child_by_field_name("left")
                        .filter(|left| left.kind() == "identifier"),
                ) {
                    out.push((text_of(name, src), ty));
                }
            }
            "catch_declaration" => {
                if let (Some(ty), Some(name)) = (
                    node.child_by_field_name("type")
                        .and_then(|ty| type_name(ty, src)),
                    node.child_by_field_name("name"),
                ) {
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

/// Simple name of a type: `Repo`, `List` for `List<Repo>`, `Repo` for
/// `com.example.Repo`, `Repo?` and `Repo[]`.
fn type_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "predefined_type" => {
            let text = text_of(node, src);
            (!text.is_empty()).then_some(text)
        }
        "generic_name" => {
            let mut cursor = node.walk();
            let name = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")
                .map(|child| text_of(child, src));
            name
        }
        "qualified_name" | "alias_qualified_name" => node
            .child_by_field_name("name")
            .and_then(|name| type_name(name, src)),
        "nullable_type" | "array_type" | "pointer_type" | "ref_type" => node
            .child_by_field_name("type")
            .and_then(|ty| type_name(ty, src)),
        _ => None,
    }
}

fn supertypes(class: Node<'_>, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = class.walk();
    for child in class.named_children(&mut cursor) {
        if child.kind() != "base_list" {
            continue;
        }
        let mut inner = child.walk();
        for ty in child.named_children(&mut inner) {
            if let Some(name) = type_name(ty, src) {
                out.push(name);
            }
        }
    }
    out
}

fn calls_in(member: Node<'_>, src: &str) -> Vec<Call> {
    let mut out = Vec::new();
    let mut stack = vec![member];
    while let Some(node) = stack.pop() {
        if node.kind() == "invocation_expression" {
            if let Some(function) = node.child_by_field_name("function") {
                if let Some(call) = call_of(function, src) {
                    out.push(call);
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

fn call_of(function: Node<'_>, src: &str) -> Option<Call> {
    match function.kind() {
        "identifier" => {
            let name = text_of(function, src);
            (!name.is_empty()).then_some(Call {
                name,
                receiver: Receiver::Bare,
            })
        }
        "generic_name" => {
            let mut cursor = function.walk();
            let name = function
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")
                .map(|child| text_of(child, src))?;
            (!name.is_empty()).then_some(Call {
                name,
                receiver: Receiver::Bare,
            })
        }
        "member_access_expression" => {
            let name = function
                .child_by_field_name("name")
                .and_then(|name| simple_name(name, src))?;
            let receiver = function
                .child_by_field_name("expression")
                .map(|expression| receiver_of(expression, src))
                .unwrap_or(Receiver::Unknown);
            (!name.is_empty()).then_some(Call { name, receiver })
        }
        _ => None,
    }
}

/// Name of a member (`Foo` for `Foo` and `Foo<T>`).
fn simple_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "identifier" => Some(text_of(node, src)),
        "generic_name" => {
            let mut cursor = node.walk();
            let name = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")
                .map(|child| text_of(child, src));
            name
        }
        _ => None,
    }
}

fn receiver_of(node: Node<'_>, src: &str) -> Receiver {
    match node.kind() {
        "this" => Receiver::This,
        "base" => Receiver::Super,
        "identifier" => {
            let text = text_of(node, src);
            match text.as_str() {
                "this" => Receiver::This,
                "base" => Receiver::Super,
                _ => Receiver::Name(text),
            }
        }
        "member_access_expression" => node
            .child_by_field_name("name")
            .and_then(|name| simple_name(name, src))
            .map(Receiver::Name)
            .unwrap_or(Receiver::Unknown),
        "object_creation_expression" => node
            .child_by_field_name("type")
            .and_then(|ty| type_name(ty, src))
            .map(Receiver::Name)
            .unwrap_or(Receiver::Unknown),
        _ => Receiver::Unknown,
    }
}

/// `using` directives can appear at any scope, so scan the whole tree.
fn collect_usings(node: Node<'_>, src: &str, out: &mut Vec<Import>) {
    if node.kind() == "using_directive" {
        if let Some(import) = parse_using(node, src) {
            out.push(import);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_usings(child, src, out);
    }
}

fn parse_using(node: Node<'_>, src: &str) -> Option<Import> {
    let alias = node.child_by_field_name("name");
    let mut cursor = node.walk();
    let target = node.named_children(&mut cursor).find(|child| {
        matches!(child.kind(), "qualified_name" | "identifier")
            && alias.is_none_or(|alias| alias.id() != child.id())
    })?;

    let mut path = text_of(target, src);
    if let Some(stripped) = path.strip_prefix("global::") {
        path = stripped.to_string();
    }
    if path.is_empty() {
        return None;
    }

    let text = text_of(node, src);
    let kind = if text.split_whitespace().any(|word| word == "static") {
        ImportKind::Static
    } else if alias.is_some() {
        ImportKind::Type
    } else {
        // A plain `using Some.Namespace;` imports every type in it.
        ImportKind::Wildcard
    };
    Some(Import { path, kind })
}

/// C# has no `type_identifier`; type names are ordinary identifiers. Only
/// identifiers in type positions (and object receivers) are collected.
fn collect_type_uses(root: Node<'_>, src: &str, out: &mut Vec<(String, usize)>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "identifier" => {
                if is_type_position(node) || is_member_receiver(node) {
                    let text = text_of(node, src);
                    if !text.is_empty() {
                        out.push((text, node.start_byte()));
                    }
                }
            }
            "generic_name" => {
                if is_type_position(node) {
                    let mut cursor = node.walk();
                    let first = node.named_children(&mut cursor).next();
                    if let Some(first) = first {
                        if first.kind() == "identifier" {
                            out.push((text_of(first, src), first.start_byte()));
                        }
                    }
                }
            }
            "qualified_name" if is_type_position(node) => {
                if let Some(last) = last_identifier(node) {
                    out.push((text_of(last, src), last.start_byte()));
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

fn is_type_position(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let field_is = |field: &str| {
        parent
            .child_by_field_name(field)
            .is_some_and(|child| child.id() == node.id())
    };
    if field_is("type") || field_is("returns") {
        return true;
    }
    matches!(
        parent.kind(),
        "base_list" | "type_argument_list" | "type_parameter_constraint" | "declaration_pattern"
    )
}

fn is_member_receiver(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "member_access_expression"
            && parent
                .child_by_field_name("expression")
                .is_some_and(|expression| expression.id() == node.id())
    })
}

fn last_identifier(node: Node<'_>) -> Option<Node<'_>> {
    let mut stack = vec![node];
    let mut found: Option<Node<'_>> = None;
    while let Some(current) = stack.pop() {
        if current.kind() == "identifier"
            && found.is_none_or(|found| current.start_byte() > found.start_byte())
        {
            found = Some(current);
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    found
}

fn text_of<'a>(node: Node<'a>, src: &str) -> String {
    src.get(node.byte_range()).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Source {
        CSharpParser::new().parse(src).expect("parsed")
    }

    #[test]
    fn file_scoped_namespace_types_and_usings() {
        let source = parse(
            r#"
            using System;
            using System.Collections.Generic;
            using com.example.model;
            using static com.example.util.Helpers;
            using Alias = com.example.model.Account;
            global using global::System.Text;

            namespace com.example.app;

            public class Service : Base, IRunnable {
                private readonly User user;
                private List<User> users = new();
                public User Current { get; set; }
                public void Run(Acct a) {
                    var doc = DemoDocument.Create();
                    var t = typeof(Account);
                }
            }
            public interface Base {}
            public record Point(int X);
            public struct Vec {}
            public enum Color { Red }
            "#,
        );
        assert_eq!(source.package.as_deref(), Some("com.example.app"));
        assert_eq!(
            source.types,
            vec!["Service", "Base", "Point", "Vec", "Color"]
        );
        assert_eq!(
            source
                .imports
                .iter()
                .map(|import| (import.path.as_str(), import.kind))
                .collect::<Vec<_>>(),
            vec![
                ("System", ImportKind::Wildcard),
                ("System.Collections.Generic", ImportKind::Wildcard),
                ("com.example.model", ImportKind::Wildcard),
                ("com.example.util.Helpers", ImportKind::Static),
                ("com.example.model.Account", ImportKind::Type),
                ("System.Text", ImportKind::Wildcard),
            ]
        );
        for expected in [
            "Base",
            "IRunnable",
            "User",
            "List",
            "Acct",
            "DemoDocument",
            "Account",
        ] {
            assert!(
                source.uses.iter().any(|name| name == expected),
                "missing {expected} in {:?}",
                source.uses
            );
        }
    }

    #[test]
    fn extracts_members_fields_supertypes_and_calls() {
        let source = parse(
            r#"
            namespace app;

            public class Service : Base, Api {
                private readonly Repo repo;
                public Helper Helper { get; set; }
                private int count;

                public Service(Repo repo) { this.repo = repo; }

                public int Used(Local local, Helper h) {
                    Local temp = new Local();
                    temp.Ping();
                    h.Pong();
                    repo.Find();
                    Helper.StaticCall();
                    this.Self();
                    base.Parent();
                    return count;
                }

                public int Omitted() => 1;
                public void Self() {}
                public abstract void Nothing();
            }
            "#,
        );
        let service = &source.symbols[0];
        assert_eq!(service.name, "Service");
        assert_eq!(service.supertypes, vec!["Base", "Api"]);
        for expected in [("repo", "Repo"), ("Helper", "Helper"), ("count", "int")] {
            assert!(
                service
                    .fields
                    .iter()
                    .any(|(name, ty)| name == expected.0 && ty == expected.1),
                "missing {expected:?} in {:?}",
                service.fields
            );
        }

        let used = service
            .members
            .iter()
            .find(|member| member.name == "Used")
            .expect("Used");
        assert_eq!(used.kind, MemberKind::Method);
        assert_eq!(
            used.params,
            vec![
                ("local".to_string(), "Local".to_string()),
                ("h".to_string(), "Helper".to_string()),
            ]
        );
        assert_eq!(used.locals, vec![("temp".to_string(), "Local".to_string())]);
        let receivers: Vec<(&str, &Receiver)> = used
            .calls
            .iter()
            .map(|call| (call.name.as_str(), &call.receiver))
            .collect();
        for (name, receiver) in [
            ("Ping", Receiver::Name("temp".to_string())),
            ("Pong", Receiver::Name("h".to_string())),
            ("Find", Receiver::Name("repo".to_string())),
            ("StaticCall", Receiver::Name("Helper".to_string())),
            ("Self", Receiver::This),
            ("Parent", Receiver::Super),
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

        let omitted = service
            .members
            .iter()
            .find(|member| member.name == "Omitted")
            .expect("Omitted");
        assert!(omitted.body_start.is_some());
        let nothing = service
            .members
            .iter()
            .find(|member| member.name == "Nothing")
            .expect("Nothing");
        assert_eq!(nothing.body_start, None);
    }

    #[test]
    fn braced_and_nested_namespaces() {
        let source = parse(
            r#"
            namespace com.example {
                class A {}
                namespace nested { class B {} }
            }
            "#,
        );
        assert_eq!(source.package.as_deref(), Some("com.example"));
        assert_eq!(source.types, vec!["A", "B"]);
    }
}
