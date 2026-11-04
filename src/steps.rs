//! Cucumber step definitions: discovery in code files and matching in
//! `.feature` files.
//!
//! Gherkin has no imports, so a feature cannot name its glue through a path.
//! Cucumber's actual contract is textual: a step name has to match a pattern
//! declared by an annotation, attribute, decorator or lambda call somewhere in
//! the repository. This module finds those declarations and compiles their
//! patterns once per index, then answers "which definitions match this step".
//!
//! Three pattern dialects are supported, in the order Cucumber uses them:
//!
//! - regular expressions, anchored on both ends (`^I have (\d+) cukes$`),
//! - cucumber expressions (`I have {int} cukes`),
//! - plain text, compared literally (`I have 3 cukes`).

use crate::index::FileEntry;
use crate::lang::{line_of, Language, StepDefinition, Symbol};

/// Step-definition declarations are recognised by the keyword that introduces
/// them. The keyword list is per language because the syntax around it differs:
/// Java/Kotlin/C#/JS write `@Given("...")`, `[Given("...")]` or `Given("...")`,
/// while Python and Rust only ever decorate (`@given`, `#[given]`).
fn keywords(language: Language) -> Option<&'static [&'static str]> {
    const BARE: &[&str] = &["Given(", "When(", "Then(", "And(", "But("];
    Some(match language {
        Language::Java | Language::Kotlin | Language::Groovy | Language::Scala => BARE,
        Language::CSharp => BARE,
        Language::TypeScript | Language::JavaScript | Language::Vue => &[
            "Given(",
            "When(",
            "Then(",
            "And(",
            "But(",
            "defineStep(",
            "Step(",
        ],
        Language::Python => &["@given(", "@when(", "@then(", "@and_(", "@but_(", "@step("],
        Language::Rust => &["#[given(", "#[when(", "#[then("],
        Language::Fe | Language::Gherkin | Language::Robot => return None,
    })
}

/// Finds every step definition in one parsed file and attributes it to the
/// enclosing symbol/member.
pub fn scan(language: Language, src: &str, symbols: &[Symbol]) -> Vec<StepDefinition> {
    let Some(keywords) = keywords(language) else {
        return Vec::new();
    };
    let mut out: Vec<StepDefinition> = Vec::new();
    let mut start = 0;
    for line in src.split_inclusive('\n') {
        for keyword in keywords {
            let mut from = 0;
            while let Some(offset) = line[from..].find(keyword) {
                let position = from + offset;
                from = position + keyword.len();
                if !starts_token(
                    line,
                    position,
                    keyword.starts_with('@') || keyword.starts_with('#'),
                ) {
                    continue;
                }
                let open = position + keyword.len();
                let Some((pattern, regex)) = literal_at(line, open) else {
                    continue;
                };
                let byte = start + position;
                let (symbol, member) = attribute(symbols, byte);
                let definition = StepDefinition {
                    regex: regex || looks_like_regex(&pattern),
                    pattern,
                    line: line_of(src, byte),
                    symbol,
                    member,
                };
                if out.iter().any(|known| {
                    known.pattern == definition.pattern
                        && known.line == definition.line
                        && known.member == definition.member
                }) {
                    continue;
                }
                out.push(definition);
            }
        }
        start += line.len();
    }
    out
}

/// Rejects `obj.Given(` and `MyGiven(`: a declaration starts at a token
/// boundary. Decorator keywords carry their own `@`/`#`, so they are exempt.
fn starts_token(line: &str, position: usize, decorated: bool) -> bool {
    if position == 0 {
        return true;
    }
    let previous = line.as_bytes()[position - 1];
    if decorated {
        return true;
    }
    !(previous.is_ascii_alphanumeric() || previous == b'_' || previous == b'.')
}

/// Parses the first string literal of the argument list, which is the step
/// pattern in every supported dialect. `None` when the first argument is not a
/// literal (`@Given(SOME_CONSTANT)`): Cucumber itself would not know the
/// pattern without running the code.
fn literal_at(line: &str, open: usize) -> Option<(String, bool)> {
    let mut index = open;
    // Only whitespace and an optional `value =` may precede the literal.
    skip_whitespace(line, &mut index);
    if line[index..].starts_with("value") {
        index += "value".len();
        skip_whitespace(line, &mut index);
        if line[index..].starts_with('=') {
            index += 1;
            skip_whitespace(line, &mut index);
        }
    }
    if line[index..].starts_with("@\"") {
        return verbatim(line, index + 1).map(|text| (text, false));
    }
    let rest = &line[index..];
    let quote = rest.chars().next()?;
    if quote == '/' {
        let end = rest[1..].find('/')? + index + 1;
        return Some((rest[1..(end - index)].to_string(), true));
    }
    if !matches!(quote, '"' | '\'' | '`') {
        return None;
    }
    quoted(line, index).map(|text| (text, false))
}

