//! Acceptance checks for issue #11: spans, per-unit hashes, `resolve` and
//! `locate`.
//!
//! The tests run the real binary because the contract is about stdout: a FQN
//! resolves to a span, a line inside that span locates the same unit, and the
//! hash changes when the source does.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SOURCE: &str = "package app;\n\npublic class Pricing {\n    private int base = 10;\n\n    public int discount(int qty) {\n        return base * qty;\n    }\n\n    public int total() {\n        return base;\n    }\n}\n";

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-spans-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("app")).unwrap();
    fs::write(dir.join("app/Pricing.java"), SOURCE).unwrap();
    dir
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_filer"))
        .arg("--root")
        .arg(root)
        .args(args)
        .env("XDG_CACHE_HOME", root.join(".cache"))
        .output()
        .expect("run filer")
}

fn ok(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "filer {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

fn object(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim()).expect("stdout must be one JSON object")
}

#[test]
fn resolve_and_locate_return_the_same_unit() {
    let root = project("agree");
    let resolved = object(&ok(&root, &["--json", "resolve", "app.Pricing.discount"]));
    assert_eq!(resolved["schema_version"], 1, "{resolved}");
    assert_eq!(resolved["path"], "app/Pricing.java", "{resolved}");
    assert_eq!(resolved["scope"], "methods", "{resolved}");
    assert_eq!(resolved["symbol"], "Pricing", "{resolved}");
    assert_eq!(resolved["member"], "discount", "{resolved}");
    assert_eq!(resolved["granularity"], "member", "{resolved}");
    assert_eq!(resolved["start_line"], 6, "{resolved}");
    assert_eq!(resolved["end_line"], 8, "{resolved}");
    let hash = resolved["hash"].as_str().expect("hash");
    assert_eq!(hash.len(), 64, "{resolved}");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{resolved}");

    let located = object(&ok(
        &root,
        &["--json", "locate", "--at-line", "7", "app/Pricing.java"],
    ));
    assert_eq!(located["schema_version"], 1, "{located}");
    for field in [
        "path",
        "scope",
        "symbol",
        "member",
        "start_line",
        "end_line",
        "hash",
        "granularity",
    ] {
        assert_eq!(
            located[field], resolved[field],
            "field {field}: {located} vs {resolved}"
        );
    }
}

#[test]
fn editing_the_file_changes_the_hash() {
    let root = project("hash");
    let before = object(&ok(&root, &["--json", "resolve", "app.Pricing.discount"]));
    let edited = SOURCE.replace("return base * qty;", "return base * qty + 1;");
    fs::write(root.join("app/Pricing.java"), edited).unwrap();
    let after = object(&ok(&root, &["--json", "resolve", "app.Pricing.discount"]));
    assert_ne!(before["hash"], after["hash"], "{before} vs {after}");
}

#[test]
fn search_rows_carry_span_and_budget_metadata() {
    let root = project("budget");
    let state = root.join(".filer/bundle.toml");
    let rows: Vec<serde_json::Value> = ok(&root, &["--json", "search", "--query", "Pricing"])
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(object)
        .collect();
    assert!(!rows.is_empty(), "search returned no rows");
    for row in &rows {
        for field in ["start_line", "end_line", "bytes", "est_tokens"] {
            assert!(!row[field].is_null(), "missing {field}: {row}");
        }
    }

    let class_row = rows
        .iter()
        .find(|row| row["scope"] == "symbol" && row["fqn"] == "app.Pricing")
        .expect("class row");
    let bundle = object(&ok(
        &root,
        &[
            "--json",
            "bundle",
            "--depth",
            "0",
            "--state",
            state.to_str().unwrap(),
            "app/Pricing.java::Pricing",
        ],
    ));
    let entry = bundle["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["scope"] == "symbol" && entry["path"] == "app/Pricing.java")
        .expect("class bundle entry");
    assert_eq!(
        class_row["est_tokens"], entry["tokens"],
        "{class_row} vs {entry}"
    );
    let hash = entry["hash"].as_str().expect("bundle entry hash");
    assert_eq!(hash.len(), 64, "{entry}");
}

#[test]
fn locate_outside_any_unit_exits_2() {
    let root = project("outside");
    for line in ["0", "2", "99"] {
        let output = run(
            &root,
            &["--json", "locate", "--at-line", line, "app/Pricing.java"],
        );
        assert_eq!(
            output.status.code(),
            Some(2),
            "line {line}, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("\"code\":\"unit_not_found\""), "{stderr}");
    }
}

#[test]
fn resolve_unknown_fqn_exits_2() {
    let root = project("unknown");
    let output = run(&root, &["--json", "resolve", "app.Pricing.nope"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"code\":\"unit_not_found\""), "{stderr}");
}
