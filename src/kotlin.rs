//! Kotlin parsing via tree-sitter: package header, top-level declarations and
//! imports (wildcards, aliases, member imports).

use tree_sitter::{Node, Parser};

use crate::lang::{
    end_line_of, finish_symbols, leading_comment_start, line_of, symbol_from_node, Call, Import,
    ImportKind, Member, MemberKind, Receiver, Source, Symbol, SymbolKind,
};

const COMMENTS: &[&str] = &["line_comment", "multiline_comment", "comment"];

pub struct KotlinParser {
    parser: Parser,
}

impl KotlinParser {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .expect("tree-sitter-kotlin grammar failed to load");
        Self { parser }
    }

    pub fn parse(&mut self, src: &str) -> Option<Source> {
        let tree = self.parser.parse(src, None)?;
        let root = tree.root_node();
        let mut out = Source::default();

        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            match child.kind() {
                "package_header" => out.package = package_name(child, src),
                "import" => out.imports.push(parse_import(child, src)),
                "class_declaration" | "object_declaration" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        push_type(&mut out, child, name, src);
                    }
                }
                // `typealias Foo = Bar`: the alias name is the `type` field.
                "type_alias" => {
                    if let Some(name) = child.child_by_field_name("type") {
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

impl Default for KotlinParser {
    fn default() -> Self {
        Self::new()
    }
}

fn package_name(node: Node<'_>, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    let name = node
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "qualified_identifier" | "identifier"))
        .map(|child| text_of(child, src))
        .filter(|name| !name.is_empty());
    name
}

fn parse_import(node: Node<'_>, src: &str) -> Import {
    let mut path = String::new();
    let mut cursor = node.walk();
    if let Some(name) = node
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "qualified_identifier" | "identifier"))
    {
        path = text_of(name, src);
    }

    let text = text_of(node, src);
    let kind = if text.contains('*') {
        ImportKind::Wildcard
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

/// Methods, constructors, fields and supertypes of one class/object, with the
/// calls each method makes and the receiver types the resolver needs.
fn collect_members(class: Node<'_>, src: &str, symbol: &mut Symbol) {
    symbol.supertypes = supertypes(class, src);

    // Primary-constructor `val`/`var` parameters are fields, like Java's
    // constructor-assigned ones.
    if let Some(primary) = named_child(class, "primary_constructor") {
        if let Some(parameters) = named_child(primary, "class_parameters") {
            let mut cursor = parameters.walk();
            for parameter in parameters.named_children(&mut cursor) {
                if parameter.kind() != "class_parameter" {
                    continue;
                }
                let mut is_property = false;
                let mut inner = parameter.walk();
                for child in parameter.children(&mut inner) {
                    if matches!(child.kind(), "val" | "var") {
                        is_property = true;
                        break;
                    }
                }
                if !is_property {
                    continue;
                }
                if let (Some(name), Some(ty)) =
                    (parameter_name(parameter, src), type_name(parameter, src))
                {
                    symbol.fields.push((name, ty));
                }
            }
        }
    }

    let Some(body) =
        named_child(class, "class_body").or_else(|| named_child(class, "enum_class_body"))
    else {
        return;
    };
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        match member.kind() {
            "function_declaration" => {
                let Some(name) = member.child_by_field_name("name") else {
                    continue;
                };
                let name = text_of(name, src);
                if name.is_empty() {
                    continue;
                }
                let start = leading_comment_start(member, COMMENTS);
                let body_start =
                    named_child(member, "function_body").and_then(|body| body_marker(body));
                let params =
                    parameters_simple(named_child(member, "function_value_parameters"), src);
                symbol.members.push(Member {
                    name,
                    kind: MemberKind::Method,
                    start,
                    end: member.end_byte(),
                    line: line_of(src, start),
                    end_line: end_line_of(src, start, member.end_byte()),
                    body_start,
                    params,
                    locals: local_names(member, src),
                    calls: calls_in(member, src),
                });
            }
            "secondary_constructor" => {
                let start = leading_comment_start(member, COMMENTS);
                let body_start = named_child(member, "block").map(|block| block.start_byte());
                symbol.members.push(Member {
                    name: symbol.name.clone(),
                    kind: MemberKind::Constructor,
                    start,
                    end: member.end_byte(),
                    line: line_of(src, start),
                    end_line: end_line_of(src, start, member.end_byte()),
                    body_start,
                    params: parameters_simple(
                        named_child(member, "function_value_parameters"),
                        src,
                    ),
                    locals: Vec::new(),
                    calls: calls_in(member, src),
                });
            }
            // `init { }` is kept as class context, never stubbed, but its calls
            // still count for a class/file seed.
            "anonymous_initializer" => {
                let start = leading_comment_start(member, COMMENTS);
                let body_start = named_child(member, "block").map(|block| block.start_byte());
                symbol.members.push(Member {
                    name: "init".to_string(),
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
            "property_declaration" => {
                if let Some((name, ty)) = property_name_and_type(member, src) {
                    symbol.fields.push((name, ty));
                }
            }
            _ => {}
        }
    }
}

fn named_child<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == kind);
    found
}

/// `{` of a block body, or the `=` of an expression body, so the rendered
/// signature stops before the body.
fn body_marker(body: Node<'_>) -> Option<usize> {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "block" || child.kind() == "=" {
            return Some(child.start_byte());
        }
    }
    None
}

