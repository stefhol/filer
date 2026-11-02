//! End-to-end checks for the diagnostics/coverage contract and `deps --unit`.
//!
//! These run the real binary because the contract is about stdout: `bundle
//! --json` must report what the traversal declined, and `deps --json --unit`
//! must expose the method closure the file-granular view collapses.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Every kind the closed enum may emit. Adding a kind without documenting it
/// here is a wire-format change, not an additive one.
const DIAGNOSTIC_KINDS: &[&str] = &[
    "unresolved-import",
    "unknown-receiver",
    "unresolved-call",
    "method-not-found",
    "type-only-skipped",
    "ambiguous",
];

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-diag-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("app")).unwrap();
    fs::write(
        dir.join("app/Pricing.java"),
        "package app;\n\nimport com.acme.Tax;\n\npublic class Pricing {\n    private Repo repo;\n    private Audit audit;\n\n    public int round(Bridge bridge) {\n        return repo.size() + Tax.rate() + bridge.lookup().tick();\n    }\n\n    public int unused() {\n        return repo.spare();\n    }\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("app/Repo.java"),
        "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n    public int spare() { return 1; }\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("app/Audit.java"),
        "package app;\n\npublic class Audit {\n    public void write() {}\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("app/Bridge.java"),
        "package app;\n\npublic class Bridge {\n    public Object lookup() { return null; }\n}\n",
    )
    .unwrap();
    dir
}

