//! Groovy parsing via tree-sitter: package, top-level declarations and imports.
//! Also covers Gradle build scripts (`.gradle`), which are Groovy.

use tree_sitter::{Node, Parser};

use crate::lang::{
    end_line_of, finish_symbols, leading_comment_start, line_of, symbol_from_node, Call, Import,
    ImportKind, Member, MemberKind, Receiver, Source, Symbol, SymbolKind,
};

const COMMENTS: &[&str] = &["comment", "line_comment", "block_comment"];

pub struct GroovyParser {
    parser: Parser,
}

const TYPE_DECLS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "trait_declaration",
    "annotation_type_declaration",
    "record_declaration",
];

impl GroovyParser {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .expect("tree-sitter-groovy grammar failed to load");
        Self { parser }
    }

    pub fn parse(&mut self, src: &str) -> Option<Source> {
        let tree = self.parser.parse(src, None)?;
        let root = tree.root_node();
        let mut out = Source::default();

        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            match child.kind() {
                "package_declaration" => out.package = declared_name(child, src),
                "import_declaration" => out.imports.push(parse_import(child, src)),
                kind if TYPE_DECLS.contains(&kind) => {
                    if let Some(name) = child.child_by_field_name("name") {
                        push_type(&mut out, child, name, src);
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

impl Default for GroovyParser {
    fn default() -> Self {
        Self::new()
    }
}

fn declared_name(node: Node<'_>, src: &str) -> Option<String> {
    if let Some(name) = node.child_by_field_name("name") {
        let text = text_of(name, src);
        if !text.is_empty() {
            return Some(text);
        }
    }
    let mut cursor = node.walk();
    let name = node
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "qualified_name" | "identifier"))
        .map(|child| text_of(child, src))
        .filter(|name| !name.is_empty());
    name
}

fn parse_import(node: Node<'_>, src: &str) -> Import {
    let path = declared_name(node, src).unwrap_or_default();
    let text = text_of(node, src);
    let kind = if text.contains('*') {
        ImportKind::Wildcard
    } else if text.split_whitespace().any(|word| word == "static") {
        ImportKind::Static
    } else {
        ImportKind::Type
    };
    Import { path, kind }
}

fn push_type(out: &mut Source, declaration: Node<'_>, name: Node<'_>, src: &str) {
    let text = text_of(name, src);
    if !text.is_empty() {
        let mut symbol = symbol_from_node(declaration, &text, SymbolKind::Type, COMMENTS, src);
        collect_members(declaration, src, &mut symbol);
        out.symbols.push(symbol);
        out.types.push(text);
    }
}

/// Methods, constructors, fields and supertypes of one type, with the calls
/// each member makes and the receiver types the resolver needs.
fn collect_members(class: Node<'_>, src: &str, symbol: &mut Symbol) {
    symbol.supertypes = supertypes(class, src);
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
                    end_line: end_line_of(src, start, member.end_byte()),
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
                    end_line: end_line_of(src, start, member.end_byte()),
                    body_start,
                    params: parameter_names(member.child_by_field_name("parameters"), src),
                    locals: Vec::new(),
                    calls: calls_in(member, src),
                });
            }
            // `static { }`: class context, never stubbed, calls still count.
            "static_initializer" => {
                let start = leading_comment_start(member, COMMENTS);
                let body_start = member
                    .child_by_field_name("body")
                    .map(|body| body.start_byte());
                symbol.members.push(Member {
                    name: "static".to_string(),
                    kind: MemberKind::Constructor,
                    start,
                    end: member.end_byte(),
                    line: line_of(src, start),
                    end_line: end_line_of(src, start, member.end_byte()),
                    body_start,
                    params: Vec::new(),
                    locals: Vec::new(),
                    calls: calls_in(member, src),
                });
            }
            "field_declaration" => {
                symbol.fields.extend(field_names(member, src));
            }
            _ => {}
        }
    }
}

