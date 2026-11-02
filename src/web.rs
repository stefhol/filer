//! TypeScript, JavaScript and Vue single-file components.
//!
//! Vue files have no grammar of their own here: their `<script>` blocks are
//! extracted and parsed with the TypeScript grammar, which covers both TS and
//! plain JS.

use tree_sitter::{Node, Parser};

use crate::lang::{
    finish_symbols, symbol_from_node, Import, ImportKind, Language, Source, SymbolKind,
};

pub struct WebParser {
    typescript: Parser,
    javascript: Parser,
}

const TYPE_DECLS: &[&str] = &[
    "class_declaration",
    "abstract_class_declaration",
    "interface_declaration",
    "type_alias_declaration",
    "enum_declaration",
];

impl WebParser {
    pub fn new() -> Self {
        let mut typescript = Parser::new();
        typescript
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .expect("tree-sitter-typescript grammar failed to load");
        let mut javascript = Parser::new();
        javascript
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .expect("tree-sitter-javascript grammar failed to load");
        Self {
            typescript,
            javascript,
        }
    }

    pub fn parse(&mut self, language: Language, src: &str) -> Option<Source> {
        match language {
            Language::JavaScript => parse_script(&mut self.javascript, src),
            Language::TypeScript => parse_script(&mut self.typescript, src),
            Language::Vue => parse_vue(&mut self.typescript, src),
            _ => None,
        }
    }
}

impl Default for WebParser {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_script(parser: &mut Parser, src: &str) -> Option<Source> {
    let tree = parser.parse(src, None)?;
    let root = tree.root_node();
    let mut out = Source::default();

    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        let declaration = match child.kind() {
            "export_statement" => child.child_by_field_name("declaration").unwrap_or(child),
            _ => child,
        };
        if TYPE_DECLS.contains(&declaration.kind()) {
            if let Some(name) = declaration.child_by_field_name("name") {
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
        }
    }

