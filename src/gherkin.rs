//! Cucumber/Gherkin parsing, by hand.
//!
//! A `.feature` file is line oriented: `Feature:`, `Rule:`, `Background:`,
//! `Scenario:`/`Scenario Outline:`, one `Given`/`When`/`Then`/`And`/`But` per
//! step, and `| … |` rows under `Examples:`. The English keywords plus the `*`
//! step marker cover what filer needs, and skipping a grammar keeps the
//! dependency tree as small as it is for the other formats.
//!
//! Scenarios are the bundling unit: one symbol per scenario, and one call per
//! step of that scenario (a `Background` is attributed to every scenario in its
//! scope, because that is how it runs). A `.feature` file has no imports. What
//! makes it worth bundling is the step text: [`crate::resolve`] matches it
//! against the step definitions the index collected from the code that
//! implements them (`@Given`, `Given(...)`, ...).

use crate::lang::{finish_symbols, Call, Receiver, Source, Symbol, SymbolKind};

/// Step keywords as they are written. A line is checked in this order.
const STEP_KEYWORDS: [&str; 6] = ["Given ", "When ", "Then ", "And ", "But ", "* "];

/// A step of a background or scenario: its text and 1-based line.
type Step = (String, usize);

struct ScenarioBody {
    symbol: Symbol,
    /// `(header, rows)` per `Examples:` block, for outline placeholder
    /// expansion.
    examples: Vec<(Vec<String>, Vec<Vec<String>>)>,
}

/// Steps of the background block currently being read, and whether it belongs
/// to a `Rule` (otherwise to the feature).
struct PendingBackground {
    in_rule: bool,
    steps: Vec<Step>,
}

pub fn parse(src: &str) -> Option<Source> {
    let lines: Vec<Line<'_>> = lines(src).collect();
    let mut out = Source::default();

    let mut feature_background: Vec<Step> = Vec::new();
    let mut rule_background: Vec<Step> = Vec::new();
    let mut pending: Option<PendingBackground> = None;
    let mut scenario: Option<ScenarioBody> = None;
    let mut in_rule = false;

    for line in &lines {
        let trimmed = line.text.trim_start();
        let indent = line.text.len() - trimmed.len();
        let text = trimmed.trim_end();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        if let Some(target) = declaration(text) {
            // `Examples:` belongs to the open scenario; every other declaration
            // ends it.
            if matches!(target, Declaration::Examples) {
                if let Some(open) = scenario.as_mut() {
                    open.examples.push((Vec::new(), Vec::new()));
                    open.symbol.end = line.end();
                    open.symbol.end_line = line.number;
                }
                continue;
            }
            flush(
                &mut scenario,
                &mut out,
                &mut pending,
                &mut feature_background,
                &mut rule_background,
            );
            match target {
                Declaration::Feature => {
                    in_rule = false;
                    feature_background.clear();
                    rule_background.clear();
                }
                Declaration::Rule => {
                    in_rule = true;
                    rule_background.clear();
                }
                Declaration::Background => {
                    pending = Some(PendingBackground {
                        in_rule,
                        steps: Vec::new(),
                    });
                }
                Declaration::Scenario => {
                    // A scenario inside a rule runs the feature background and
                    // then the rule background.
                    let mut steps = feature_background.clone();
                    steps.extend(rule_background.iter().cloned());
                    scenario = Some(ScenarioBody {
                        symbol: symbol(text, line, indent, steps),
                        examples: Vec::new(),
                    });
                }
                Declaration::Examples => unreachable!("handled above"),
            }
            continue;
        }
        if text.starts_with('@') {
            continue;
        }
        if let Some(row) = table_row(text) {
            if let Some(open) = scenario.as_mut() {
                open.symbol.end = line.end();
                open.symbol.end_line = line.number;
                if let Some((header, rows)) = open.examples.last_mut() {
                    if header.is_empty() {
                        *header = row;
                    } else {
                        rows.push(row);
                    }
                }
            }
            continue;
        }
        if let Some(step) = step_text(text) {
            if let Some(pending) = pending.as_mut() {
                pending.steps.push((step, line.number));
            } else if let Some(open) = scenario.as_mut() {
                open.symbol.calls.push(Call {
                    name: step,
                    receiver: Receiver::Bare,
                    line: line.number,
                });
                open.symbol.end = line.end();
                open.symbol.end_line = line.number;
            }
            continue;
        }
        // Feature/rule/scenario description lines are not steps, but they do
        // belong to the open scenario's span.
        if let Some(open) = scenario.as_mut() {
            open.symbol.end = line.end();
            open.symbol.end_line = line.number;
        }
    }
    flush(
        &mut scenario,
        &mut out,
        &mut pending,
        &mut feature_background,
        &mut rule_background,
    );

    out.symbols.sort_by_key(|symbol| symbol.start);
    // Gherkin declares no names other files could import, so there are no uses
    // to attribute; this only fills in `preamble_end` (the feature header
    // repeated in front of each scenario excerpt).
    finish_symbols(&mut out, &[]);

    // Never `None`: an invalid feature is a file without scenarios, not an
    // unreadable one.
    Some(out)
}

