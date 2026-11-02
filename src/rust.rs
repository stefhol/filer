//! Rust parsing via tree-sitter: `use` / `mod` declarations, top-level items
//! and `impl` blocks. Methods are attached to the type they implement so the
//! resolver and renderer can prune them like Java/Kotlin methods.

use tree_sitter::{Node, Parser};

use crate::lang::{
    end_line_of, finish_symbols, line_of, symbol_from_node, Call, Import, ImportKind, Member,
    MemberKind, Receiver, Source, Symbol, SymbolKind,
};

const COMMENTS: &[&str] = &["line_comment", "block_comment"];

pub struct RustParser {
    parser: Parser,
}

impl RustParser {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("tree-sitter-rust grammar failed to load");
        Self { parser }
    }

    pub fn parse(&mut self, src: &str) -> Option<Source> {
        let tree = self.parser.parse(src, None)?;
        let root = tree.root_node();
        let mut out = Source::default();

        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            match child.kind() {
                "use_declaration" => {
                    if let Some(argument) = child.child_by_field_name("argument") {
                        let mut uses = Vec::new();
                        flatten_uses(argument, "", src, &mut uses);
                        for (path, kind) in uses {
                            if !path.is_empty() {
                                out.imports.push(Import { path, kind });
                            }
                        }
                    }
                }
                // `mod foo;` pulls in `foo.rs` or `foo/mod.rs`. Inline modules
                // (`mod foo { ... }`) are part of the current file.
                "mod_item" => {
                    if child.child_by_field_name("body").is_none() {
                        if let Some(name) = child.child_by_field_name("name") {
                            let name = text_of(name, src);
                            if !name.is_empty() {
                                out.imports.push(Import {
                                    path: format!("self.{name}"),
                                    kind: ImportKind::Type,
                                });
                            }
                        }
                    }
                }
                "function_item" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let name = text_of(name, src);
                        if !name.is_empty() {
                            let mut symbol =
                                symbol_from_node(child, &name, SymbolKind::Function, COMMENTS, src);
                            let start = declaration_start(child);
                            symbol.start = start;
                            symbol.line = line_of(src, start);
                            symbol.end_line = end_line_of(src, start, symbol.end);
                            symbol.calls = calls_in(child, src);
                            push_symbol(&mut out, symbol);
                        }
                    }
                }
                "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let name = text_of(name, src);
                        if !name.is_empty() {
                            let start = declaration_start(child);
                            let mut symbol = Symbol {
                                name: name.clone(),
                                kind: SymbolKind::Type,
                                start,
                                end: child.end_byte(),
                                line: line_of(src, start),
                                end_line: end_line_of(src, start, child.end_byte()),
                                uses: Vec::new(),
                                calls: Vec::new(),
                                members: Vec::new(),
                                fields: Vec::new(),
                                supertypes: Vec::new(),
                                params: Vec::new(),
                                locals: Vec::new(),
                            };
                            if child.kind() == "struct_item" || child.kind() == "union_item" {
                                symbol.fields = fields_of(child, src);
                            }
                            if child.kind() == "trait_item" {
                                if let Some(body) = child.child_by_field_name("body") {
                                    collect_methods(body, src, &mut symbol.members);
                                }
                            }
                            out.symbols.push(symbol);
                            if !out.types.contains(&name) {
                                out.types.push(name);
                            }
                        }
                    }
                }
                "const_item" | "static_item" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let name = text_of(name, src);
                        if !name.is_empty() {
                            let mut symbol = symbol_from_node(
                                child,
                                &name,
                                SymbolKind::Declaration,
                                COMMENTS,
                                src,
                            );
                            let start = declaration_start(child);
                            symbol.start = start;
                            symbol.line = line_of(src, start);
                            symbol.end_line = end_line_of(src, start, symbol.end);
                            out.symbols.push(symbol);
                        }
                    }
                }
                "impl_item" => {
                    if let Some(type_node) = child.child_by_field_name("type") {
                        if let Some(name) = simple_type_name(type_node, src) {
                            let start = declaration_start(child);
                            let symbol =
                                merge_symbol(&mut out, &name, start, child.end_byte(), src);
                            if let Some(trait_node) = child.child_by_field_name("trait") {
                                if let Some(trait_name) = simple_type_name(trait_node, src) {
                                    if !symbol.supertypes.contains(&trait_name) {
                                        symbol.supertypes.push(trait_name);
                                    }
                                }
                            }
                            if let Some(body) = child.child_by_field_name("body") {
                                collect_methods(body, src, &mut symbol.members);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        out.symbols.sort_by_key(|symbol| symbol.start);
        let mut uses = Vec::new();
        collect_type_uses(root, src, &mut uses);
        finish_symbols(&mut out, &uses);

        Some(out)
    }
}

impl Default for RustParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Start of a declaration including its doc comments and attributes
/// (`#[derive(...)]`), which tree-sitter keeps as preceding siblings.
fn declaration_start(node: Node<'_>) -> usize {
    let mut start = node.start_byte();
    let mut current = node;
    while let Some(previous) = current.prev_named_sibling() {
        if !COMMENTS.contains(&previous.kind()) && previous.kind() != "attribute_item" {
            break;
        }
        start = previous.start_byte();
        current = previous;
    }
    start
}

fn push_symbol(out: &mut Source, symbol: Symbol) {
    out.symbols.push(symbol);
}

/// Finds the symbol for `name` in the file, or creates one for an `impl` block
/// whose type is declared in another file. Keeps one symbol per type so
/// `path::Type` can carry methods from several impl blocks.
fn merge_symbol<'a>(
    out: &'a mut Source,
    name: &str,
    start: usize,
    end: usize,
    src: &str,
) -> &'a mut Symbol {
    if let Some(index) = out.symbols.iter().position(|symbol| symbol.name == name) {
        let symbol = &mut out.symbols[index];
        if start < symbol.start {
            symbol.start = start;
            symbol.line = line_of(src, start);
        }
        symbol.end = symbol.end.max(end);
        symbol.end_line = end_line_of(src, symbol.start, symbol.end);
        return symbol;
    }
    out.symbols.push(Symbol {
        name: name.to_string(),
        kind: SymbolKind::Type,
        start,
        end,
        line: line_of(src, start),
        end_line: end_line_of(src, start, end),
        uses: Vec::new(),
        calls: Vec::new(),
        members: Vec::new(),
        fields: Vec::new(),
        supertypes: Vec::new(),
        params: Vec::new(),
        locals: Vec::new(),
    });
    out.symbols.last_mut().expect("just pushed")
}

