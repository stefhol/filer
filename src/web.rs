//! TypeScript, JavaScript and Vue single-file components.
//!
//! Vue files have no grammar of their own here: their `<script>` blocks are
//! extracted and parsed with the TypeScript grammar, which covers both TS and
//! plain JS.

use tree_sitter::{Node, Parser};

use crate::lang::{
    end_line_of, finish_symbols, leading_comment_start, line_of, symbol_from_node, Call, Import,
    ImportKind, Language, Member, MemberKind, Receiver, Source, Symbol, SymbolKind,
};

const COMMENTS: &[&str] = &["comment"];

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
                    let mut symbol =
                        symbol_from_node(child, &text, SymbolKind::Type, COMMENTS, src);
                    collect_members(declaration, src, &mut symbol);
                    out.symbols.push(symbol);
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

/// Methods, constructors, fields and supertypes of one class, with the calls
/// each member makes and the receiver types the resolver needs. JavaScript has
/// no type annotations, so most receiver edges need `this` or a bare call.
fn collect_members(class: Node<'_>, src: &str, symbol: &mut Symbol) {
    symbol.supertypes = supertypes(class, src);
    let Some(body) = class.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        match member.kind() {
            "method_definition" | "method_signature" | "abstract_method_signature" => {
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
                let kind = if name == "constructor" {
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
                // Constructor parameter properties (`constructor(private repo:
                // Repo)`) are fields.
                if kind == MemberKind::Constructor {
                    if let Some(parameters) = member.child_by_field_name("parameters") {
                        for (name, ty) in parameter_names(Some(parameters), src) {
                            if parameter_is_property(parameters, &name, src) {
                                symbol.fields.push((name, ty));
                            }
                        }
                    }
                }
            }
            "public_field_definition" => {
                if let (Some(name), Some(ty)) = (
                    member.child_by_field_name("name"),
                    member
                        .child_by_field_name("type")
                        .and_then(|ty| type_name(ty, src)),
                ) {
                    symbol.fields.push((text_of(name, src), ty));
                }
            }
            _ => {}
        }
    }
}

fn parameter_is_property(parameters: Node<'_>, name: &str, src: &str) -> bool {
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter_name(parameter, src).as_deref() != Some(name) {
            continue;
        }
        let mut inner = parameter.walk();
        if parameter
            .children(&mut inner)
            .any(|child| child.kind() == "accessibility_modifier")
        {
            return true;
        }
    }
    false
}

