//! Scala parsing via tree-sitter: (possibly nested) package clauses, top-level
//! type declarations and Scala 2/3 imports.

use tree_sitter::{Node, Parser};

use crate::lang::{
    end_line_of, finish_symbols, leading_comment_start, line_of, symbol_from_node, Call, Import,
    ImportKind, Member, MemberKind, Receiver, Source, Symbol, SymbolKind,
};

const COMMENTS: &[&str] = &["comment"];

pub struct ScalaParser {
    parser: Parser,
}

/// Declarations that introduce a name importable from a package.
const TYPE_DECLS: &[&str] = &[
    "class_definition",
    "object_definition",
    "trait_definition",
    "enum_definition",
    "type_definition",
];

impl ScalaParser {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_scala::LANGUAGE.into())
            .expect("tree-sitter-scala grammar failed to load");
        Self { parser }
    }

    /// Parses a Scala source file (`.scala`, `.sbt` or `.sc`).
    pub fn parse(&mut self, src: &str) -> Option<Source> {
        let tree = self.parser.parse(src, None)?;
        let root = tree.root_node();

        let mut out = Source::default();
        let mut state = WalkState::default();
        walk_scope(root, src, &mut Vec::new(), &mut out, &mut state);

        out.package = state
            .type_package
            .filter(|package| !package.is_empty())
            .or(state.package_object)
            .or(state.first_package);

        collect_imports(root, src, &mut out.imports);

        let mut uses = Vec::new();
        collect_type_uses(root, src, &mut uses);
        finish_symbols(&mut out, &uses);

        Some(out)
    }
}

impl Default for ScalaParser {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct WalkState {
    /// Package of the first top-level type declaration.
    type_package: Option<String>,
    /// Package of the first `package` clause, used when the file declares no
    /// types of its own (package objects, imports-only files).
    first_package: Option<String>,
    /// `package object foo` composes with the enclosing package.
    package_object: Option<String>,
}

/// Walks the compilation unit and the bodies of `package` clauses, but not
/// class/object bodies: only top-level declarations are indexed.
fn walk_scope(
    node: Node<'_>,
    src: &str,
    prefix: &mut Vec<String>,
    out: &mut Source,
    state: &mut WalkState,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "package_clause" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|name| text_of(name, src))
                    .unwrap_or_default();
                if state.first_package.is_none() {
                    state.first_package = Some(name.clone());
                }

                let depth = prefix.len();
                push_package(prefix, &name);

                // `package a.b { ... }` closes again afterwards, while a bare
                // `package a.b` accumulates for the following clauses.
                if let Some(body) = child.child_by_field_name("body") {
                    walk_scope(body, src, prefix, out, state);
                    prefix.truncate(depth);
                }
            }
            "package_object" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|name| text_of(name, src))
                    .unwrap_or_default();
                if state.package_object.is_none() {
                    let mut path = prefix.clone();
                    path.push(name);
                    state.package_object = Some(path.join("."));
                }
                // The object's members are package members, not top-level
                // types; only its imports/uses matter and those are collected
                // globally.
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
                if state.type_package.is_none() {
                    state.type_package = Some(prefix.join("."));
                }
            }
            _ => {}
        }
    }
}

/// Methods, constructors, fields and supertypes of one class/object/trait,
/// with the calls each member makes and the receiver types the resolver needs.
fn collect_members(class: Node<'_>, src: &str, symbol: &mut Symbol) {
    symbol.supertypes = supertypes(class, src);

    // `class Service(val repo: Repo)` makes `repo` a field.
    if let Some(parameters) = class.child_by_field_name("class_parameters") {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if parameter.kind() != "class_parameter" {
                continue;
            }
            if !has_modifier_token(parameter, &["val", "var"]) {
                continue;
            }
            if let (Some(name), Some(ty)) = (
                parameter.child_by_field_name("name"),
                parameter
                    .child_by_field_name("type")
                    .and_then(|ty| type_name(ty, src)),
            ) {
                symbol.fields.push((text_of(name, src), ty));
            }
        }
    }

    let Some(body) = class.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        match member.kind() {
            "function_definition" | "function_declaration" => {
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
                    .map(|body| body_marker(member).unwrap_or_else(|| body.start_byte()));
                let kind = if name == "this" {
                    MemberKind::Constructor
                } else {
                    MemberKind::Method
                };
                symbol.members.push(Member {
                    name,
                    kind,
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
            "val_definition" | "var_definition" => {
                if let Some((name, ty)) = defined_name_and_type(member, src) {
                    symbol.fields.push((name, ty));
                }
            }
            _ => {}
        }
    }
}

fn has_modifier_token(node: Node<'_>, tokens: &[&str]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if tokens.contains(&child.kind()) {
            return true;
        }
    }
    false
}