fn field_names(node: Node<'_>, src: &str) -> Vec<(String, String)> {
    let Some(ty) = node
        .child_by_field_name("type")
        .and_then(|ty| type_name(ty, src))
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

fn parameter_names(parameters: Option<Node<'_>>, src: &str) -> Vec<(String, String)> {
    let Some(parameters) = parameters else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() != "formal_parameter" {
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
            "local_variable_declaration" => {
                if let Some(ty) = node
                    .child_by_field_name("type")
                    .and_then(|ty| type_name(ty, src))
                {
                    if let Some(name) = node.child_by_field_name("name") {
                        out.push((text_of(name, src), ty.clone()));
                    }
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
            "catch_clause" => {
                if let Some(parameter) = node.child_by_field_name("parameter") {
                    if let (Some(ty), Some(name)) = (
                        parameter
                            .child_by_field_name("type")
                            .and_then(|ty| type_name(ty, src)),
                        parameter.child_by_field_name("name"),
                    ) {
                        out.push((text_of(name, src), ty));
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

/// Simple name of a type node: `Repo`, `List` for `List<Repo>`, `Repo` for
/// `a.b.Repo` and `Repo[]`.
fn type_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" => {
            let text = text_of(node, src);
            (!text.is_empty()).then_some(text)
        }
        "generic_type" => node
            .child_by_field_name("base")
            .and_then(|base| type_name(base, src)),
        "qualified_type" | "array_type" | "varargs_type" | "multi_type" => {
            let mut cursor = node.walk();
            let mut last = None;
            for child in node.named_children(&mut cursor) {
                if let Some(name) = type_name(child, src) {
                    last = Some(name);
                }
            }
            last
        }
        _ => None,
    }
}

fn supertypes(class: Node<'_>, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = class.walk();
    for child in class.named_children(&mut cursor) {
        if !matches!(child.kind(), "superclass" | "super_interfaces") {
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
        if node.kind() == "method_invocation" {
            if let Some(function) = node.child_by_field_name("function") {
                if let Some(call) = call_of(function, src, line_of(src, node.start_byte())) {
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

fn call_of(function: Node<'_>, src: &str, line: usize) -> Option<Call> {
    match function.kind() {
        "identifier" => {
            let name = text_of(function, src);
            (!name.is_empty()).then_some(Call {
                name,
                receiver: Receiver::Bare,
                line,
            })
        }
        "field_access" => {
            let name = function
                .child_by_field_name("field")
                .map(|field| text_of(field, src).trim_matches('"').to_string())?;
            let receiver = function
                .child_by_field_name("object")
                .map(|object| receiver_of(object, src))
                .unwrap_or(Receiver::Unknown);
            (!name.is_empty()).then_some(Call {
                name,
                receiver,
                line,
            })
        }
        // `new Repo().size()`.
        "object_creation_expression" => None,
        _ => None,
    }
}

fn receiver_of(node: Node<'_>, src: &str) -> Receiver {
    match node.kind() {
        "identifier" => {
            let text = text_of(node, src);
            match text.as_str() {
                "this" => Receiver::This,
                "super" => Receiver::Super,
                _ => Receiver::Name(text),
            }
        }
        "super" => Receiver::Super,
        "field_access" => node
            .child_by_field_name("field")
            .map(|field| Receiver::Name(text_of(field, src).trim_matches('"').to_string()))
            .unwrap_or(Receiver::Unknown),
        "object_creation_expression" => node
            .child_by_field_name("type")
            .and_then(|ty| type_name(ty, src))
            .map(Receiver::Name)
            .unwrap_or(Receiver::Unknown),
        _ => Receiver::Unknown,
    }
}

/// Groovy exposes type names as `type_identifier`; object receivers such as
/// `DemoDocument.create()` are the `object` field of a `field_access`.
fn collect_type_uses(root: Node<'_>, src: &str, out: &mut Vec<(String, usize)>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "type_identifier" | "identifier" => {
                let is_receiver = node.parent().is_some_and(|parent| {
                    parent.kind() == "field_access"
                        && parent
                            .child_by_field_name("object")
                            .is_some_and(|object| object.id() == node.id())
                });
                if node.kind() == "type_identifier" || is_receiver {
                    let text = text_of(node, src);
                    if !text.is_empty() {
                        out.push((text, node.start_byte()));
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

fn text_of<'a>(node: Node<'a>, src: &str) -> String {
    src.get(node.byte_range()).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Source {
        GroovyParser::new().parse(src).expect("parsed")
    }

    #[test]
    fn package_types_and_imports() {
        let source = parse(
            r#"
            package com.example.app

            import com.example.model.User
            import com.example.model.*
            import static com.example.util.Helpers.parse
            import com.example.model.Account as Acct

            class Service implements Runnable {
                User user
                def doc = DemoDocument.create()
                void run() { List<User> users = [] }
            }
            interface Base {}
            trait Named {}
            enum Color { RED }
            "#,
        );
        assert_eq!(source.package.as_deref(), Some("com.example.app"));
        assert_eq!(source.types, vec!["Service", "Base", "Named", "Color"]);
        assert_eq!(
            source
                .imports
                .iter()
                .map(|import| (import.path.as_str(), import.kind))
                .collect::<Vec<_>>(),
            vec![
                ("com.example.model.User", ImportKind::Type),
                ("com.example.model", ImportKind::Wildcard),
                ("com.example.util.Helpers.parse", ImportKind::Static),
                ("com.example.model.Account", ImportKind::Type),
            ]
        );
        for expected in ["User", "List", "DemoDocument", "Runnable"] {
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
            package app

            class Service extends Base implements Api {
                private Repo repo
                def untyped = 1

                Service(Repo repo) { this.repo = repo }

                int used(Local local, Helper h) {
                    Local temp = new Local()
                    temp.ping()
                    h.pong()
                    repo.find()
                    Helper.staticCall()
                    this.inside()
                    super.parent()
                    return 1
                }

                int omitted() { return 1 }
                def inside() {}
                abstract void nothing()
            }
            "#,
        );
        let service = &source.symbols[0];
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
            ("ping", Receiver::Name("temp".to_string())),
            ("pong", Receiver::Name("h".to_string())),
            ("find", Receiver::Name("repo".to_string())),
            ("staticCall", Receiver::Name("Helper".to_string())),
            ("inside", Receiver::This),
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

        let constructor = service
            .members
            .iter()
            .find(|member| member.kind == MemberKind::Constructor)
            .expect("constructor");
        assert_eq!(
            constructor.params,
            vec![("repo".to_string(), "Repo".to_string())]
        );
    }

    #[test]
    fn gradle_script_without_package() {
        let source = parse("plugins { id 'java' }\ndependencies { implementation \"x:y:1\" }\n");
        assert_eq!(source.package, None);
        assert!(source.types.is_empty());
    }
}
