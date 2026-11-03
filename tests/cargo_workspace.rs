//! End-to-end checks for Cargo workspace resolution.
//!
//! A workspace member is intra-repo: `use a::sub::Thing` from `crates/b` must
//! reach `crates/a/src/sub.rs`, a crate directory must be a seed, and an
//! incomplete closure must say so even with `--quiet`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("filer-cargo-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let files: &[(&str, &str)] = &[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        ),
        ("crates/a/src/lib.rs", "pub mod sub;\n\npub struct Root;\n"),
        ("crates/a/src/sub.rs", "pub struct Thing;\n"),
        (
            "crates/b/Cargo.toml",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\n",
        ),
        // `crates/b/src/a.rs` exists on purpose: a wrong look under the
        // importing crate's own root would surface it in the closure.
        ("crates/b/src/a.rs", "pub struct Shadow;\n"),
        (
            "crates/b/src/lib.rs",
            "use a::sub::Thing;\nuse a::Root;\nuse std::fmt::Debug;\nuse serde::Serialize;\n\npub fn run(_thing: Thing) -> Root {\n    Root\n}\n",
        ),
    ];
    for (rel, text) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("nested file")).unwrap();
        fs::write(path, text).unwrap();
    }
    root
}

fn run(root: &Path, args: &[&str]) -> (String, String) {
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
    (
        String::from_utf8(output.stdout).expect("utf-8 stdout"),
        String::from_utf8(output.stderr).expect("utf-8 stderr"),
    )
}

fn object(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim()).expect("stdout must be one JSON object")
}

#[test]
fn deps_resolves_workspace_members_by_cargo_layout() {
    let root = workspace("deps");
    let (stdout, _stderr) = run(
        &root,
        &["--json", "deps", "crates/b/src/lib.rs", "--depth", "1"],
    );
    let value = object(&stdout);
    let rows = value["deps"].as_array().expect("deps array");
    let row = |path: &str| {
        rows.iter()
            .find(|row| row["path"] == path)
            .unwrap_or_else(|| panic!("{path} missing: {stdout}"))
    };

    assert_eq!(row("crates/b/src/lib.rs")["reason"], "seed");
    assert_eq!(row("crates/b/src/lib.rs")["depth"], 0);
    // The dependencies of the fixture are imports, not name-usage luck.
    assert_eq!(row("crates/a/src/lib.rs")["reason"], "import");
    assert_eq!(row("crates/a/src/lib.rs")["depth"], 1);
    assert_eq!(row("crates/a/src/sub.rs")["reason"], "import");
    assert_eq!(row("crates/a/src/sub.rs")["depth"], 1);
    assert!(
        !rows.iter().any(|row| row["path"] == "crates/b/src/a.rs"),
        "never guessed from the importing crate's own root: {stdout}"
    );

    assert_eq!(value["seed"], "crates/b/src/lib.rs");
    assert_eq!(value["seeds"], serde_json::json!(["crates/b/src/lib.rs"]));

    // Only third-party imports stay unresolved; nothing in the workspace does.
    let unresolved: Vec<&str> = value["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .filter(|diagnostic| diagnostic["kind"] == "unresolved-import")
        .map(|diagnostic| diagnostic["import"].as_str().expect("import string"))
        .collect();
    let mut unresolved = unresolved;
    unresolved.sort_unstable();
    assert_eq!(unresolved, vec!["serde.Serialize", "std.fmt.Debug"]);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn incomplete_closure_is_grouped_and_survives_quiet() {
    let root = workspace("quiet");
    let (_stdout, stderr) = run(&root, &["deps", "crates/b/src/lib.rs", "--depth", "1"]);
    assert!(
        stderr
            .contains("closure incomplete: 2 unresolved imports: all third-party (serde 1, std 1)"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "unresolved crate 'std' (crates/b/src/lib.rs): not a member of this Cargo workspace"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("JDK/third-party"), "{stderr}");

    // --quiet drops the per-import detail but not the fact that the closure is
    // partial: a partial closure that looks complete is the bug this fixes.
    let (_stdout, quiet) = run(
        &root,
        &["--quiet", "deps", "crates/b/src/lib.rs", "--depth", "1"],
    );
    assert!(
        quiet.contains("closure incomplete: 2 unresolved imports"),
        "{quiet}"
    );
    assert!(!quiet.contains("unresolved crate"), "{quiet}");

    // A file seed in `bundle` follows the call graph (by design), but the
    // incomplete closure is still reported.
    let (stdout, stderr) = run(&root, &["bundle", "crates/b/src/lib.rs", "--depth", "1"]);
    assert!(stdout.contains("crates/b/src/lib.rs"), "{stdout}");
    assert!(
        stderr.contains("closure incomplete: 2 unresolved imports"),
        "{stderr}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_crate_directory_seeds_every_source_of_the_crate() {
    let root = workspace("seed");
    let (stdout, _stderr) = run(&root, &["--json", "deps", "crates/b", "--depth", "0"]);
    let value = object(&stdout);
    let paths: Vec<&str> = value["deps"]
        .as_array()
        .expect("deps array")
        .iter()
        .map(|row| row["path"].as_str().expect("path"))
        .collect();
    // `src/lib.rs` first, then the rest of the crate in path order.
    assert_eq!(
        paths,
        vec!["crates/b/src/lib.rs", "crates/b/src/a.rs"],
        "{stdout}"
    );
    assert_eq!(
        value["seeds"],
        serde_json::json!(["crates/b/src/lib.rs", "crates/b/src/a.rs"])
    );

    let (bundle, _stderr) = run(&root, &["bundle", "crates/a", "--depth", "0"]);
    assert!(bundle.contains("crates/a/src/lib.rs"), "{bundle}");
    assert!(bundle.contains("crates/a/src/sub.rs"), "{bundle}");

    let _ = fs::remove_dir_all(&root);
}
