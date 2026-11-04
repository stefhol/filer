//! Robot Framework parsing (`.robot` suites, `.resource` resource files).
//!
//! Robot has no tree-sitter grammar on crates.io, and it does not need one: a
//! suite is a line-oriented file with `*** Section ***` headers, one
//! declaration per unindented line inside a section, and one keyword call per
//! indented line of a test or keyword body.
//!
//! What the index gets out of it:
//!
//! - one [`Symbol`] per test case/task and per user keyword, spanning the body,
//! - `Resource`/`Variables`/`Library` settings as [`Import`]s (relative paths,
//!   [`ImportKind::Resource`](crate::lang::ImportKind::Resource) for the ones
//!   whose keywords enter scope),
//! - one [`Call`] per keyword call in a test or keyword body.

use crate::lang::{finish_symbols, Call, Import, ImportKind, Receiver, Source, Symbol, SymbolKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Settings,
    Variables,
    Tests,
    Keywords,
    Comments,
}

/// Parses a Robot suite or resource file. `rel` is unused today: `Resource`
/// paths are resolved relative to the file by the resolver, which knows the
/// path anyway.
pub fn parse(_rel: &str, src: &str) -> Option<Source> {
    let mut out = Source::default();
    let lines: Vec<Line<'_>> = lines(src).collect();
    let mut section: Option<Section> = None;
    // Symbol currently being collected: its index plus the byte offset and
    // line of its last non-blank body line, so it can be closed when the next
    // declaration starts.
    let mut open: Option<(usize, usize, usize)> = None;

    for line in &lines {
        let trimmed = line.text.trim_start();
        let indent = line.text.len() - trimmed.len();
        let trimmed = trimmed.trim_end();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("...") {
            continue;
        }
        if let Some(next) = section_of(trimmed) {
            close(&mut open, &mut out);
            section = Some(next);
            continue;
        }
        let Some(current) = section else {
            continue;
        };
        if indent == 0 {
            close(&mut open, &mut out);
            match current {
                Section::Tests | Section::Keywords => {
                    out.symbols.push(Symbol {
                        name: trimmed.to_string(),
                        kind: SymbolKind::Function,
                        start: line.start + indent,
                        end: line.end(),
                        line: line.number,
                        end_line: line.number,
                        uses: Vec::new(),
                        calls: Vec::new(),
                        members: Vec::new(),
                        fields: Vec::new(),
                        supertypes: Vec::new(),
                        params: Vec::new(),
                        locals: Vec::new(),
                    });
                    open = Some((out.symbols.len() - 1, line.end(), line.number));
                }
                Section::Settings => collect_setting(trimmed, &mut out),
                Section::Variables | Section::Comments => {}
            }
            continue;
        }
        // Indented line inside a test/keyword body.
        if let Some((_, end, end_line)) = open.as_mut() {
            *end = line.end();
            *end_line = line.number;
        }
        if let Some(name) = call_name(trimmed) {
            if let Some((index, _, _)) = open {
                out.symbols[index].calls.push(Call {
                    name,
                    receiver: Receiver::Bare,
                    line: line.number,
                });
            }
        }
    }
    close(&mut open, &mut out);

    // A file with no declarations (a bare `*** Settings ***`) still has its
    // imports; `finish_symbols` only fixes up `preamble_end`.
    finish_symbols(&mut out, &[]);
    Some(out)
}

/// Ends the open symbol at the last non-blank line seen, so a body never
/// carries the blank lines that separate it from the next declaration.
fn close(open: &mut Option<(usize, usize, usize)>, out: &mut Source) {
    if let Some((index, end, end_line)) = open.take() {
        let symbol = &mut out.symbols[index];
        if end > symbol.start {
            symbol.end = end;
            symbol.end_line = end_line.max(symbol.line);
        }
    }
}

