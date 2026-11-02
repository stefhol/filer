//! C# parsing via tree-sitter: namespaces (including file-scoped ones), `using`
//! directives and top-level type declarations.

use tree_sitter::{Node, Parser};

use crate::lang::{finish_symbols, symbol_from_node, Import, ImportKind, Source, SymbolKind};

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
