//! Java parsing via tree-sitter: package, top-level types and imports.

use tree_sitter::{Node, Parser};

use crate::lang::{Import, ImportKind, Source};

pub struct JavaParser {
    parser: Parser,
}

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
                            out.types.push(text);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut uses = Vec::new();
        collect_type_uses(root, src, &mut uses);
        uses.sort();
        uses.dedup();
        out.uses = uses;
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

/// Walks the whole tree collecting every `type_identifier` (class names in
/// type positions, class literals, generics, casts, `new` expressions).
fn collect_type_uses(root: Node<'_>, src: &str, out: &mut Vec<String>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "type_identifier" {
            let text = text_of(node, src);
            if !text.is_empty() {
                out.push(text);
            }
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
    fn default_package_and_syntax_errors() {
        let parsed = parse("class Broken { void x( { }");
        assert_eq!(parsed.package, None);
        assert_eq!(parsed.types, vec!["Broken"]);
    }
}