/// Name and declared type of `val x: Type`, from either a property or a local.
fn property_name_and_type(node: Node<'_>, src: &str) -> Option<(String, String)> {
    let declaration = named_child(node, "variable_declaration")?;
    let name = named_child(declaration, "identifier")?;
    let ty = type_name(declaration, src)?;
    Some((text_of(name, src), ty))
}

fn parameter_name(node: Node<'_>, src: &str) -> Option<String> {
    named_child(node, "identifier").map(|name| text_of(name, src))
}

/// Label of one parameter, including its modifiers.
fn parameters_simple(parameters: Option<Node<'_>>, src: &str) -> Vec<(String, String)> {
    let Some(parameters) = parameters else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() != "parameter" {
            continue;
        }
        if let (Some(name), Some(ty)) = (parameter_name(parameter, src), type_name(parameter, src))
        {
            out.push((name, ty));
        }
    }
    out
}

fn local_names(member: Node<'_>, src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![member];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "property_declaration" => {
                if let Some(entry) = property_name_and_type(node, src) {
                    out.push(entry);
                }
            }
            // `for (x: Type in ...)` / `catch (e: Type)`.
            "for_statement" | "catch_block" => {
                if let Some(declaration) = named_child(node, "variable_declaration") {
                    if let (Some(name), Some(ty)) = (
                        named_child(declaration, "identifier").map(|name| text_of(name, src)),
                        type_name(declaration, src),
                    ) {
                        out.push((name, ty));
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

/// Simple name of a type node: `Repo` for `com.example.Repo`, `List` for
/// `List<Account>`, and the inner type for `Repo?` / `(Repo)`.
fn type_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "user_type" => {
            let mut cursor = node.walk();
            let mut last = None;
            for child in node.named_children(&mut cursor) {
                if child.kind() == "identifier" {
                    last = Some(text_of(child, src));
                }
            }
            last.filter(|name| !name.is_empty())
        }
        "nullable_type" | "parenthesized_type" | "non_nullable_type" => {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find_map(|child| type_name(child, src));
            found
        }
        // `variable_declaration`/`class_parameter`/`parameter`: find the first
        // child that is a type node, skipping the name and modifiers.
        _ => {
            let mut cursor = node.walk();
            let ty = node.named_children(&mut cursor).find(|child| {
                matches!(
                    child.kind(),
                    "user_type" | "nullable_type" | "parenthesized_type" | "non_nullable_type"
                )
            });
            ty.and_then(|child| type_name(child, src))
        }
    }
}

fn supertypes(class: Node<'_>, src: &str) -> Vec<String> {
    let Some(specifiers) = named_child(class, "delegation_specifiers") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = specifiers.walk();
    for specifier in specifiers.named_children(&mut cursor) {
        if specifier.kind() != "delegation_specifier" {
            continue;
        }
        // `Base()` / `Base` / `Base by delegate`.
        if let Some(name) = first_type_name(specifier, src) {
            out.push(name);
        }
    }
    out
}

/// Every call expression in the member, with its receiver shape.
/// First type-name found anywhere below `node` (through `constructor_invocation`
/// and `explicit_delegation` wrappers).
fn first_type_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "user_type" | "nullable_type" | "parenthesized_type" | "non_nullable_type" => {
            return type_name(node, src)
        }
        "identifier" => return None,
        _ => {}
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find_map(|child| first_type_name(child, src));
    found
}