fn skip_whitespace(line: &str, index: &mut usize) {
    while line
        .as_bytes()
        .get(*index)
        .is_some_and(u8::is_ascii_whitespace)
    {
        *index += 1;
    }
}

/// A quoted literal starting at `index`, with Kotlin/Scala/Groovy triple
/// quotes handled first. Escapes that matter for a pattern (`\d`, `\.`) are
/// kept verbatim; only quotes, backslashes and newlines are unescaped.
fn quoted(line: &str, index: usize) -> Option<String> {
    let rest = &line[index..];
    if let Some(body) = rest.strip_prefix("\"\"\"") {
        let end = body.find("\"\"\"")?;
        return Some(body[..end].to_string());
    }
    let quote = rest.chars().next()?;
    let mut out = String::new();
    let mut chars = rest[quote.len_utf8()..].chars();
    while let Some(character) = chars.next() {
        match character {
            '\\' => {
                let escaped = chars.next()?;
                match escaped {
                    '"' | '\'' | '\\' | '`' => out.push(escaped),
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            character if character == quote => return Some(out),
            character => out.push(character),
        }
    }
    None
}

/// C# verbatim string (`@"..."`), where `""` is an escaped quote and
/// backslashes are literal.
fn verbatim(line: &str, index: usize) -> Option<String> {
    let rest = line[index..].strip_prefix('"')?;
    let mut out = String::new();
    let mut characters = rest.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '"' {
            if characters.peek() == Some(&'"') {
                characters.next();
                out.push('"');
                continue;
            }
            return Some(out);
        }
        out.push(character);
    }
    None
}

/// Whether a literal should be compiled as a regular expression rather than as
/// cucumber expression/plain text. Anchors and character classes only make
/// sense as regexes; `{int}`-style placeholders only make sense as expressions.
fn looks_like_regex(pattern: &str) -> bool {
    if is_expression(pattern) {
        return false;
    }
    pattern
        .chars()
        .any(|character| matches!(character, '^' | '$' | '\\' | '[' | ']' | '(' | ')' | '|'))
}

/// Whether the literal uses cucumber expression placeholders. A pattern
/// without one is not an expression, even though it *could* be compiled as
/// one: plain literals must not treat `.` or `(` as regex syntax.
fn is_expression(pattern: &str) -> bool {
    let mut rest = pattern;
    let mut found = false;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            return found;
        };
        let placeholder = &rest[start + 1..start + end];
        if !matches!(
            placeholder,
            "" | "int" | "float" | "word" | "string" | "any"
        ) {
            return found;
        }
        found = true;
        rest = &rest[start + end + 1..];
    }
    found
}

/// A compiled step pattern. Built once per index (the index holds a
/// [`StepIndex`]), so resolution never recompiles a pattern.
#[derive(Debug)]
pub enum Pattern {
    Literal(String),
    Regex(regex::Regex),
}

impl Pattern {
    pub fn matches(&self, text: &str) -> bool {
        match self {
            Pattern::Literal(literal) => literal.trim() == text.trim(),
            Pattern::Regex(regex) => regex.is_match(text.trim()),
        }
    }
}

/// Compiles one definition's pattern. `None` when a regular expression is
/// invalid; such a definition is skipped instead of failing the index.
pub fn compile(definition: &StepDefinition) -> Option<Pattern> {
    let pattern = definition.pattern.trim();
    if pattern.is_empty() {
        return None;
    }
    if !definition.regex && !looks_like_regex(pattern) && !is_expression(pattern) {
        return Some(Pattern::Literal(pattern.to_string()));
    }
    let source = if is_expression(pattern) && !definition.regex {
        expression_to_regex(pattern)
    } else {
        pattern.to_string()
    };
    regex::Regex::new(&format!("^(?:{source})$"))
        .ok()
        .map(Pattern::Regex)
}

/// Cucumber expression -> anchored regex body. Literal parts are escaped, so
/// `I pay 10.5 EUR` stays literal around the placeholder.
fn expression_to_regex(pattern: &str) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        out.push_str(&regex::escape(&rest[..start]));
        out.push_str(match &rest[start + 1..start + end] {
            "int" => "(-?\\d+)",
            "float" => "(-?\\d*\\.?\\d+(?:[eE][-+]?\\d+)?)",
            "word" => "(\\S+)",
            "string" => "(\"[^\"]*\"|'[^']*')",
            _ => "(.*)",
        });
        rest = &rest[start + end + 1..];
    }
    out.push_str(&regex::escape(rest));
    out
}

/// Every step definition in the index, with its pattern compiled once.
#[derive(Debug, Default)]
pub struct StepIndex {
    definitions: Vec<(Option<Pattern>, usize, StepDefinition)>,
}

