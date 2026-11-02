//! Kotlin parsing via tree-sitter: package header, top-level declarations and
//! imports (wildcards, aliases, member imports).

use tree_sitter::{Node, Parser};

use crate::lang::{finish_symbols, symbol_from_node, Import, ImportKind, Source, SymbolKind};

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
        out.symbols.push(symbol_from_node(
            declaration,
            &text,
            SymbolKind::Type,
            &["line_comment", "multiline_comment", "comment"],
            src,
        ));
        out.types.push(text);
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
    fn script_without_package() {
        let source = parse("val x = 1\nfun main() = println(x)\n");
        assert_eq!(source.package, None);
        assert!(source.types.is_empty());
    }
}