fn calls_in(member: Node<'_>, src: &str) -> Vec<Call> {
    let mut out = Vec::new();
    let mut stack = vec![member];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" {
            let mut cursor = node.walk();
            let callee = node.named_children(&mut cursor).next();
            if let Some(callee) = callee {
                if let Some(call) = call_of(callee, src, line_of(src, node.start_byte())) {
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

fn call_of(callee: Node<'_>, src: &str, line: usize) -> Option<Call> {
    match callee.kind() {
        "identifier" => {
            let name = text_of(callee, src);
            (!name.is_empty()).then_some(Call {
                name,
                receiver: Receiver::Bare,
                line,
            })
        }
        "navigation_expression" => {
            let mut cursor = callee.walk();
            let named: Vec<Node> = callee.named_children(&mut cursor).collect();
            let name = named.last().map(|name| text_of(*name, src))?;
            let receiver = named
                .first()
                .map(|base| receiver_of(*base, src))
                .unwrap_or(Receiver::Unknown);
            (!name.is_empty()).then_some(Call {
                name,
                receiver,
                line,
            })
        }
        // `Type(...)` is a constructor, not a method.
        _ => None,
    }
}

fn receiver_of(node: Node<'_>, src: &str) -> Receiver {
    match node.kind() {
        "this_expression" => Receiver::This,
        "super_expression" => Receiver::Super,
        "identifier" => Receiver::Name(text_of(node, src)),
        _ => Receiver::Unknown,
    }
}

/// Kotlin's `user_type` holds simple type names (including generic arguments);
/// `navigation_expression` receivers catch object references like
/// `DemoDocument.create()`.
fn collect_type_uses(root: Node<'_>, src: &str, out: &mut Vec<(String, usize)>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "user_type" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "identifier" {
                        push_simple_name(out, child, src);
                    }
                }
            }
            "navigation_expression" => {
                let mut cursor = node.walk();
                let first = node.named_children(&mut cursor).next();
                if let Some(first) = first {
                    if first.kind() == "identifier" {
                        push_simple_name(out, first, src);
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

/// Uses the last segment of a (possibly qualified) name.
fn push_simple_name(out: &mut Vec<(String, usize)>, node: Node<'_>, src: &str) {
    let text = text_of(node, src);
    if let Some(name) = text.rsplit('.').next() {
        if !name.is_empty() {
            out.push((name.to_string(), node.start_byte()));
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
        KotlinParser::new().parse(src).expect("parsed")
    }

    #[test]
    fn package_types_and_imports() {
        let source = parse(
            r#"
            package com.example.app

            import com.example.model.User
            import com.example.model.*
            import com.example.model.Account as Acct
            import com.example.util.Helpers.parse

            class Service(val user: User) : Base(), Runnable
            interface Base
            object DemoDocument
            enum class Color { RED }
            typealias Alias = User
            "#,
        );
        assert_eq!(source.package.as_deref(), Some("com.example.app"));
        assert_eq!(
            source.types,
            vec!["Service", "Base", "DemoDocument", "Color", "Alias"]
        );
        assert_eq!(
            source
                .imports
                .iter()
                .map(|import| (import.path.as_str(), import.kind))
                .collect::<Vec<_>>(),
            vec![
                ("com.example.model.User", ImportKind::Type),
                ("com.example.model", ImportKind::Wildcard),
                ("com.example.model.Account", ImportKind::Type),
                ("com.example.util.Helpers.parse", ImportKind::Type),
            ]
        );
    }

    #[test]
    fn collects_type_uses_and_object_receivers() {
        let source = parse(
            r#"
            package app
            class Service {
                val doc = DemoDocument.create()
                fun run(user: User): List<Account> = TODO()
            }
            "#,
        );
        for expected in ["DemoDocument", "User", "List", "Account"] {
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

            class Service(val repo: Repo, val count: Int) : Base(), Api {
                var helper: Helper = Helper()

                fun used(local: Local, h: Helper): Int {
                    val temp: Local = Local()
                    temp.ping()
                    h.pong()
                    repo.find()
                    Helper.staticCall()
                    this.inside()
                    super.parent()
                    return count
                }

                fun omitted(): Int = 1
                fun inside() {}
                abstract fun nothing()
            }
            "#,
        );
        let service = &source.symbols[0];
        assert_eq!(service.name, "Service");
        assert_eq!(service.supertypes, vec!["Base", "Api"]);
        assert_eq!(
            service.fields,
            vec![
                ("repo".to_string(), "Repo".to_string()),
                ("count".to_string(), "Int".to_string()),
                ("helper".to_string(), "Helper".to_string()),
            ]
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

        // An expression body stops before the `=`, so the stub is a signature.
        let omitted = service
            .members
            .iter()
            .find(|member| member.name == "omitted")
            .expect("omitted");
        assert!(omitted.body_start.is_some());
        // An abstract method has no body to elide.
        let nothing = service
            .members
            .iter()
            .find(|member| member.name == "nothing")
            .expect("nothing");
        assert_eq!(nothing.body_start, None);
    }

    #[test]
    fn script_without_package() {
        let source = parse("val x = 1\nfun main() = println(x)\n");
        assert_eq!(source.package, None);
        assert!(source.types.is_empty());
    }
}