/// `*** Settings ***`, `*Setting`, `*** Keywords ***`, ... -> section.
fn section_of(trimmed: &str) -> Option<Section> {
    if !trimmed.starts_with('*') {
        return None;
    }
    let name = trimmed.trim_matches('*').trim().to_ascii_lowercase();
    let name = name.split_whitespace().collect::<Vec<_>>();
    match name.first().copied()? {
        "setting" | "settings" => Some(Section::Settings),
        "variable" | "variables" => Some(Section::Variables),
        "test" | "tests" | "task" | "tasks" => Some(Section::Tests),
        "keyword" | "keywords" => Some(Section::Keywords),
        "comment" | "comments" => Some(Section::Comments),
        _ => None,
    }
}

/// Settings that import another file: `Resource`, `Variables`, `Library`.
fn collect_setting(trimmed: &str, out: &mut Source) {
    let cells = cells(trimmed);
    let Some((_, setting)) = cells.first() else {
        return;
    };
    let kind = match setting.to_ascii_lowercase().as_str() {
        "resource" => ImportKind::Resource,
        "variables" => ImportKind::Path,
        "library" => ImportKind::Path,
        _ => return,
    };
    // `Library` takes one name; `Resource`/`Variables` take the first value and
    // then optional arguments.
    let Some((_, value)) = cells.get(1) else {
        return;
    };
    if value.is_empty() || contains_variable(value) {
        return;
    }
    if out
        .imports
        .iter()
        .any(|import| import.kind == kind && import.path == *value)
    {
        return;
    }
    out.imports.push(Import {
        path: value.to_string(),
        kind,
    });
}

/// `${CURDIR}/x.resource`, `%{TMP}`, `@{dirs}` cannot be resolved statically.
fn contains_variable(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes
        .windows(2)
        .any(|window| matches!(window[0], b'$' | b'@' | b'&' | b'%') && window[1] == b'{')
}

/// The keyword a body line calls, ignoring assignments, `[Setting]` lines and
/// control structures (`FOR`, `IF`, `TRY`, ...).
fn call_name(trimmed: &str) -> Option<String> {
    let cells = cells(trimmed);
    let mut cells = cells.iter().map(|(_, cell)| *cell);
    loop {
        let cell = cells.next()?;
        if cell.starts_with('[') {
            let setting = cell.to_ascii_lowercase();
            // `[Setup]`, `[Teardown]` and `[Template]` name a keyword; the rest
            // (`[Documentation]`, `[Tags]`, `[Arguments]`, ...) do not.
            if matches!(
                setting.trim_matches(['[', ']']).trim(),
                "setup" | "teardown" | "template"
            ) {
                return cells.next().map(|name| name.to_string());
            }
            return None;
        }
        if is_assignment(cell) {
            continue;
        }
        if is_control(cell) {
            return None;
        }
        return Some(cell.to_string());
    }
}

/// `${value} =`, `@{list}`, `&{dict} =`: leading cells that assign, not call.
fn is_assignment(cell: &str) -> bool {
    let cell = cell.trim_end_matches('=').trim_end();
    let bytes = cell.as_bytes();
    bytes.len() > 3
        && matches!(bytes[0], b'$' | b'@' | b'&')
        && bytes[1] == b'{'
        && cell.ends_with('}')
}

/// Built-in control words; their arguments are not keyword calls.
fn is_control(cell: &str) -> bool {
    matches!(
        cell.to_ascii_uppercase().trim_start_matches(':').trim(),
        "FOR"
            | "END"
            | "IF"
            | "ELSE"
            | "ELSE IF"
            | "WHILE"
            | "TRY"
            | "EXCEPT"
            | "FINALLY"
            | "BREAK"
            | "CONTINUE"
            | "RETURN"
            | "GROUP"
    )
}

/// Robot cells are separated by a tab or at least two spaces.
fn cells(text: &str) -> Vec<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        let separator =
            bytes[index] == b'\t' || (bytes[index] == b' ' && bytes.get(index + 1) == Some(&b' '));
        if !separator {
            index += 1;
            continue;
        }
        if start < index {
            out.push((start, text[start..index].trim_end()));
        }
        while index < bytes.len() && (bytes[index] == b'\t' || bytes[index] == b' ') {
            index += 1;
        }
        start = index;
    }
    if start < bytes.len() {
        out.push((start, text[start..].trim_end()));
    }
    out
}