enum Declaration {
    Feature,
    Rule,
    Background,
    Scenario,
    Examples,
}

fn declaration(text: &str) -> Option<Declaration> {
    if text.starts_with("Feature:") {
        return Some(Declaration::Feature);
    }
    if text.starts_with("Rule:") {
        return Some(Declaration::Rule);
    }
    if text.starts_with("Background:") {
        return Some(Declaration::Background);
    }
    if text.starts_with("Scenario:")
        || text.starts_with("Scenario Outline:")
        || text.starts_with("Scenario Template:")
    {
        return Some(Declaration::Scenario);
    }
    if text.starts_with("Examples:") || text.starts_with("Scenarios:") {
        return Some(Declaration::Examples);
    }
    None
}

fn symbol(text: &str, line: &Line<'_>, indent: usize, background: Vec<Step>) -> Symbol {
    let name = text
        .split_once(':')
        .map(|(_, name)| name.trim().to_string())
        .unwrap_or_default();
    Symbol {
        name: if name.is_empty() {
            text.to_string()
        } else {
            name
        },
        kind: SymbolKind::Function,
        start: line.start + indent,
        end: line.end(),
        line: line.number,
        end_line: line.number,
        uses: Vec::new(),
        calls: background
            .into_iter()
            .map(|(name, line)| Call {
                name,
                receiver: Receiver::Bare,
                line,
            })
            .collect(),
        members: Vec::new(),
        fields: Vec::new(),
        supertypes: Vec::new(),
        params: Vec::new(),
        locals: Vec::new(),
    }
}

/// Ends the open scenario and the open background block. Background steps count
/// for the container they sit in: the feature or the current `Rule`.
fn flush(
    scenario: &mut Option<ScenarioBody>,
    out: &mut Source,
    pending: &mut Option<PendingBackground>,
    feature_background: &mut Vec<Step>,
    rule_background: &mut Vec<Step>,
) {
    if let Some(pending) = pending.take() {
        if pending.in_rule {
            *rule_background = pending.steps;
        } else {
            *feature_background = pending.steps;
        }
    }
    let Some(mut open) = scenario.take() else {
        return;
    };
    let mut calls = Vec::new();
    for call in &open.symbol.calls {
        for text in expand(&call.name, &open.examples) {
            calls.push(Call {
                name: text,
                receiver: Receiver::Bare,
                line: call.line,
            });
        }
    }
    calls.sort_by(|left, right| {
        left.line
            .cmp(&right.line)
            .then_with(|| left.name.cmp(&right.name))
    });
    calls.dedup_by(|left, right| left.line == right.line && left.name == right.name);
    open.symbol.calls = calls;
    open.symbol.end_line = open.symbol.end_line.max(open.symbol.line);
    open.symbol.end = open.symbol.end.max(open.symbol.start + 1);
    out.symbols.push(open.symbol);
}