fn flatten_uses(node: Node<'_>, prefix: &str, src: &str, out: &mut Vec<(String, ImportKind)>) {
    match node.kind() {
        "scoped_use_list" => {
            let base = node
                .child_by_field_name("path")
                .map(|path| join_path(prefix, &text_of(path, src)))
                .unwrap_or_else(|| prefix.to_string());
            if let Some(list) = node.child_by_field_name("list") {
                flatten_uses(list, &base, src, out);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                flatten_uses(child, prefix, src, out);
            }
        }
        // `use a::B as C`: the target is `B`; aliases are left to name lookup.
        "use_as_clause" => {
            if let Some(path) = node.child_by_field_name("path") {
                flatten_uses(path, prefix, src, out);
            }
        }
        "use_wildcard" => {
            let mut cursor = node.walk();
            let base = node
                .named_children(&mut cursor)
                .next()
                .map(|path| join_path(prefix, &text_of(path, src)))
                .unwrap_or_else(|| prefix.to_string());
            out.push((base, ImportKind::Wildcard));
        }
        _ => out.push((join_path(prefix, &text_of(node, src)), ImportKind::Type)),
    }
}

/// Normalises Rust's `::` paths to the dotted form used by the shared Import
/// model (`crate::app::Service` -> `crate.app.Service`).
fn join_path(prefix: &str, path: &str) -> String {
    let path = path.trim().trim_end_matches('*').trim_end_matches(':');
    let path = path.replace("::", ".");
    let path = path.trim_matches('.').to_string();
    match (prefix.trim_matches('.'), path.as_str()) {
        ("", _) => path,
        (prefix, "") => prefix.to_string(),
        (prefix, path) => format!("{prefix}.{path}"),
    }
}

fn collect_methods(body: Node<'_>, src: &str, out: &mut Vec<Member>) {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        match child.kind() {
            "function_item" | "function_signature_item" => {
                if let Some(member) = method_of(child, src) {
                    out.push(member);
                }
            }
            _ => {}
        }
    }
}