fn run(root: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_filer"))
        .arg("--root")
        .arg(root)
        .args(args)
        .env("XDG_CACHE_HOME", root.join(".cache"))
        .output()
        .expect("run filer");
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

fn diagnostics(value: &serde_json::Value) -> &Vec<serde_json::Value> {
    value["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("diagnostics array: {value}"))
}

fn assert_closed_kinds(value: &serde_json::Value) {
    for diagnostic in diagnostics(value) {
        let kind = diagnostic["kind"].as_str().expect("kind is a string");
        assert!(
            DIAGNOSTIC_KINDS.contains(&kind),
            "undocumented diagnostic kind {kind}: {diagnostic}"
        );
        assert!(
            diagnostic["path"].is_string(),
            "every diagnostic has a path: {diagnostic}"
        );
    }
}

#[test]
fn bundle_json_reports_unresolved_imports_in_diagnostics_and_coverage() {
    let root = project("coverage");
    let bundle = object(&run(&root, &["--json", "bundle", "app/Pricing.java"]));
    assert_closed_kinds(&bundle);

    let unresolved: Vec<&serde_json::Value> = diagnostics(&bundle)
        .iter()
        .filter(|diagnostic| diagnostic["kind"] == "unresolved-import")
        .collect();
    assert_eq!(unresolved.len(), 1, "{bundle}");
    assert_eq!(
        unresolved[0],
        &serde_json::json!({
            "kind": "unresolved-import",
            "path": "app/Pricing.java",
            "import": "com.acme.Tax",
        })
    );

    assert_eq!(bundle["coverage"]["imports_unresolved"], 1, "{bundle}");
    assert_eq!(bundle["coverage"]["files_indexed"], 4, "{bundle}");
    assert_eq!(bundle["coverage"]["files_skipped"], 0, "{bundle}");
    assert_eq!(bundle["schema_version"], 1, "{bundle}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn bundle_json_reports_the_other_declined_units() {
    let root = project("kinds");
    let bundle = object(&run(&root, &["--json", "bundle", "app/Pricing.java"]));
    assert_closed_kinds(&bundle);

    let diagnostic = |kind: &str| {
        diagnostics(&bundle)
            .iter()
            .find(|diagnostic| diagnostic["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} diagnostic: {bundle}"))
            .clone()
    };

    // A field type that is in scope but not on the call path.
    assert_eq!(
        diagnostic("type-only-skipped"),
        serde_json::json!({
            "kind": "type-only-skipped",
            "path": "app/Pricing.java",
            "symbol": "Audit",
        })
    );
    // A chained call whose receiver type the parser cannot know.
    assert_eq!(
        diagnostic("unknown-receiver"),
        serde_json::json!({
            "kind": "unknown-receiver",
            "path": "app/Pricing.java",
            "line": 10,
            "call": "tick",
        })
    );
    // An import that resolved no type, used as a receiver at the same line.
    assert_eq!(
        diagnostic("unresolved-call"),
        serde_json::json!({
            "kind": "unresolved-call",
            "path": "app/Pricing.java",
            "line": 10,
            "name": "Tax",
        })
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn deps_unit_exposes_the_method_closure_and_carries_edges() {
    let root = project("unit");
    let unit_run = object(&run(
        &root,
        &[
            "--json",
            "deps",
            "app/Pricing.java::Pricing.round",
            "--unit",
        ],
    ));

    assert_eq!(unit_run["granularity"], "unit", "{unit_run}");
    assert_eq!(
        unit_run["seed"], "app/Pricing.java::Pricing.round",
        "{unit_run}"
    );
    assert_closed_kinds(&unit_run);

    let units: Vec<&str> = unit_run["deps"]
        .as_array()
        .expect("deps")
        .iter()
        .map(|row| row["unit"].as_str().expect("unit label"))
        .collect();
    assert!(
        units.contains(&"app/Pricing.java::Pricing.round"),
        "{unit_run}"
    );
    assert!(units.contains(&"app/Repo.java::Repo.size"), "{unit_run}");
    assert!(
        units.contains(&"app/Bridge.java::Bridge.lookup"),
        "{unit_run}"
    );
    // Off the call path of `round`, so not in the method closure.
    assert!(
        !units.iter().any(|unit| unit.ends_with(".unused")),
        "{unit_run}"
    );
    assert!(
        !units.iter().any(|unit| unit.ends_with(".spare")),
        "{unit_run}"
    );

    let rows = unit_run["deps"].as_array().expect("deps");
    let seed_row = rows
        .iter()
        .find(|row| row["unit"] == "app/Pricing.java::Pricing.round")
        .expect("seed row");
    assert_eq!(seed_row["scope"], "methods", "{unit_run}");
    assert_eq!(seed_row["symbol"], "Pricing", "{unit_run}");
    assert_eq!(seed_row["member"], "round", "{unit_run}");
    assert_eq!(seed_row["reason"], "seed", "{unit_run}");

    let edges = unit_run["edges"].as_array().expect("edges");
    let seed_edge = edges
        .iter()
        .find(|edge| edge["to"] == "app/Repo.java::Repo.size")
        .unwrap_or_else(|| panic!("call edge to Repo.size: {unit_run}"));
    assert_eq!(
        seed_edge,
        &serde_json::json!({
            "from": "app/Pricing.java::Pricing.round",
            "to": "app/Repo.java::Repo.size",
            "reason": "call",
            "line": 10,
        })
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn deps_file_mode_is_the_same_traversal_collapsed_to_files() {
    let root = project("granularity");
    let unit_run = object(&run(
        &root,
        &[
            "--json",
            "deps",
            "app/Pricing.java::Pricing.round",
            "--unit",
        ],
    ));
    let file_run = object(&run(
        &root,
        &["--json", "deps", "app/Pricing.java::Pricing.round"],
    ));

    assert_eq!(file_run["granularity"], "file", "{file_run}");
    let unit_paths: BTreeSet<&str> = unit_run["deps"]
        .as_array()
        .expect("deps")
        .iter()
        .map(|row| row["path"].as_str().expect("path"))
        .collect();
    let file_paths: BTreeSet<&str> = file_run["deps"]
        .as_array()
        .expect("deps")
        .iter()
        .map(|row| row["path"].as_str().expect("path"))
        .collect();
    // Differ by unit, not by traversal: same files, one row each.
    assert_eq!(unit_paths, file_paths, "{unit_run} vs {file_run}");
    for row in file_run["deps"].as_array().expect("deps") {
        assert_eq!(row["scope"], "file", "{file_run}");
    }

    // The same seed twice: the unit list and the file list describe one
    // traversal, and only the unit list names the methods.
    assert_closed_kinds(&file_run);

    // A file seed needs no collapsing: `--unit` and the default agree.
    let file_unit = object(&run(
        &root,
        &["--json", "deps", "app/Pricing.java", "--unit"],
    ));
    let file_default = object(&run(&root, &["--json", "deps", "app/Pricing.java"]));
    assert_eq!(file_unit["deps"], file_default["deps"], "{file_unit}");

    let _ = fs::remove_dir_all(&root);
}