fn parameter_names(parameters: Option<Node<'_>>, src: &str) -> Vec<(String, String)> {
    let Some(parameters) = parameters else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if !matches!(
            parameter.kind(),
            "required_parameter" | "optional_parameter"
        ) {
            continue;
        }
        let Some(name) = parameter_name(parameter, src) else {
            continue;
        };
        if let Some(ty) = parameter
            .child_by_field_name("type")
            .and_then(|ty| type_name(ty, src))
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
        if node.kind() == "variable_declarator" {
            if let (Some(name), Some(ty)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("type")
                    .and_then(|ty| type_name(ty, src)),
            ) {
                if name.kind() == "identifier" {
                    out.push((text_of(name, src), ty));
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

/// Name of a parameter, from `name` or from a plain identifier `pattern`.
fn parameter_name(parameter: Node<'_>, src: &str) -> Option<String> {
    if let Some(name) = parameter.child_by_field_name("name") {
        if name.kind() == "identifier" {
            return Some(text_of(name, src));
        }
    }
    parameter
        .child_by_field_name("pattern")
        .filter(|pattern| pattern.kind() == "identifier")
        .map(|pattern| text_of(pattern, src))
}

/// Simple name of a type node: `Repo`, `List` for `List<Repo>`, `Repo` for
/// `a.b.Repo` and `string`/`number` for predefined types.
fn type_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "predefined_type" => {
            let text = text_of(node, src);
            (!text.is_empty()).then_some(text)
        }
        "generic_type" => node
            .child_by_field_name("name")
            .and_then(|name| type_name(name, src)),
        "nested_type_identifier" => node
            .child_by_field_name("name")
            .and_then(|name| type_name(name, src)),
        "type_annotation" | "type_arguments" | "readonly_type" | "parenthesized_type" => {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find_map(|child| type_name(child, src));
            found
        }
        "union_type" | "intersection_type" | "array_type" => {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find_map(|child| type_name(child, src));
            found
        }
        _ => None,
    }
}

fn supertypes(class: Node<'_>, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = class.walk();
    for heritage in class.named_children(&mut cursor) {
        if heritage.kind() != "class_heritage" {
            continue;
        }
        let mut inner = heritage.walk();
        for clause in heritage.named_children(&mut inner) {
            match clause.kind() {
                "extends_clause" => {
                    let mut values = clause.walk();
                    for value in clause.children_by_field_name("value", &mut values) {
                        if let Some(name) = expression_name(value, src) {
                            out.push(name);
                        }
                    }
                }
                "implements_clause" => {
                    let mut types = clause.walk();
                    for ty in clause.named_children(&mut types) {
                        if let Some(name) = type_name(ty, src) {
                            out.push(name);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Name of a class expression: `Base` or `ns.Base`.
fn expression_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "type_identifier" => {
            let text = text_of(node, src);
            (!text.is_empty()).then_some(text)
        }
        "member_expression" => node
            .child_by_field_name("property")
            .map(|property| text_of(property, src)),
        "call_expression" => node
            .child_by_field_name("function")
            .and_then(|function| expression_name(function, src)),
        _ => None,
    }
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
        "identifier" => {
            let name = text_of(function, src);
            (!name.is_empty()).then_some(Call {
                name,
                receiver: Receiver::Bare,
                line,
            })
        }
        "member_expression" => {
            let name = function
                .child_by_field_name("property")
                .map(|property| text_of(property, src))?;
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
        _ => None,
    }
}

fn receiver_of(node: Node<'_>, src: &str) -> Receiver {
    match node.kind() {
        "this" => Receiver::This,
        "super" => Receiver::Super,
        "identifier" => {
            let text = text_of(node, src);
            match text.as_str() {
                "this" => Receiver::This,
                "super" => Receiver::Super,
                _ => Receiver::Name(text),
            }
        }
        "member_expression" => node
            .child_by_field_name("property")
            .map(|property| Receiver::Name(text_of(property, src)))
            .unwrap_or(Receiver::Unknown),
        "new_expression" => node
            .child_by_field_name("constructor")
            .and_then(|constructor| expression_name(constructor, src))
            .map(Receiver::Name)
            .unwrap_or(Receiver::Unknown),
        _ => Receiver::Unknown,
    }
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
    fn extracts_class_members_fields_supertypes_and_calls() {
        let source = parse(
            Language::TypeScript,
            r#"
            export abstract class Service extends Base implements Api {
                private count: number;

                constructor(private repo: Repo) { this.repo = repo; }

                used(local: Local, helper: Helper): number {
                    const temp: Local = new Local();
                    temp.ping();
                    helper.pong();
                    this.repo.size();
                    Helper.staticCall();
                    this.self();
                    super.parent();
                    return this.count;
                }

                omitted(): number { return 1; }
                self(): void {}
                abstract nothing(): void;
            }
            "#,
        );
        let service = &source.symbols[0];
        assert_eq!(service.name, "Service");
        assert_eq!(service.supertypes, vec!["Base", "Api"]);
        for expected in [("repo", "Repo"), ("count", "number")] {
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
                ("helper".to_string(), "Helper".to_string()),
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
            ("pong", Receiver::Name("helper".to_string())),
            ("size", Receiver::Name("repo".to_string())),
            ("staticCall", Receiver::Name("Helper".to_string())),
            ("self", Receiver::This),
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