fn method_of(node: Node<'_>, src: &str) -> Option<Member> {
    let name = node.child_by_field_name("name")?;
    let name = text_of(name, src);
    if name.is_empty() {
        return None;
    }
    let start = declaration_start(node);
    let body_start = node
        .child_by_field_name("body")
        .map(|body| body.start_byte());
    Some(Member {
        name,
        kind: MemberKind::Method,
        start,
        end: node.end_byte(),
        line: line_of(src, start),
        end_line: end_line_of(src, start, node.end_byte()),
        body_start,
        params: parameters_of(node, src),
        locals: locals_of(node, src),
        calls: calls_in(node, src),
    })
}

fn parameters_of(node: Node<'_>, src: &str) -> Vec<(String, String)> {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() != "parameter" {
            continue;
        }
        let name = parameter
            .child_by_field_name("pattern")
            .and_then(|pattern| pattern_name(pattern, src));
        let ty = parameter
            .child_by_field_name("type")
            .and_then(|ty| simple_type_name(ty, src));
        if let (Some(name), Some(ty)) = (name, ty) {
            out.push((name, ty));
        }
    }
    out
}

fn locals_of(node: Node<'_>, src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "let_declaration" {
            if let Some(pattern) = current.child_by_field_name("pattern") {
                if let Some(name) = pattern_name(pattern, src) {
                    let ty = current
                        .child_by_field_name("type")
                        .and_then(|ty| simple_type_name(ty, src))
                        .or_else(|| {
                            current
                                .child_by_field_name("value")
                                .and_then(|value| inferred_type(value, src))
                        });
                    if let Some(ty) = ty {
                        out.push((name, ty));
                    }
                }
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

fn pattern_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "identifier" => {
            let name = text_of(node, src);
            is_identifier(&name).then_some(name)
        }
        "mut_pattern" | "reference_pattern" | "ref_pattern" => {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find_map(|child| pattern_name(child, src));
            found
        }
        _ => None,
    }
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
        && !name.starts_with(|character: char| character.is_ascii_digit())
}

/// Best-effort type of a `let` initializer for receiver typing.
fn inferred_type(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "struct_expression" => node
            .child_by_field_name("name")
            .and_then(|name| simple_type_name(name, src)),
        "call_expression" => {
            let function = node.child_by_field_name("function")?;
            match function.kind() {
                // `Foo::new()` -> `Foo`.
                "scoped_identifier" => function
                    .child_by_field_name("path")
                    .and_then(|path| simple_type_name(path, src)),
                // `Foo { .. }` wrapped in a call is not a thing.
                _ => None,
            }
        }
        "reference_expression" | "unary_expression" | "parenthesized_expression" => {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find_map(|child| inferred_type(child, src));
            found
        }
        _ => None,
    }
}

fn calls_in(node: Node<'_>, src: &str) -> Vec<Call> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "call_expression" {
            if let Some(function) = current.child_by_field_name("function") {
                if let Some(call) = call_of(function, src, line_of(src, current.start_byte())) {
                    out.push(call);
                }
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
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
        "field_expression" => {
            let name = function.child_by_field_name("field")?;
            let name = text_of(name, src);
            let receiver = function
                .child_by_field_name("value")
                .map(|value| receiver_of(value, src))
                .unwrap_or(Receiver::Unknown);
            (!name.is_empty()).then_some(Call {
                name,
                receiver,
                line,
            })
        }
        "scoped_identifier" => {
            let name = function.child_by_field_name("name")?;
            let name = text_of(name, src);
            let receiver = function
                .child_by_field_name("path")
                .map(|path| receiver_of_path(path, src))
                .unwrap_or(Receiver::Bare);
            (!name.is_empty()).then_some(Call {
                name,
                receiver,
                line,
            })
        }
        "generic_function" => function
            .child_by_field_name("function")
            .and_then(|inner| call_of(inner, src, line)),
        _ => None,
    }
}

fn receiver_of(node: Node<'_>, src: &str) -> Receiver {
    match node.kind() {
        "self" => Receiver::This,
        "identifier" => Receiver::Name(text_of(node, src)),
        "scoped_identifier" => receiver_of_path(node, src),
        "field_expression" => {
            // `self.repo.find()` -> receiver `repo`; nested `self.a.b` stays
            // unknown because we do not infer field chains.
            let Some(value) = node.child_by_field_name("value") else {
                return Receiver::Unknown;
            };
            if value.kind() == "self" {
                node.child_by_field_name("field")
                    .map(|field| Receiver::Name(text_of(field, src)))
                    .unwrap_or(Receiver::Unknown)
            } else {
                Receiver::Unknown
            }
        }
        "reference_expression" | "parenthesized_expression" => {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .next()
                .map(|inner| receiver_of(inner, src))
                .unwrap_or(Receiver::Unknown);
            found
        }
        _ => Receiver::Unknown,
    }
}

