//! Acceptance checks for issue #12: canonical order and a byte/token budget.
//!
//! The tests run the real binary because the contract is about stdout: the same
//! seed and budget must produce the same bundle twice, an import reorder in the
//! seed must not reorder the bundle, and shrinking the budget may only remove
//! entries from the tail — everything else is reported in `omitted`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn class(name: &str) -> String {
    format!("package app;\n\npublic class {name} {{\n    public int id() {{ return 0; }}\n}}\n")
}

/// A seed class that calls three callees through type usages, so the closure has
/// one entry per file regardless of the import order.
fn seed(imports: &[&str]) -> String {
    format!(
        "package app;\n\n{}\n\npublic class Service {{\n    private Alpha alpha;\n    private Beta beta;\n    private Gamma gamma;\n\n    int used() {{ return alpha.id() + beta.id() + gamma.id(); }}\n}}\n",
        imports.join("\n")
    )
}

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-budget-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("app")).unwrap();
    for callee in ["Alpha", "Beta", "Gamma"] {
        fs::write(dir.join(format!("app/{callee}.java")), class(callee)).unwrap();
    }
    fs::write(
        dir.join("app/Service.java"),
        seed(&["import app.Alpha;", "import app.Beta;", "import app.Gamma;"]),
    )
    .unwrap();
    dir
}

fn ok(root: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_filer"))
        .arg("--root")
        .arg(root)
        .args(args)
        .env("XDG_CACHE_HOME", root.join(".cache"))
        .output()
        .expect("run filer");
    assert!(
        output.status.success(),
        "filer {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

fn object(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim()).expect("stdout must be one JSON object")
}

/// The units in the order the envelope lists them: `depth path::symbol scope`.
fn order(value: &serde_json::Value) -> Vec<String> {
    value["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| {
            let symbol = entry["symbol"].as_str().unwrap_or("");
            format!(
                "{} {}::{symbol} {}",
                entry["depth"],
                entry["path"].as_str().unwrap_or(""),
                entry["scope"]
            )
        })
        .collect()
}

fn used_tokens(value: &serde_json::Value) -> u64 {
    value["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["tokens"].as_u64().expect("tokens"))
        .sum()
}

#[test]
fn the_same_seed_and_budget_render_the_same_bundle_twice() {
    let root = project("stable");
    let args = [
        "--json",
        "bundle",
        "--depth",
        "2",
        "--max-tokens",
        "1000",
        "app/Service.java",
    ];
    let first = ok(&root, &args);
    let second = ok(&root, &args);
    assert_eq!(first, second);
    let value = object(&first);
    assert_eq!(value["budget"]["max_tokens"], 1000);
    assert_eq!(value["budget"]["truncated"], false);
    assert_eq!(value["budget"]["used_tokens"], used_tokens(&value));
    assert!(order(&value).len() >= 2, "{value}");
}

#[test]
fn reordering_imports_in_the_seed_does_not_reorder_the_bundle() {
    let root = project("reorder");
    let args = ["--json", "bundle", "--depth", "2", "app/Service.java"];
    let before = object(&ok(&root, &args));
    assert!(order(&before).len() >= 4, "{before}");

    // Reverse the imports and add a comment, so both the discovery order and the
    // index cache key (mtime + size) change.
    fs::write(
        root.join("app/Service.java"),
        seed(&[
            "import app.Gamma;",
            "import app.Beta;",
            "import app.Alpha;",
            "// importer order changed",
        ]),
    )
    .unwrap();
    let after = object(&ok(&root, &args));

    assert_eq!(order(&before), order(&after));
}

#[test]
fn a_budget_only_cuts_the_tail_and_lists_the_dropped_units() {
    let root = project("tail");
    let args = ["--json", "bundle", "--depth", "2", "app/Service.java"];
    let full = object(&ok(&root, &args));
    let baseline = order(&full);
    let total: u64 = full["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["tokens"].as_u64().expect("tokens"))
        .sum();
    assert!(baseline.len() >= 4, "{full}");
    assert!(total > 0, "{full}");

    // A larger budget never keeps fewer entries.
    let mut previous = 0;
    for tokens in (0..=total).step_by(3) {
        let budget = tokens.to_string();
        let value = object(&ok(
            &root,
            &[
                "--json",
                "bundle",
                "--depth",
                "2",
                "--max-tokens",
                &budget,
                "app/Service.java",
            ],
        ));
        let kept = order(&value);
        assert_eq!(
            kept,
            baseline[..kept.len()],
            "--max-tokens {budget} must keep a prefix of the canonical order"
        );
        assert!(kept.len() >= previous, "--max-tokens {budget}");
        previous = kept.len();

        assert_eq!(value["budget"]["max_tokens"], serde_json::json!(tokens));
        assert_eq!(value["budget"]["used_tokens"], used_tokens(&value));
        // The budget is exact in bytes, and the token form is derived: a
        // `--max-tokens N` budget is `N * 4` source bytes.
        assert!(value["budget"]["used_bytes"].as_u64().unwrap() <= tokens * 4);
        assert!(used_tokens(&value) <= tokens);

        let omitted = value["omitted"].as_array().expect("omitted");
        assert_eq!(kept.len() + omitted.len(), baseline.len());
        assert!(omitted.iter().all(|row| row["reason"] == "budget"));
        assert_eq!(value["budget"]["truncated"], !omitted.is_empty());

        // Budget drops are not in `entries`, so they are not in the markdown
        // either: one `###` section per kept entry, no placeholder text.
        let text = ok(
            &root,
            &["bundle", "--max-tokens", &budget, "app/Service.java"],
        );
        assert_eq!(text.matches("\n### ").count(), kept.len(), "{text}");
    }

    // `--max-bytes` is the primitive; `--max-tokens` is the same budget at four
    // bytes per token.
    let by_tokens = ok(
        &root,
        &["--json", "bundle", "--max-tokens", "12", "app/Service.java"],
    );
    let by_bytes = ok(
        &root,
        &["--json", "bundle", "--max-bytes", "48", "app/Service.java"],
    );
    assert_eq!(by_tokens, by_bytes);
}
