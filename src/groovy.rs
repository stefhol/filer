//! Groovy parsing via tree-sitter: package, top-level declarations and imports.
//! Also covers Gradle build scripts (`.gradle`), which are Groovy.

use tree_sitter::{Node, Parser};

use crate::lang::{finish_symbols, symbol_from_node, Import, ImportKind, Source, SymbolKind};

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
        out.symbols.push(symbol_from_node(
            declaration,
            &text,
            SymbolKind::Type,
            &["comment", "line_comment", "block_comment"],
            src,
        ));
        out.types.push(text);
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
    fn gradle_script_without_package() {
        let source = parse("plugins { id 'java' }\ndependencies { implementation \"x:y:1\" }\n");
        assert_eq!(source.package, None);
        assert!(source.types.is_empty());
    }
}