fn receiver_of_path(node: Node<'_>, src: &str) -> Receiver {
    let text = text_of(node, src);
    let text = text.trim();
    if text == "Self" {
        return Receiver::This;
    }
    if matches!(text, "self" | "super" | "crate")
        || text.starts_with("self::")
        || text.starts_with("super::")
        || text.starts_with("crate::")
    {
        // A module-level free function: let the by-name fallback resolve it.
        return Receiver::Bare;
    }
    let leaf = text
        .rsplit("::")
        .next()
        .and_then(|name| name.split('<').next())
        .unwrap_or(text);
    if is_identifier(leaf) {
        Receiver::Name(leaf.to_string())
    } else {
        Receiver::Unknown
    }
}

fn fields_of(node: Node<'_>, src: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "field_declaration" {
            let name = current
                .child_by_field_name("name")
                .map(|name| text_of(name, src));
            let ty = current
                .child_by_field_name("type")
                .and_then(|ty| simple_type_name(ty, src));
            if let (Some(name), Some(ty)) = (name, ty) {
                found.push((current.start_byte(), name, ty));
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    found.sort_by_key(|(start, _, _)| *start);
    found.into_iter().map(|(_, name, ty)| (name, ty)).collect()
}

/// Simple name of a type: `Foo` for `crate::a::Foo`, `Foo<T>`, `&Foo` and
/// `Box<Foo>` (only the outer type matters for receiver resolution).
fn simple_type_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" => {
            let name = text_of(node, src);
            (!name.is_empty()).then_some(name)
        }
        "scoped_type_identifier" => node
            .child_by_field_name("name")
            .map(|name| text_of(name, src))
            .filter(|name| !name.is_empty()),
        "generic_type" | "reference_type" | "pointer_type" | "array_type" | "bracketed_type"
        | "tuple_type" => node
            .child_by_field_name("type")
            .and_then(|inner| simple_type_name(inner, src))
            .or_else(|| {
                let mut cursor = node.walk();
                let found = node
                    .named_children(&mut cursor)
                    .find_map(|child| simple_type_name(child, src));
                found
            }),
        "scoped_identifier" => {
            let text = text_of(node, src);
            let leaf = text.rsplit("::").next().unwrap_or(&text);
            (!leaf.is_empty()).then(|| leaf.to_string())
        }
        _ => None,
    }
}

