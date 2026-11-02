//! End-to-end checks for the versioned JSON surface.
//!
//! These run the real binary because the contract is about stdout: every JSON
//! output must carry `schema_version`, and `bundle`/`show` must expose the same
//! units as markdown.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-json-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("app")).unwrap();
    fs::write(
        dir.join("app/Service.java"),
        "package app;\n\npublic class Service {\n    private Repo repo;\n\n    public Service(Repo repo) { this.repo = repo; }\n\n    int used() { return repo.size(); }\n\n    int unused() { return 1; }\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("app/Repo.java"),
        "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n    public int other() { return 1; }\n}\n",
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

#[test]
fn every_json_output_carries_the_schema_version() {
    let root = project("version");
    let state = root.join(".filer/bundle.toml");

    let index = object(&run(&root, &["--json", "index"]));
    assert_eq!(index["schema_version"], 1, "{index}");
    assert_eq!(index["source_files"], 2, "{index}");

    for line in run(&root, &["--json", "search", "--query", "Service"])
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let row = object(line);
        assert_eq!(row["schema_version"], 1, "{row}");
        assert!(row["path"].is_string(), "{row}");
    }

    let deps = object(&run(&root, &["--json", "deps", "app/Service.java"]));
    assert_eq!(deps["schema_version"], 1, "{deps}");
    assert!(deps["deps"].is_array(), "{deps}");

    let bundle = object(&run(
        &root,
        &[
            "--json",
            "bundle",
            "--depth",
            "0",
            "--state",
            state.to_str().unwrap(),
            "app/Service.java::Service.used",
        ],
    ));
    assert_eq!(bundle["schema_version"], 1, "{bundle}");
    let entries = bundle["entries"].as_array().expect("entries");
    assert!(!entries.is_empty(), "{bundle}");
    for entry in entries {
        for field in [
            "path",
            "scope",
            "depth",
            "reason",
            "start_line",
            "end_line",
            "start_byte",
            "end_byte",
            "tokens",
            "rendered",
            "markdown",
        ] {
            assert!(!entry[field].is_null(), "missing {field}: {entry}");
        }
        assert!(entry["rendered"].as_bool().unwrap(), "{entry}");
    }
    assert!(bundle["state_toml"].is_string(), "{bundle}");

    let shown = object(&run(
        &root,
        &["--json", "show", "--state", state.to_str().unwrap()],
    ));
    assert_eq!(shown["schema_version"], 1, "{shown}");
    assert_eq!(
        shown["entries"].as_array().unwrap().len(),
        entries.len(),
        "show and bundle describe the same units"
    );
    assert!(shown["state_toml"].is_string(), "{shown}");

    let index = object(&run(&root, &["--json", "index"]));
    assert_eq!(index["state"]["entries"], 1, "{index}");

    let _ = fs::remove_dir_all(&root);
}