/// Start of the body expression: the `=` of `def f = ...`/`val x = ...`, or the
/// `{` of a block body, so the rendered signature stops before it.
fn body_marker(node: Node<'_>) -> Option<usize> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "=" {
            return Some(child.start_byte());
        }
    }
    None
}

/// Name and declared type of `val x: Type` / `var x: Type`.
fn defined_name_and_type(node: Node<'_>, src: &str) -> Option<(String, String)> {
    let pattern = node.child_by_field_name("pattern")?;
    let name = match pattern.kind() {
        "identifier" | "operator_identifier" => text_of(pattern, src),
        "identifiers" => {
            let mut cursor = pattern.walk();
            let name = pattern
                .named_children(&mut cursor)
                .find(|child| matches!(child.kind(), "identifier" | "operator_identifier"))
                .map(|child| text_of(child, src))?;
            name
        }
        _ => return None,
    };
    let ty = node
        .child_by_field_name("type")
        .and_then(|ty| type_name(ty, src))?;
    (!name.is_empty()).then_some((name, ty))
}

fn parameter_names(parameters: Option<Node<'_>>, src: &str) -> Vec<(String, String)> {
    let Some(parameters) = parameters else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if !matches!(parameter.kind(), "parameter" | "class_parameter") {
            continue;
        }
        if let (Some(name), Some(ty)) = (
            parameter.child_by_field_name("name"),
            parameter
                .child_by_field_name("type")
                .and_then(|ty| type_name(ty, src)),
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
            // `val x: Type = ...` / `var x: Type = ...` inside the body.
            "val_definition" | "var_definition" => {
                if let Some(entry) = defined_name_and_type(node, src) {
                    out.push(entry);
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

/// Simple name of a type: `Repo`, `List` for `List[Repo]`, `Repo` for
/// `com.example.Repo` and `Repo` for `Repo @annot`.
fn type_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "operator_identifier" => {
            let text = text_of(node, src);
            (!text.is_empty()).then_some(text)
        }
        "generic_type" | "projected_type" => node
            .child_by_field_name("type")
            .and_then(|ty| type_name(ty, src)),
        "stable_type_identifier" | "singleton_type" => {
            let mut cursor = node.walk();
            let mut last = None;
            for child in node.named_children(&mut cursor) {
                if matches!(
                    child.kind(),
                    "identifier" | "type_identifier" | "operator_identifier"
                ) {
                    last = Some(text_of(child, src));
                }
            }
            last.or_else(|| {
                node.named_children(&mut cursor)
                    .find_map(|child| type_name(child, src))
            })
        }
        // `annotated_type` has no `type` field: its first type-ish child is the
        // type, the rest are annotations.
        _ => {
            let mut cursor = node.walk();
            let found = node.named_children(&mut cursor).find(|child| {
                matches!(
                    child.kind(),
                    "type_identifier"
                        | "generic_type"
                        | "stable_type_identifier"
                        | "projected_type"
                        | "singleton_type"
                        | "annotated_type"
                        | "applied_constructor_type"
                )
            });
            found.and_then(|child| type_name(child, src))
        }
    }
}

fn supertypes(class: Node<'_>, src: &str) -> Vec<String> {
    let Some(extends) = class.child_by_field_name("extend") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = extends.walk();
    for ty in extends.children_by_field_name("type", &mut cursor) {
        if let Some(name) = type_name(ty, src) {
            out.push(name);
        }
    }
    if out.is_empty() {
        let mut cursor = extends.walk();
        for child in extends.named_children(&mut cursor) {
            if let Some(name) = type_name(child, src) {
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
        if node.kind() == "call_expression" {
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
        "identifier" | "operator_identifier" => {
            let name = text_of(function, src);
            (!name.is_empty()).then_some(Call {
                name,
                receiver: Receiver::Bare,
                line,
            })
        }
        // `foo[Int](...)`.
        "generic_function" => function
            .child_by_field_name("function")
            .and_then(|inner| call_of(inner, src, line)),
        "field_expression" => {
            let name = function
                .child_by_field_name("field")
                .map(|field| text_of(field, src))?;
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
        _ => None,
    }
}

fn receiver_of(node: Node<'_>, src: &str) -> Receiver {
    match node.kind() {
        "this" => Receiver::This,
        "super" => Receiver::Super,
        "identifier" | "operator_identifier" => {
            let text = text_of(node, src);
            match text.as_str() {
                "this" => Receiver::This,
                "super" => Receiver::Super,
                _ => Receiver::Name(text),
            }
        }
        "field_expression" => node
            .child_by_field_name("field")
            .map(|field| Receiver::Name(text_of(field, src)))
            .unwrap_or(Receiver::Unknown),
        "instance_expression" => {
            let mut cursor = node.walk();
            let ty = node.named_children(&mut cursor).find(|child| {
                matches!(
                    child.kind(),
                    "type_identifier"
                        | "generic_type"
                        | "stable_type_identifier"
                        | "annotated_type"
                        | "applied_constructor_type"
                )
            });
            ty.and_then(|ty| type_name(ty, src))
                .map(Receiver::Name)
                .unwrap_or(Receiver::Unknown)
        }
        _ => Receiver::Unknown,
    }
}

fn push_package(prefix: &mut Vec<String>, name: &str) {
    prefix.extend(
        name.split('.')
            .filter(|segment| !segment.is_empty())
            .map(str::to_string),
    );
}

/// Collects `import` and `export` declarations anywhere in the file: Scala
/// allows them inside blocks, and they are always absolute paths.
fn collect_imports(node: Node<'_>, src: &str, out: &mut Vec<Import>) {
    if matches!(node.kind(), "import_declaration" | "export_declaration") {
        out.extend(parse_import(node, src));
        // No imports nested inside an import declaration.
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_imports(child, src, out);
    }
}

/// Normalizes one Scala import declaration into ordinary dotted imports.
///
/// - `import a.b.C`           -> `a.b.C`
/// - `import a.b._` / `.*`    -> wildcard `a.b`
/// - `import a.b.{C, D}`      -> `a.b.C`, `a.b.D`
/// - `import a.b.C.{x, y}`    -> `a.b.C.x`, `a.b.C.y` (resolution trims to `C`)
/// - `import a.b.C.{x => _}`  -> nothing (exclusion)
/// - `import a.b.C.given`     -> wildcard `a.b.C` (the object/file that declares it)
/// - `import a.b.C as U`      -> `a.b.C`
/// - `import a.b.C, d.e.F`    -> both
fn parse_import(node: Node<'_>, src: &str) -> Vec<Import> {
    // Commas inside selector braces are nested in `namespace_selectors`, so a
    // direct comma separates import expressions (`import a.B, c.D`).
    let mut cursor = node.walk();
    let mut groups: Vec<Vec<Node>> = vec![Vec::new()];
    for child in node.children(&mut cursor) {
        match child.kind() {
            "import" | "export" => {}
            "," => groups.push(Vec::new()),
            _ => groups
                .last_mut()
                .expect("groups always has one element")
                .push(child),
        }
    }
    groups
        .iter()
        .flat_map(|group| parse_import_group(group, src))
        .collect()
}

fn parse_import_group(group: &[Node<'_>], src: &str) -> Vec<Import> {
    let mut parts: Vec<String> = group
        .iter()
        .filter(|child| matches!(child.kind(), "identifier" | "operator_identifier"))
        .map(|child| text_of(*child, src))
        .filter(|part| !part.is_empty())
        .collect();
    // In `import a.b.C as U` tree-sitter parses `C` as the rename's name, not
    // as a path segment, so the last segment of the path lives there.
    for child in group {
        if child.kind() == "as_renamed_identifier" {
            if let Some(name) = child.child_by_field_name("name") {
                let name = text_of(name, src);
                if !name.is_empty() && name != "." {
                    parts.push(name);
                }
            }
        }
    }
    let base = parts.join(".");
    if base.is_empty() {
        return Vec::new();
    }

    let mut imports = Vec::new();
    let mut had_spec = false;

    for child in group {
        match child.kind() {
            "namespace_wildcard" => {
                had_spec = true;
                imports.push(wildcard(&base));
            }
            "namespace_selectors" => {
                had_spec = true;
                imports.extend(parse_selectors(*child, &base, src));
            }
            "as_renamed_identifier" => {
                // `import a.b.C as U` imports `a.b.C` itself; `as _` hides it.
                had_spec = true;
                let hidden = child
                    .child_by_field_name("alias")
                    .is_some_and(|alias| alias.kind() == "wildcard");
                if !hidden {
                    imports.push(Import {
                        path: base.clone(),
                        kind: ImportKind::Type,
                    });
                }
            }
            // `import a.b.C.given Ordering[Int]` leaves a bare type node behind;
            // treat it like the other `given` forms, i.e. the whole object.
            _ => {}
        }
    }

    if had_spec {
        imports
    } else {
        vec![Import {
            path: base,
            kind: ImportKind::Type,
        }]
    }
}

fn parse_selectors(node: Node<'_>, base: &str, src: &str) -> Vec<Import> {
    let mut imports = Vec::new();
    let mut cursor = node.walk();
    for selector in node.named_children(&mut cursor) {
        match selector.kind() {
            "identifier" | "operator_identifier" => {
                let name = text_of(selector, src);
                if !name.is_empty() {
                    imports.push(Import {
                        path: format!("{base}.{name}"),
                        kind: ImportKind::Type,
                    });
                }
            }
            "namespace_wildcard" => imports.push(wildcard(base)),
            "arrow_renamed_identifier" | "as_renamed_identifier" => {
                // `C => D` / `C as D` imports C; `C => _` / `C as _` hides it.
                let hidden = selector
                    .child_by_field_name("alias")
                    .is_some_and(|alias| alias.kind() == "wildcard");
                if hidden {
                    continue;
                }
                if let Some(name) = selector.child_by_field_name("name") {
                    let name = text_of(name, src);
                    if !name.is_empty() {
                        imports.push(Import {
                            path: format!("{base}.{name}"),
                            kind: ImportKind::Type,
                        });
                    }
                }
            }
            // `given Ordering[Int]` and friends: the import comes from the
            // object named by the base path.
            _ => imports.push(wildcard(base)),
        }
    }
    imports
}

fn wildcard(base: &str) -> Import {
    Import {
        path: base.to_string(),
        kind: ImportKind::Wildcard,
    }
}

/// Collects type names (`type_identifier`) plus object-like receivers of field
/// accesses (`DemoDocument.create`), which Scala needs no import for when the
/// object lives in the same package.
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
            "identifier" => {
                let receiver = node.parent().is_some_and(|parent| {
                    parent.kind() == "field_expression"
                        && parent
                            .child_by_field_name("value")
                            .is_some_and(|value| value.id() == node.id())
                });
                if receiver {
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
        ScalaParser::new().parse(src).expect("parsed")
    }

    fn imports(source: &Source) -> Vec<(&str, ImportKind)> {
        source
            .imports
            .iter()
            .map(|import| (import.path.as_str(), import.kind))
            .collect()
    }

    #[test]
    fn nested_and_sequential_package_clauses() {
        let source = parse(
            r#"
            package com.example
            package app

            class Service
            object Registry
            trait Base
            enum Color { case Red }
            type Alias = Service

            package nested {
                class Deep
            }
            "#,
        );
        assert_eq!(source.package.as_deref(), Some("com.example.app"));
        assert_eq!(
            source.types,
            vec!["Service", "Registry", "Base", "Color", "Alias", "Deep"]
        );
    }

    #[test]
    fn braced_package_and_package_object() {
        let source = parse("package com.example { class A }\n");
        assert_eq!(source.package.as_deref(), Some("com.example"));
        assert_eq!(source.types, vec!["A"]);

        let source = parse("package com.example\npackage object util { val x = 1 }\n");
        assert_eq!(source.package.as_deref(), Some("com.example.util"));
        assert!(source.types.is_empty());
    }

    #[test]
    fn scala_2_and_3_import_forms() {
        let source = parse(
            r#"
            import scala.collection.mutable
            import scala.collection.mutable._
            import scala.collection.mutable.*
            import com.example.model.User
            import com.example.model.{Account, Order => PurchaseOrder}
            import com.example.model.{Hidden => _, *}
            import com.example.model.User.given
            import com.example.model.User.{given Ordering[Int], *}
            import com.example.model.User as U
            import com.example.model.User.Given
            import java.util.{List as JList}
            export com.example.model.User
            export com.example.service.{Api, *}
            "#,
        );
        assert_eq!(
            imports(&source),
            vec![
                ("scala.collection.mutable", ImportKind::Type),
                ("scala.collection.mutable", ImportKind::Wildcard),
                ("scala.collection.mutable", ImportKind::Wildcard),
                ("com.example.model.User", ImportKind::Type),
                ("com.example.model.Account", ImportKind::Type),
                ("com.example.model.Order", ImportKind::Type),
                ("com.example.model", ImportKind::Wildcard),
                ("com.example.model.User", ImportKind::Wildcard),
                ("com.example.model.User", ImportKind::Wildcard),
                ("com.example.model.User", ImportKind::Wildcard),
                ("com.example.model.User", ImportKind::Type),
                ("com.example.model.User.Given", ImportKind::Type),
                ("java.util.List", ImportKind::Type),
                ("com.example.model.User", ImportKind::Type),
                ("com.example.service.Api", ImportKind::Type),
                ("com.example.service", ImportKind::Wildcard),
            ]
        );
    }

    #[test]
    fn imports_inside_blocks_are_collected() {
        let source = parse(
            r#"
            package app
            class Service {
                import app.model.User
                def run(user: User) = ()
            }
            "#,
        );
        assert_eq!(imports(&source), vec![("app.model.User", ImportKind::Type)]);
    }

    #[test]
    fn collects_type_uses_and_object_receivers() {
        let source = parse(
            r#"
            package app
            class Service {
                val doc = DemoDocument.create()
                val user: User = null
                val list: List[User] = null
                def run(): Either[String, Account] = ???
            }
            "#,
        );
        for expected in [
            "DemoDocument",
            "User",
            "List",
            "Either",
            "String",
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
            package app

            abstract class Service(val repo: Repo, val count: Int) extends Base with Api {
              var helper: Helper = new Helper()

              def used(local: Local, h: Helper): Int = {
                val temp: Local = new Local()
                temp.ping()
                h.pong()
                repo.find()
                Helper.staticCall()
                this.inside()
                super.parent()
                count
              }

              def omitted: Int = 1
              def inside(): Unit = {}
              def nothing(): Unit
            }
            "#,
        );
        let service = &source.symbols[0];
        assert_eq!(service.name, "Service");
        assert_eq!(service.supertypes, vec!["Base", "Api"]);
        for expected in [("repo", "Repo"), ("count", "Int"), ("helper", "Helper")] {
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

        let omitted = service
            .members
            .iter()
            .find(|member| member.name == "omitted")
            .expect("omitted");
        assert!(omitted.body_start.is_some());
        let nothing = service
            .members
            .iter()
            .find(|member| member.name == "nothing")
            .expect("nothing");
        assert_eq!(nothing.body_start, None);
    }

    #[test]
    fn sbt_and_script_files_without_package() {
        let source = parse(
            r#"
            lazy val root = project.in(file("."))
            val x = 1
            "#,
        );
        assert_eq!(source.package, None);
        assert!(source.types.is_empty());
    }

    #[test]
    fn syntax_errors_still_yield_declarations() {
        let source = parse("package app\nclass Broken { def x( = ");
        assert_eq!(source.package.as_deref(), Some("app"));
        assert_eq!(source.types, vec!["Broken"]);
    }
}
