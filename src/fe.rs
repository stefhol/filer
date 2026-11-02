//! Asset Control Formula Engine code (`.fe`): top-level functions and variable
//! declarations, plus the names each one references.
//!
//! No tree-sitter grammar exists for FE, and filer never needs statements or
//! expressions — only where a function starts and ends, and what it calls. So
//! this is a small hand-written lexer plus a shallow bracket-depth scan:
//!
//! ```text
//! global integer size = 3;          <- Declaration symbol
//!
//! function total(list values) {     <- Function symbol
//!     local integer sum = size;     <- `size` is a use, `sum` is local
//!     return sum + len(values);     <- `len` is a built-in, not a call
//! }
//! ```
//!
//! Everything between a declaration's first token and its closing brace or
//! semicolon becomes the symbol's byte span; the bundle renderer slices exactly
//! that out of the file.

use std::collections::HashSet;

use crate::lang::{line_of, Source, Symbol, SymbolKind};

/// Built-in functions from `FormulaEngine.bnf`. They have no defining file, so
/// they must not turn into cross-file references.
const BUILTIN_FUNCS: &[&str] = &[
    "ado_browse",
    "attribute",
    "bsearch",
    "correct_price_field",
    "date",
    "datetime",
    "daydiff",
    "dayplus",
    "dict",
    "dict_get",
    "dict_iskey",
    "dict_keys",
    "dict_put",
    "dict_values",
    "elt",
    "evaluate",
    "first",
    "float",
    "hash_get",
    "hash_global",
    "hash_iskey",
    "hash_keys",
    "hash_put",
    "insert",
    "integer",
    "is_date",
    "is_datetime",
    "is_float",
    "is_integer",
    "is_list",
    "is_matrix",
    "is_na",
    "is_number",
    "is_opaque",
    "is_string",
    "is_time",
    "kernel",
    "last",
    "len",
    "list",
    "load",
    "lsearch",
    "map",
    "max",
    "out",
    "price_field",
    "rdbms_proc",
    "regex_match",
    "remove",
    "second",
    "set_suspect_price_field",
    "sort",
    "split",
    "sql",
    "status",
    "str",
    "string",
    "strip",
    "strval",
    "time",
    "toupper",
    "ts",
    "ts_update",
    "year_fraction",
];

const TYPE_KEYWORDS: &[&str] = &[
    "any", "boolean", "date", "datetime", "dict", "float", "integer", "list", "number", "string",
    "time",
];

const KEYWORDS: &[&str] = &[
    "and", "break", "case", "continue", "default", "do", "else", "for", "function", "if", "local",
    "global", "or", "return", "seq", "switch", "while",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Identifier, keyword, type or `$BUILT_IN_VAR`.
    Word,
    /// Number literal (integer, float or scientific).
    Number,
    /// String literal, quotes included.
    Text,
    /// `//` or `/* ... */`.
    Comment,
    /// Operator or punctuation.
    Punct,
}

#[derive(Debug, Clone, Copy)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
    start: usize,
    end: usize,
}

impl Token<'_> {
    fn is(&self, kind: Kind, text: &str) -> bool {
        self.kind == kind && self.text == text
    }
}

pub struct FeParser;

impl FeParser {
    pub fn new() -> Self {
        Self
    }

    pub fn parse(&mut self, src: &str) -> Option<Source> {
        let tokens = lex(src);
        let mut out = Source::default();

        let mut depth = 0usize;
        let mut index = 0usize;
        while index < tokens.len() {
            let token = &tokens[index];
            if depth == 0 {
                if let Some((symbol, next)) = parse_function(src, &tokens, index, None) {
                    out.symbols.push(symbol);
                    index = next;
                    continue;
                }
                if let Some((symbol, next)) = parse_visibility_or_declaration(src, &tokens, index) {
                    out.symbols.push(symbol);
                    index = next;
                    continue;
                }
            }

            match token.text {
                "{" if token.kind == Kind::Punct => depth += 1,
                "}" if token.kind == Kind::Punct => depth = depth.saturating_sub(1),
                _ => {}
            }
            index += 1;
        }

        out.types = out
            .symbols
            .iter()
            .map(|symbol| symbol.name.clone())
            .collect();
        out.preamble_end = out
            .symbols
            .iter()
            .map(|symbol| symbol.start)
            .min()
            .unwrap_or(0);
        Some(out)
    }
}

impl Default for FeParser {
    fn default() -> Self {
        Self::new()
    }
}