/// `I add <count> items` becomes one call per example row (`I add 3 items`).
/// A scenario without an examples table keeps the raw text.
fn expand(text: &str, examples: &[(Vec<String>, Vec<Vec<String>>)]) -> Vec<String> {
    if !text.contains('<') {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    for (header, rows) in examples {
        for row in rows {
            let mut expanded = text.to_string();
            for (name, value) in header.iter().zip(row.iter()) {
                expanded = expanded.replace(&format!("<{name}>"), value);
            }
            out.push(expanded);
        }
    }
    if out.is_empty() {
        out.push(text.to_string());
    }
    out.sort();
    out.dedup();
    out
}

/// A step line: its keyword stripped, or `None` for anything else.
fn step_text(text: &str) -> Option<String> {
    for keyword in STEP_KEYWORDS {
        if let Some(rest) = text.strip_prefix(keyword) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// `| a | b |` -> `["a", "b"]`, and `None` when the line is not a table row.
fn table_row(text: &str) -> Option<Vec<String>> {
    let inner = text.strip_prefix('|')?.strip_suffix('|')?;
    Some(
        inner
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect(),
    )
}

struct Line<'a> {
    number: usize,
    start: usize,
    text: &'a str,
}

impl Line<'_> {
    fn end(&self) -> usize {
        self.start + self.text.len()
    }
}

fn lines(src: &str) -> impl Iterator<Item = Line<'_>> {
    let mut start = 0;
    let mut number = 0;
    std::iter::from_fn(move || {
        if start >= src.len() {
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
    use crate::lang::{end_line_of, line_of};

    const FEATURE: &str = "\
@login
Feature: Login
  As a user I want to log in

  Background:
    Given the site is up

  Scenario: valid credentials
    Given the user is logged in
    When I open the dashboard
    Then I see 3 widgets

  Rule: locked accounts
    Background:
      Given the account is locked

    Scenario Outline: locked login
      When I try <count> times
      Then I am blocked

      Examples:
        | count |
        | 1     |
        | 4     |
";

    fn names(source: &Source, index: usize) -> Vec<String> {
        let mut calls: Vec<String> = source.symbols[index]
            .calls
            .iter()
            .map(|call| call.name.clone())
            .collect();
        calls.sort();
        calls.dedup();
        calls
    }

    #[test]
    fn scenarios_become_symbols() {
        let source = parse(FEATURE).expect("parsed");
        let names: Vec<&str> = source
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(names, ["valid credentials", "locked login"]);
        let locked = &source.symbols[1];
        assert_eq!(locked.kind, SymbolKind::Function);
        assert_eq!(locked.line, 17);
        assert!(locked.line < locked.end_line);
        assert!(locked.end > locked.start);
        assert!(locked.end <= FEATURE.len());
    }

    #[test]
    fn background_and_outline_steps_are_calls() {
        let source = parse(FEATURE).expect("parsed");
        // Every scenario runs the feature background; the first one has no
        // rule background yet.
        let first = names(&source, 0);
        assert!(first.contains(&"the site is up".to_string()), "{first:?}");
        assert!(first.contains(&"the user is logged in".to_string()));
        assert!(!first.contains(&"the account is locked".to_string()));
        // A scenario inside a rule runs both backgrounds, and outline
        // placeholders are expanded for every example row.
        let locked = names(&source, 1);
        assert!(locked.contains(&"the site is up".to_string()), "{locked:?}");
        assert!(locked.contains(&"the account is locked".to_string()));
        assert!(locked.contains(&"I try 1 times".to_string()), "{locked:?}");
        assert!(locked.contains(&"I try 4 times".to_string()));
        assert!(locked.contains(&"I am blocked".to_string()));
    }

    #[test]
    fn spans_match_the_source_lines() {
        let source = parse(FEATURE).expect("parsed");
        for symbol in &source.symbols {
            assert!(symbol.start < symbol.end, "{}", symbol.name);
            assert!(symbol.end <= FEATURE.len(), "{}", symbol.name);
            assert_eq!(line_of(FEATURE, symbol.start), symbol.line);
            assert!(
                symbol.end_line >= end_line_of(FEATURE, symbol.start, symbol.end),
                "{}",
                symbol.name
            );
        }
        // The excerpts do not overlap.
        assert!(source.symbols[0].end < source.symbols[1].start);
    }

    #[test]
    fn non_gherkin_text_has_no_symbols() {
        let source = parse("this is not gherkin\n").expect("parsed");
        assert!(source.symbols.is_empty());
    }

    #[test]
    fn step_keywords_are_stripped() {
        assert_eq!(
            step_text("Given the site is up"),
            Some("the site is up".into())
        );
        assert_eq!(step_text("* a bullet step"), Some("a bullet step".into()));
        assert_eq!(step_text("Given:"), None);
        assert_eq!(step_text("# a comment"), None);
    }
}