impl StepIndex {
    pub fn build(files: &[FileEntry]) -> StepIndex {
        let mut definitions = Vec::new();
        for (position, file) in files.iter().enumerate() {
            for definition in &file.step_defs {
                definitions.push((compile(definition), position, definition.clone()));
            }
        }
        StepIndex { definitions }
    }

    /// Definitions whose pattern matches `text`, as `(file position,
    /// definition)`. Several definitions may match one step; Cucumber picks by
    /// specificity, filer keeps them all and lets the caller dedupe.
    pub fn matching<'a>(&'a self, text: &str) -> Vec<(usize, &'a StepDefinition)> {
        self.definitions
            .iter()
            .filter_map(|(pattern, file, definition)| {
                let pattern = pattern.as_ref()?;
                pattern.matches(text).then_some((*file, definition))
            })
            .collect()
    }
}

/// Enclosing symbol and method of a byte offset, for pointer-free edges.
fn attribute(symbols: &[Symbol], offset: usize) -> (Option<String>, Option<String>) {
    let Some(symbol) = symbols
        .iter()
        .filter(|symbol| symbol.contains(offset))
        .min_by_key(|symbol| symbol.end - symbol.start)
    else {
        return (None, None);
    };
    let member = symbol
        .members
        .iter()
        .filter(|member| (member.start..member.end).contains(&offset))
        .max_by_key(|member| member.start)
        .map(|member| member.name.clone());
    (Some(symbol.name.clone()), member)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(pattern: &str) -> StepDefinition {
        StepDefinition {
            pattern: pattern.to_string(),
            regex: false,
            line: 1,
            symbol: None,
            member: None,
        }
    }

    fn matches(pattern: &str, text: &str) -> bool {
        compile(&definition(pattern))
            .expect("compiled")
            .matches(text)
    }

    #[test]
    fn scans_java_annotations() {
        let source = "\
class Steps {
    @Given(\"^the user (\\\\w+) is logged in$\")
    void loggedIn(String name) {}

    @When(value = \"I pay {int} EUR\")
    void pay(int cents) {}
}
";
        let mut parser = crate::lang::SourceParser::new();
        let parsed = parser
            .parse(Language::Java, source, "Steps.java")
            .expect("parsed");
        let defs = scan(Language::Java, source, &parsed.symbols);
        assert_eq!(defs.len(), 2, "{defs:?}");
        assert_eq!(defs[0].pattern, r"^the user (\w+) is logged in$");
        assert!(defs[0].regex);
        assert_eq!(defs[0].symbol.as_deref(), Some("Steps"));
        assert_eq!(defs[0].member.as_deref(), Some("loggedIn"));
        assert_eq!(defs[1].pattern, "I pay {int} EUR");
        assert!(!defs[1].regex);
    }

    #[test]
    fn scans_javascript_and_python_and_csharp() {
        let js = "Given(/^the cart has (\\d+) items$/, () => {});\nWhen('I pay', () => {});\n";
        let defs = scan(Language::TypeScript, js, &[]);
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].pattern, r"^the cart has (\d+) items$");
        assert!(defs[0].regex);
        assert_eq!(defs[1].pattern, "I pay");

        let python = "@given('the user is logged in')\ndef logged_in():\n    pass\n";
        let defs = scan(Language::Python, python, &[]);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].pattern, "the user is logged in");

        let csharp = "[Given(@\"^I have (\\d+) cukes$\")]\npublic void Have(int n) {}\n";
        let defs = scan(Language::CSharp, csharp, &[]);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].pattern, r"^I have (\d+) cukes$");
    }

    #[test]
    fn ignores_non_literal_arguments_and_plain_calls() {
        let source = "Given(MY_PATTERN);\nobject.Given(\"x\");\nMyGiven(\"y\");\n";
        assert!(scan(Language::Java, source, &[]).is_empty());
    }

    #[test]
    fn matches_regex_expressions_and_literals() {
        assert!(matches(r"^I have (\d+) cukes$", "I have 3 cukes"));
        assert!(!matches(r"^I have (\d+) cukes$", "I have 3 apples"));
        assert!(matches("I have {int} cukes", "I have 3 cukes"));
        assert!(matches("I pay {float} EUR", "I pay 10.50 EUR"));
        assert!(matches("I say {string}", "I say \"hi\""));
        assert!(matches("the user is logged in", "the user is logged in"));
        assert!(!matches("the user is logged in", "the user is logged out"));
        // A dot in a plain literal must stay literal.
        assert!(!matches("I pay 10.5 EUR", "I pay 10x5 EUR"));
    }

    #[test]
    fn empty_and_invalid_patterns_are_skipped() {
        assert!(compile(&definition("")).is_none());
        assert!(compile(&StepDefinition {
            regex: true,
            ..definition("^(unclosed$")
        })
        .is_none());
    }
}