fn is_type(word: &str) -> bool {
    TYPE_KEYWORDS.contains(&word)
}

fn is_reserved(word: &str) -> bool {
    KEYWORDS.contains(&word) || is_type(word)
}

/// Parses the declaration at `index` when it starts with `global`/`local`, or
/// when it is a bare `type name = ...` (the grammar requires visibility, but
/// accepting both costs nothing and real files are not always that strict).
fn parse_visibility_or_declaration<'a>(
    src: &str,
    tokens: &[Token<'a>],
    index: usize,
) -> Option<(Symbol, usize)> {
    let token = tokens.get(index)?;
    if token.kind != Kind::Word {
        return None;
    }
    if token.text == "function" {
        return None;
    }
    let is_visibility = matches!(token.text, "global" | "local");
    if !is_visibility && !is_type(token.text) {
        return None;
    }
    if is_visibility
        && tokens
            .get(index + 1)
            .is_some_and(|next| next.kind == Kind::Word && next.text == "function")
    {
        return parse_function(src, tokens, index, Some(token.text));
    }

    // `global integer x`, `global x`, `integer x`.
    let mut name_index = index + 1;
    if is_visibility
        && tokens
            .get(name_index)
            .is_some_and(|next| is_type(next.text))
    {
        name_index += 1;
    }
    let name_token = tokens.get(name_index)?;
    if name_token.kind != Kind::Word || is_reserved(name_token.text) {
        return None;
    }

    // The initializer may contain `;` inside `seq(...)`, so balance brackets.
    let mut cursor = name_index + 1;
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut end = src.len();
    while cursor < tokens.len() {
        let current = &tokens[cursor];
        match current.text {
            "(" => parens += 1,
            ")" => parens = parens.saturating_sub(1),
            "[" => brackets += 1,
            "]" => brackets = brackets.saturating_sub(1),
            ";" if parens == 0 && brackets == 0 => {
                end = current.end;
                cursor += 1;
                break;
            }
            _ => {}
        }
        cursor += 1;
    }

    let mut declared = HashSet::new();
    declared.insert(name_token.text.to_string());
    let (calls, uses) = scan_references(tokens, name_index + 1, cursor, &[], &declared);

    let start = doc_start(src, tokens, index);
    let symbol = Symbol {
        name: name_token.text.to_string(),
        kind: SymbolKind::Declaration,
        start,
        end,
        line: line_of(src, start),
        uses,
        calls,
    };
    Some((symbol, cursor))
}

fn parse_function<'a>(
    src: &str,
    tokens: &[Token<'a>],
    index: usize,
    visibility: Option<&str>,
) -> Option<(Symbol, usize)> {
    let keyword_index = match visibility {
        Some(_) => index + 1,
        None => index,
    };
    let keyword = tokens.get(keyword_index)?;
    if keyword.kind != Kind::Word || keyword.text != "function" {
        return None;
    }

    let name_token = tokens.get(keyword_index + 1)?;
    if name_token.kind != Kind::Word || is_reserved(name_token.text) {
        return None;
    }

    // Parameters: `function name(Type a, b)`.
    let open = (keyword_index + 2..tokens.len()).find(|&i| tokens[i].is(Kind::Punct, "("))?;
    let close = matching(tokens, open, "(", ")")?;
    let params: Vec<String> = tokens[open + 1..close]
        .iter()
        .filter(|token| token.kind == Kind::Word && !is_reserved(token.text))
        .map(|token| token.text.to_string())
        .collect();

    // Body.
    let brace = (close + 1..tokens.len()).find(|&i| tokens[i].is(Kind::Punct, "{"))?;
    let body_end = matching(tokens, brace, "{", "}")?;
    let end = tokens[body_end].end;

    let (calls, uses) = scan_references(tokens, brace + 1, body_end, &params, &HashSet::new());

    let start = doc_start(src, tokens, index);
    let symbol = Symbol {
        name: name_token.text.to_string(),
        kind: SymbolKind::Function,
        start,
        end,
        line: line_of(src, start),
        uses,
        calls,
    };
    Some((symbol, body_end + 1))
}