struct Line<'a> {
    number: usize,
    start: usize,
    text: &'a str,
}

impl Line<'_> {
    /// Byte offset one past the last character of the line, newline excluded.
    fn end(&self) -> usize {
        self.start + self.text.len()
    }
}

fn lines(src: &str) -> impl Iterator<Item = Line<'_>> {
    let mut start = 0;
    let mut number = 0;
    std::iter::from_fn(move || {
        if start > src.len() {
            return None;
        }
        let end = src[start..]
            .find('\n')
            .map(|offset| start + offset)
            .unwrap_or(src.len());
        let text = src[start..end].trim_end_matches('\r');
        let line = Line {
            number: number + 1,
            start,
            text,
        };
        number += 1;
        start = end + 1;
        Some(line)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUITE: &str = "\
*** Settings ***
Library           SeleniumLibrary
Resource          ../resources/common.resource
Variables         vars.py
Suite Setup       Open Browser To Login Page

*** Variables ***
${URL}            https://example.test

*** Test Cases ***
Valid Login
    [Documentation]    A user can log in.
    [Setup]           Reset Session
    ${title}=         Open Login Page
    Input Username    alice
    Click Button      Login
    Title Should Be   Dashboard

Locked Login
    FOR    ${i}    IN    1    2
        Try Login    ${i}
    END
    Sleep    1s
";

    #[test]
    fn test_cases_and_keywords_are_symbols() {
        let source = parse("tests/login.robot", SUITE).expect("parsed");
        let names: Vec<&str> = source
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(names, ["Valid Login", "Locked Login"]);
        assert_eq!(source.symbols[0].line, 11);
        assert!(source.symbols[0].end > source.symbols[0].start);
    }

    #[test]
    fn settings_become_imports() {
        let source = parse("tests/login.robot", SUITE).expect("parsed");
        let imports: Vec<(String, ImportKind)> = source
            .imports
            .iter()
            .map(|import| (import.path.clone(), import.kind))
            .collect();
        assert_eq!(
            imports,
            [
                ("SeleniumLibrary".to_string(), ImportKind::Path),
                (
                    "../resources/common.resource".to_string(),
                    ImportKind::Resource
                ),
                ("vars.py".to_string(), ImportKind::Path),
            ]
        );
    }

    #[test]
    fn bodies_record_keyword_calls() {
        let source = parse("tests/login.robot", SUITE).expect("parsed");
        let calls = |index: usize| -> Vec<String> {
            source.symbols[index]
                .calls
                .iter()
                .map(|call| call.name.clone())
                .collect()
        };
        assert_eq!(
            calls(0),
            [
                "Reset Session",
                "Open Login Page",
                "Input Username",
                "Click Button",
                "Title Should Be",
            ]
        );
        // `FOR` and `END` are not keyword calls.
        assert_eq!(calls(1), ["Try Login", "Sleep"]);
    }

    #[test]
    fn keyword_bodies_are_symbols() {
        let source = parse(
            "resources/common.resource",
            "*** Keywords ***\nOpen Login Page\n    [Arguments]    ${url}\n    Go To    ${url}\n",
        )
        .expect("parsed");
        assert_eq!(source.symbols.len(), 1);
        assert_eq!(source.symbols[0].name, "Open Login Page");
        assert_eq!(source.symbols[0].calls.len(), 1);
        assert_eq!(source.symbols[0].calls[0].name, "Go To");
    }

    #[test]
    fn cells_split_on_tabs_and_double_spaces() {
        let cells = cells("    ${x}=  Keyword\targ  two spaces");
        let values: Vec<&str> = cells.iter().map(|(_, cell)| *cell).collect();
        assert_eq!(values, ["${x}=", "Keyword", "arg", "two spaces"]);
    }
}
