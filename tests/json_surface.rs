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

/// Two branches, one `methods` and one whole-file entry, in the v3 on-disk
/// layout `state --json` mirrors.
const TWO_BRANCH_STATE: &str = r#"root = "/tmp/demo"
version = 3

[[bundles]]
name = "main"
created = 1793618434
depth = 2

[[bundles.entries]]
path = "app/Service.java"
scope = "methods"
symbol = "Service"
methods = ["used"]
depth = 0
reason = "seed"

[[bundles.entries]]
path = "app/Repo.java"
scope = "file"
depth = 1
reason = "import"

[[bundles]]
name = "feature"
created = 1793618444
depth = 1

[[bundles.entries]]
path = "app/Repo.java"
scope = "file"
depth = 0
reason = "seed"
"#;

fn state_file(root: &Path) -> PathBuf {
    let path = root.join("state.toml");
    fs::write(&path, TWO_BRANCH_STATE).unwrap();
    path
}

#[test]
fn state_json_reports_a_missing_state_file() {
    let root = project("state-none");
    let state = object(&run(&root, &["--json", "state"]));

    assert_eq!(state["schema_version"], 1, "{state}");
    assert_eq!(state["exists"], false, "{state}");
    assert!(state["current"].is_null(), "{state}");
    assert_eq!(state["branches"], serde_json::json!([]), "{state}");
    assert!(
        state["state_path"]
            .as_str()
            .unwrap()
            .ends_with(".filer/bundle.toml"),
        "{state}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn state_json_lists_branches_and_the_selected_bundle() {
    let root = project("state-branches");
    let path = state_file(&root);
    let file = path.to_str().unwrap();

    // No .git in the fixture: the current branch is empty, so no bundle is
    // current until `--branch` names one.
    let listing = object(&run(&root, &["--json", "state", "--file", file]));
    assert_eq!(listing["exists"], true, "{listing}");
    let branches = listing["branches"].as_array().expect("branches");
    assert_eq!(branches.len(), 2, "{listing}");
    assert_eq!(branches[0]["branch"], "main", "{listing}");
    assert_eq!(branches[0]["label"], "main", "{listing}");
    assert_eq!(branches[0]["entries"], 2, "{listing}");
    assert_eq!(branches[0]["depth"], 2, "{listing}");
    assert_eq!(branches[0]["created"], 1793618434, "{listing}");
    assert_eq!(branches[1]["branch"], "feature", "{listing}");
    assert!(listing["current"].is_null(), "{listing}");

    let selected = object(&run(
        &root,
        &["--json", "state", "--file", file, "--branch", "main"],
    ));
    let current = &selected["current"];
    assert_eq!(current["branch"], "main", "{selected}");
    assert_eq!(current["created"], 1793618434, "{selected}");
    assert_eq!(current["depth"], 2, "{selected}");
    let entries = current["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2, "{selected}");
    assert_eq!(entries[0]["path"], "app/Service.java", "{selected}");
    assert_eq!(entries[0]["scope"], "methods", "{selected}");
    assert_eq!(entries[0]["symbol"], "Service", "{selected}");
    assert_eq!(
        entries[0]["methods"],
        serde_json::json!(["used"]),
        "{selected}"
    );
    assert_eq!(entries[0]["reason"], "seed", "{selected}");
    assert_eq!(entries[1]["scope"], "file", "{selected}");
    assert!(entries[1]["symbol"].is_null(), "{selected}");
    assert_eq!(entries[1]["methods"], serde_json::json!([]), "{selected}");

    // Asking for a branch with no bundle is not an error; it reports no bundle.
    let missing = object(&run(
        &root,
        &["--json", "state", "--file", file, "--branch", "nope"],
    ));
    assert!(missing["current"].is_null(), "{missing}");
    assert_eq!(missing["exists"], true, "{missing}");
    assert_eq!(
        missing["branches"].as_array().unwrap().len(),
        2,
        "{missing}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn state_json_clear_names_what_was_removed() {
    let root = project("state-clear");
    let path = state_file(&root);
    let file = path.to_str().unwrap();

    let cleared = object(&run(
        &root,
        &[
            "--json", "state", "--file", file, "--clear", "--branch", "main",
        ],
    ));
    assert_eq!(cleared["cleared"], "main", "{cleared}");
    let listing = object(&run(&root, &["--json", "state", "--file", file]));
    let names: Vec<&str> = listing["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|branch| branch["branch"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["feature"], "{listing}");

    // Nothing to clear is still exit 0 and still an object.
    let missing = object(&run(
        &root,
        &[
            "--json", "state", "--file", file, "--clear", "--branch", "nope",
        ],
    ));
    assert!(missing["cleared"].is_null(), "{missing}");

    let all = object(&run(
        &root,
        &["--json", "state", "--file", file, "--clear", "--all"],
    ));
    assert_eq!(all["cleared"], "all", "{all}");
    assert!(!path.exists(), "--clear --all removes the state file");
    let again = object(&run(
        &root,
        &["--json", "state", "--file", file, "--clear", "--all"],
    ));
    assert!(again["cleared"].is_null(), "{again}");

    let _ = fs::remove_dir_all(&root);
}

/// `cache.parsed`/`cache.reused` depend on whether the previous run populated
/// the cache, so they are the one part of `index --json` that two runs differ in.
fn without_cache(mut value: serde_json::Value) -> serde_json::Value {
    value.as_object_mut().unwrap().remove("cache");
    value
}

#[test]
fn quiet_does_not_change_the_json_surface() {
    let root = project("quiet");
    let path = state_file(&root);
    let file = path.to_str().unwrap();

    for args in [
        vec!["--json", "index"],
        vec!["--json", "state", "--file", file],
        vec!["--json", "state", "--file", file, "--branch", "main"],
    ] {
        let mut quiet = vec!["--json", "--quiet"];
        quiet.extend(args.iter().skip(1).copied());
        assert_eq!(
            without_cache(object(&run(&root, &args))),
            without_cache(object(&run(&root, &quiet))),
            "--quiet must not change {args:?}"
        );
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn index_json_carries_index_wide_coverage() {
    let root = project("coverage");
    // One import nothing in the project provides, and one source file the walk
    // finds but cannot read, so both counters have to be computed rather than
    // defaulted.
    fs::write(
        root.join("app/Tax.java"),
        "package app;\n\nimport com.acme.Missing;\n\npublic class Tax {}\n",
    )
    .unwrap();
    let unreadable = root.join("app/Unreadable.java");
    fs::write(&unreadable, "package app;\n\npublic class Unreadable {}\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    }
    // Root ignores the mode bits, so only assert the skip when it took effect.
    let skipped = usize::from(fs::read(&unreadable).is_err());

    let index = object(&run(&root, &["--json", "index"]));
    let coverage = index["coverage"].as_object().expect("coverage object");
    assert_eq!(coverage.len(), 3, "{index}");
    assert_eq!(coverage["files_indexed"], 4 - skipped, "{index}");
    assert_eq!(coverage["files_skipped"], skipped, "{index}");
    assert_eq!(coverage["imports_unresolved"], 1, "{index}");

    let _ = fs::remove_dir_all(&root);
}