/// Reference scan for the token range `from..to` (inside a symbol's body).
///
/// Names declared locally (parameters, `local type x`, loop variables) are
/// dropped, so a 20 000-line file does not drag in half the project through
/// variable names. Calls are kept apart from plain uses, and built-ins are
/// ignored because no file defines them.
fn scan_references(
    tokens: &[Token<'_>],
    from: usize,
    to: usize,
    params: &[String],
    extra_declared: &HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut declared: HashSet<String> = params.iter().cloned().collect();
    declared.extend(extra_declared.iter().cloned());
    let mut nested_functions: HashSet<&str> = HashSet::new();

    // Pass 1: what does this scope declare?
    for index in from..to {
        let token = &tokens[index];
        if token.kind != Kind::Word {
            continue;
        }
        if token.text == "function" {
            if let Some(name) = tokens.get(index + 1) {
                if name.kind == Kind::Word && !is_reserved(name.text) {
                    nested_functions.insert(name.text);
                    declared.insert(name.text.to_string());
                }
            }
            continue;
        }
        if matches!(token.text, "global" | "local") {
            let mut next = index + 1;
            if tokens.get(next).is_some_and(|token| is_type(token.text)) {
                next += 1;
            }
            if let Some(name) = tokens.get(next) {
                if name.kind == Kind::Word && !is_reserved(name.text) {
                    declared.insert(name.text.to_string());
                }
            }
            continue;
        }
        if is_type(token.text) {
            if let Some(name) = tokens.get(index + 1) {
                if name.kind == Kind::Word && !is_reserved(name.text) {
                    declared.insert(name.text.to_string());
                }
            }
        }
    }

    // Pass 2: calls and uses.
    let mut calls = Vec::new();
    let mut uses = Vec::new();
    for index in from..to {
        let token = &tokens[index];
        if token.kind != Kind::Word || is_reserved(token.text) || token.text.starts_with('$') {
            continue;
        }
        if declared.contains(token.text) || nested_functions.contains(token.text) {
            continue;
        }

        let next = tokens.get(index + 1);
        let previous = index.checked_sub(1).and_then(|i| tokens.get(i));
        if next.is_some_and(|next| next.is(Kind::Punct, "(")) {
            if !BUILTIN_FUNCS.contains(&token.text) {
                calls.push(token.text.to_string());
            }
            continue;
        }
        // Assignment/declaration target, or the name after a type keyword.
        if next.is_some_and(|next| next.is(Kind::Punct, "=")) {
            continue;
        }
        if previous.is_some_and(|previous| {
            previous.text == "function"
                || is_type(previous.text)
                || matches!(previous.text, "global" | "local")
        }) {
            continue;
        }
        uses.push(token.text.to_string());
    }

    calls.sort();
    calls.dedup();
    uses.sort();
    uses.dedup();
    (calls, uses)
}

/// Index of the token closing the bracket opened at `open`.
fn matching(tokens: &[Token<'_>], open: usize, left: &str, right: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        if token.is(Kind::Punct, left) {
            depth += 1;
        } else if token.is(Kind::Punct, right) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// Start of a symbol, including a doc comment directly above it. A blank line
/// between the comment and the declaration keeps the comment out.
fn doc_start(src: &str, tokens: &[Token<'_>], index: usize) -> usize {
    let mut start = tokens[index].start;
    let mut cursor = index;
    while cursor > 0 {
        let previous = tokens[cursor - 1];
        if previous.kind != Kind::Comment {
            break;
        }
        if src[previous.end..start].matches('\n').count() > 1 {
            break;
        }
        // A comment at the end of a previous statement is not a doc comment.
        let line_start = src[..previous.start]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        if !src[line_start..previous.start].trim().is_empty() {
            break;
        }
        start = previous.start;
        cursor -= 1;
    }
    start
}

fn lex(src: &str) -> Vec<Token<'_>> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        let start = index;

        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            tokens.push(token(src, Kind::Comment, start, index));
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
            tokens.push(token(src, Kind::Comment, start, index));
            continue;
        }
        if byte == b'"' || byte == b'\'' {
            let quote = byte;
            index += 1;
            while index < bytes.len() {
                match bytes[index] {
                    b'\\' => index += 2,
                    b'\n' => break,
                    current if current == quote => {
                        index += 1;
                        break;
                    }
                    _ => index += 1,
                }
            }
            index = index.min(bytes.len());
            tokens.push(token(src, Kind::Text, start, index));
            continue;
        }
        if byte.is_ascii_digit() {
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if index + 1 < bytes.len() && bytes[index] == b'.' && bytes[index + 1].is_ascii_digit()
            {
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
            }
            if index < bytes.len() && (bytes[index] == b'E' || bytes[index] == b'e') {
                let exponent = index;
                index += 1;
                if index < bytes.len() && matches!(bytes[index], b'+' | b'-') {
                    index += 1;
                }
                let digits = index;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
                if index == digits {
                    index = exponent;
                }
            }
            tokens.push(token(src, Kind::Number, start, index));
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$' {
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push(token(src, Kind::Word, start, index));
            continue;
        }
        if byte >= 0x80 {
            // Not FE: skip a whole character so slicing stays on boundaries.
            index += src[start..].chars().next().map_or(1, char::len_utf8);
            continue;
        }

        // Longest operator first.
        let rest = &src[index..];
        let width = [
            "+=", "-=", "*=", "/=", "%=", "==", "!=", ">=", "<=", "&&", "||", "++", "--",
        ]
        .iter()
        .find(|operator| rest.starts_with(*operator))
        .map_or(1, |operator| operator.len());
        index += width;
        tokens.push(token(src, Kind::Punct, start, index));
    }
    tokens
}