    collect_module_imports(root, src, &mut out.imports);
    finish_symbols(&mut out, &[]);
    // Module languages resolve explicitly imported files; name inference across
    // the whole repo would be guessing.
    Some(out)
}

/// Collects every module specifier: static imports and re-exports, dynamic
/// `import()` and CommonJS `require()`.
fn collect_module_imports(node: Node<'_>, src: &str, out: &mut Vec<Import>) {
    match node.kind() {
        "import_statement" | "export_statement" => {
            if let Some(source) = node.child_by_field_name("source") {
                push_specifier(source, src, out);
            }
        }
        "call_expression" => {
            if let Some(function) = node.child_by_field_name("function") {
                let name = text_of(function, src);
                if function.kind() == "import" || name == "require" || name == "require.resolve" {
                    if let Some(arguments) = node.child_by_field_name("arguments") {
                        let mut cursor = arguments.walk();
                        let first = arguments.named_children(&mut cursor).next();
                        if let Some(first) = first {
                            push_specifier(first, src, out);
                        }
                    }
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_module_imports(child, src, out);
    }
}

fn push_specifier(node: Node<'_>, src: &str, out: &mut Vec<Import>) {
    if node.kind() != "string" {
        return;
    }
    if let Some(path) = string_value(node, src) {
        if !path.is_empty() {
            out.push(Import {
                path,
                kind: ImportKind::Module,
            });
        }
    }
}

fn string_value(node: Node<'_>, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    if let Some(fragment) = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "string_fragment")
    {
        return Some(text_of(fragment, src));
    }
    let text = text_of(node, src);
    let trimmed = text.trim();
    let quote = trimmed.chars().next()?;
    if matches!(quote, '\'' | '"' | '`') && trimmed.ends_with(quote) && trimmed.len() >= 2 {
        return Some(trimmed[1..trimmed.len() - 1].to_string());
    }
    None
}

fn parse_vue(parser: &mut Parser, src: &str) -> Option<Source> {
    let mut out = Source::default();
    for script in script_blocks(src) {
        if let Some(parsed) = parse_script(parser, script) {
            out.types.extend(parsed.types);
            out.imports.extend(parsed.imports);
        }
    }
    out.types.sort();
    out.types.dedup();
    out.imports
        .dedup_by(|a, b| a.path == b.path && a.kind == b.kind);
    // Script offsets are relative to the extracted block, not the `.vue` file.
    out.symbols.clear();
    out.preamble_end = 0;
    Some(out)
}

/// Contents of every `<script>` block in a Vue SFC (skips self-closing tags and
/// handles both `<script>` and `<script setup lang="ts">`).
fn script_blocks(src: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = src;
    while let Some(start) = rest.find("<script") {
        let after_name = &rest[start + "<script".len()..];
        let Some(gt) = after_name.find('>') else {
            break;
        };
        let attributes = &after_name[..gt];
        let body = &after_name[gt + 1..];
        let Some(end) = body.find("</script>") else {
            break;
        };
        if !attributes.trim_end().ends_with('/') {
            blocks.push(&body[..end]);
        }
        rest = &body[end + "</script>".len()..];
    }
    blocks
}

fn text_of<'a>(node: Node<'a>, src: &str) -> String {
    src.get(node.byte_range()).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(language: Language, src: &str) -> Source {
        WebParser::new().parse(language, src).expect("parsed")
    }

    #[test]
    fn typescript_types_and_module_specifiers() {
        let source = parse(
            Language::TypeScript,
            r#"
            import { User } from "./model/user";
            import type { Account } from "../model/account";
            import * as repo from "./repo";
            import Default, { Thing as Alias } from './thing.js';
            export { Extra } from "./extra";
            export * from "./all";
            const x = await import("./dynamic");
            const y = require("./legacy");

            export class Service implements Base {}
            export interface Base {}
            export type Alias2 = User;
            export enum Color { Red }
            "#,
        );
        assert_eq!(source.types, vec!["Service", "Base", "Alias2", "Color"]);
        assert_eq!(
            source
                .imports
                .iter()
                .map(|import| (import.path.as_str(), import.kind))
                .collect::<Vec<_>>(),
            vec![
                ("./model/user", ImportKind::Module),
                ("../model/account", ImportKind::Module),
                ("./repo", ImportKind::Module),
                ("./thing.js", ImportKind::Module),
                ("./extra", ImportKind::Module),
                ("./all", ImportKind::Module),
                ("./dynamic", ImportKind::Module),
                ("./legacy", ImportKind::Module),
            ]
        );
        assert!(source.uses.is_empty(), "module languages rely on imports");
    }

    #[test]
    fn javascript_types_and_requires() {
        let source = parse(
            Language::JavaScript,
            r#"
            import User from "./user";
            const a = require("./a");
            export class Service extends Base {}
            export default class {}
            "#,
        );
        assert_eq!(source.types, vec!["Service"]);
        assert_eq!(
            source
                .imports
                .iter()
                .map(|import| import.path.as_str())
                .collect::<Vec<_>>(),
            vec!["./user", "./a"]
        );
    }

    #[test]
    fn vue_script_blocks_are_parsed() {
        let source = parse(
            Language::Vue,
            r#"
            <template><div>{{ msg }}</div></template>
            <script setup lang="ts">
            import { User } from "./model/user";
            interface Props { user: User }
            </script>
            <script>
            export default { name: "Demo" };
            </script>
            <style scoped>.x { color: red }</style>
            "#,
        );
        assert_eq!(source.types, vec!["Props"]);
        assert_eq!(
            source
                .imports
                .iter()
                .map(|import| import.path.as_str())
                .collect::<Vec<_>>(),
            vec!["./model/user"]
        );
    }

    #[test]
    fn vue_without_script_is_empty() {
        let source = parse(Language::Vue, "<template><div/></template>");
        assert!(source.types.is_empty());
        assert!(source.imports.is_empty());
    }
}