/// Collects type names from the whole file so import-free references to a
/// sibling type still pull in the declaring module.
fn collect_type_uses(root: Node<'_>, src: &str, out: &mut Vec<(String, usize)>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "type_identifier" => {
                let name = text_of(node, src);
                if !name.is_empty() {
                    out.push((name, node.start_byte()));
                }
            }
            "scoped_identifier" => {
                // `DemoDocument::create()` and enum paths: the first segment
                // can be a type or module; only record the likely type.
                if let Some(first) = first_path_segment(node, src) {
                    if first
                        .chars()
                        .next()
                        .is_some_and(|character| character.is_ascii_uppercase())
                    {
                        out.push((first, node.start_byte()));
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

fn first_path_segment(node: Node<'_>, src: &str) -> Option<String> {
    let mut current = node;
    loop {
        match current.kind() {
            "identifier" | "type_identifier" => {
                return Some(text_of(current, src));
            }
            "scoped_identifier" | "scoped_type_identifier" => {
                current = current.child_by_field_name("path")?;
            }
            _ => return None,
        }
    }
}

fn text_of(node: Node<'_>, src: &str) -> String {
    src.get(node.byte_range()).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Source {
        RustParser::new().parse(src).expect("parsed")
    }

    #[test]
    fn parses_use_and_mod_declarations() {
        let source = parse(
            r#"
            use crate::app::{Service, model::User};
            use crate::app::helpers::*;
            use super::base::Base as Renamed;
            use std::collections::HashMap;

            mod local;
            mod inline {
                pub struct Hidden;
            }
            "#,
        );
        assert_eq!(
            source
                .imports
                .iter()
                .map(|import| (import.path.as_str(), import.kind))
                .collect::<Vec<_>>(),
            vec![
                ("crate.app.Service", ImportKind::Type),
                ("crate.app.model.User", ImportKind::Type),
                ("crate.app.helpers", ImportKind::Wildcard),
                ("super.base.Base", ImportKind::Type),
                ("std.collections.HashMap", ImportKind::Type),
                ("self.local", ImportKind::Type),
            ]
        );
    }

    #[test]
    fn collects_types_functions_and_impl_methods() {
        let source = parse(
            r#"
            #[derive(Debug)]
            pub struct Service {
                repo: Repo,
                helper: &'static Helper,
            }

            pub enum State { Ready, Busy }

            pub trait Api {
                fn ping(&self);
                fn pong(&self) {}
            }

            pub fn free(value: User) -> Result<Service, Error> {
                build(value)
            }

            impl Api for Service {
                fn ping(&self) {}
            }

            impl Service {
                fn new(repo: Repo) -> Self {
                    let local: Local = Local::new();
                    Self { repo, helper: &Helper }
                }

                fn used(&self, helper: Helper) -> i32 {
                    let local = Local::new();
                    local.ping();
                    helper.pong();
                    self.repo.find();
                    Service::static_call();
                    self.inside();
                    free(helper);
                    1
                }

                fn inside(&self) {}
                fn omitted(&self) -> i32 { 1 }
            }
            "#,
        );
        assert_eq!(
            source.types,
            vec!["Service", "State", "Api"],
            "inline impl does not declare the type twice"
        );
        for expected in ["Service", "State", "Api", "free"] {
            assert!(
                source.symbols.iter().any(|symbol| symbol.name == expected),
                "missing {expected}: {:?}",
                source.symbols
            );
        }

        let service = source
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Service")
            .expect("Service");
        assert_eq!(
            service.fields,
            vec![
                ("repo".to_string(), "Repo".to_string()),
                ("helper".to_string(), "Helper".to_string()),
            ]
        );
        assert_eq!(service.supertypes, vec!["Api"]);
        let used = service
            .members
            .iter()
            .find(|member| member.name == "used")
            .expect("used");
        assert_eq!(
            used.params,
            vec![("helper".to_string(), "Helper".to_string())]
        );
        assert_eq!(
            used.locals,
            vec![("local".to_string(), "Local".to_string())]
        );
        for (name, receiver) in [
            ("ping", Receiver::Name("local".to_string())),
            ("pong", Receiver::Name("helper".to_string())),
            ("find", Receiver::Name("repo".to_string())),
            ("static_call", Receiver::Name("Service".to_string())),
            ("inside", Receiver::This),
            ("free", Receiver::Bare),
        ] {
            assert!(
                used.calls
                    .iter()
                    .any(|call| call.name == name && call.receiver == receiver),
                "missing {name}: {:?}",
                used.calls
            );
        }
        assert!(used.body_start.is_some());

        let ping = service
            .members
            .iter()
            .find(|member| member.name == "ping")
            .expect("trait impl method");
        assert!(ping.body_start.is_some());
        assert!(!service
            .members
            .iter()
            .any(|member| member.name == "omitted" && member.body_start.is_none()));
    }

    #[test]
    fn declaration_spans_include_attributes() {
        let text = "#[derive(Debug)]\npub struct Service;\n";
        let source = parse(text);
        let service = &source.symbols[0];
        assert_eq!(service.start, 0);
        assert_eq!(&text[service.start..service.end], text.trim_end());

        let text = "#[inline]\nfn run() {}\n";
        let source = parse(text);
        assert_eq!(source.symbols[0].start, 0);
    }

    #[test]
    fn trait_signature_has_no_body() {
        let source = parse("pub trait Api { fn ping(&self); }");
        let api = &source.symbols[0];
        let ping = api
            .members
            .iter()
            .find(|member| member.name == "ping")
            .expect("ping");
        assert_eq!(ping.body_start, None);
    }

    #[test]
    fn impl_only_file_gets_a_symbol_for_its_type() {
        let source = parse("impl Service { fn run(&self) {} }");
        assert_eq!(source.symbols[0].name, "Service");
        assert!(source.types.is_empty());
        assert_eq!(source.symbols[0].members[0].name, "run");
    }

    #[test]
    fn collects_type_uses() {
        let source = parse(
            r#"
            struct Service { repo: Repo }
            fn run(value: User) -> Result<Service, Error> {
                DemoDocument::create();
                helper();
            }
            "#,
        );
        for expected in ["Service", "Repo", "User", "Result", "Error", "DemoDocument"] {
            assert!(
                source.uses.iter().any(|name| name == expected),
                "missing {expected} in {:?}",
                source.uses
            );
        }
    }
}
