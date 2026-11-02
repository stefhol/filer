//! Scala parsing via tree-sitter: (possibly nested) package clauses, top-level
//! type declarations and Scala 2/3 imports.

use tree_sitter::{Node, Parser};

use crate::lang::{finish_symbols, symbol_from_node, Import, ImportKind, Source, SymbolKind};

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
                        out.symbols.push(symbol_from_node(
                            child,
                            &text,
                            SymbolKind::Type,
                            &["comment"],
                            src,
                        ));
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