fn token<'a>(src: &'a str, kind: Kind, start: usize, end: usize) -> Token<'a> {
    Token {
        kind,
        text: &src[start..end],
        start,
        end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Source {
        FeParser::new().parse(src).expect("parsed")
    }

    fn names(source: &Source) -> Vec<&str> {
        source
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect()
    }

    #[test]
    fn functions_and_declarations() {
        let src = r#"
            // Header.
            global integer size = 3;

            function helper(integer a) {
                local integer b = a + 1;
                return b;
            }

            /* Doc for main. */
            function main(string symbol) {
                local dict cache = dict();
                return helper(size) + cache;
            }
        "#;
        let source = parse(src);
        assert_eq!(names(&source), vec!["size", "helper", "main"]);
        assert_eq!(source.types, names(&source));
        assert_eq!(source.symbols[0].kind, SymbolKind::Declaration);
        assert_eq!(source.symbols[1].kind, SymbolKind::Function);

        let helper = &source.symbols[1];
        assert!(helper.calls.is_empty(), "{helper:?}");
        assert!(helper.uses.is_empty(), "{helper:?}");

        let main = &source.symbols[2];
        assert_eq!(main.calls, vec!["helper"]);
        assert_eq!(main.uses, vec!["size"]);
        assert!(src[main.start..main.end].starts_with("/* Doc for main. */"));
        assert!(src[main.start..main.end].ends_with('}'));
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_confuse_depth() {
        let src = r#"
            function a() {
                // }
                local string s = "{ not a body }";
                return s;
            }
            function b() { return "}"; }
        "#;
        let source = parse(src);
        assert_eq!(names(&source), vec!["a", "b"]);
    }

    #[test]
    fn nested_functions_stay_inside_the_outer_excerpt() {
        let src = r#"
            function outer() {
                function inner() { return 1; }
                return inner();
            }
        "#;
        let source = parse(src);
        assert_eq!(names(&source), vec!["outer"]);
        assert!(
            source.symbols[0].calls.is_empty(),
            "{:?}",
            source.symbols[0]
        );
        let text = &src[source.symbols[0].start..source.symbols[0].end];
        assert!(text.contains("function inner"), "{text}");
    }

    #[test]
    fn bare_typed_declarations_are_symbols() {
        let src = "string greeting = \"hi\";\ndict prices = dict();\n";
        let source = parse(src);
        assert_eq!(names(&source), vec!["greeting", "prices"]);
        assert_eq!(source.symbols[0].kind, SymbolKind::Declaration);
    }

    #[test]
    fn builtins_and_locals_are_not_references() {
        let src = r#"
            function f(list values) {
                local integer total = len(values) + 1;
                for (local integer i = 0; i < total; i++) {
                    out(i);
                }
                return ts("x");
            }
        "#;
        let source = parse(src);
        assert!(
            source.symbols[0].calls.is_empty(),
            "{:?}",
            source.symbols[0]
        );
        assert!(source.symbols[0].uses.is_empty(), "{:?}", source.symbols[0]);
    }

    #[test]
    fn visibility_prefixed_functions_are_tolerated() {
        let src = "global function g() { return 1; }\n";
        let source = parse(src);
        assert_eq!(names(&source), vec!["g"]);
        assert_eq!(source.symbols[0].kind, SymbolKind::Function);
    }

    #[test]
    fn tabs_and_windows_newlines_keep_offsets_consistent() {
        let src = "function a() {\r\n\treturn 1;\r\n}\r\nfunction b() { return 2; }\r\n";
        let source = parse(src);
        assert_eq!(names(&source), vec!["a", "b"]);
        for symbol in &source.symbols {
            assert_eq!(&src[symbol.start..symbol.start + 8], "function");
        }
    }
}
