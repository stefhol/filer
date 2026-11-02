//! End-to-end checks for `bundle --from-state`: the refresh half of the loop
//! search -> bundle -> edit -> refresh -> verify.
//!
//! These run the real binary because the contract is about what an agent gets
//! back after an edit: a re-resolved closure, never the old bundle plus the new
//! one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SERVICE: &str = "package app;\n\npublic class Service {\n    private Repo repo;\n\n    public Service(Repo repo) { this.repo = repo; }\n\n    int used() { return repo.size(); }\n\n    int unused() { return 1; }\n}\n";

const REPO: &str = "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n    public int other() { return 1; }\n}\n";

const OTHER: &str = "package app;\n\npublic class Other {\n    int value() { return 42; }\n}\n";

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-from-state-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("app")).unwrap();
    fs::write(dir.join("app/Service.java"), SERVICE).unwrap();
    fs::write(dir.join("app/Repo.java"), REPO).unwrap();
    fs::write(dir.join("app/Other.java"), OTHER).unwrap();
    dir
}

fn filer(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_filer"))
        .arg("--root")
        .arg(root)
        .args(args)
        .env("XDG_CACHE_HOME", root.join(".cache"))
        .output()
        .expect("run filer")
}

fn run(root: &Path, args: &[&str]) -> String {
    let output = filer(root, args);
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

fn entries(root: &Path, args: &[&str]) -> Vec<serde_json::Value> {
    let bundle = object(&run(root, args));
    bundle["entries"].as_array().cloned().unwrap_or_default()
}

/// `(path, methods)` for every method-scoped entry, so assertions name the unit
/// the resolver picked instead of just its file.
fn method_units(entries: &[serde_json::Value]) -> Vec<(String, Vec<String>)> {
    entries
        .iter()
        .filter(|entry| entry["scope"] == "methods")
        .map(|entry| {
            let methods = entry["methods"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (
                entry["path"].as_str().unwrap_or_default().to_string(),
                methods,
            )
        })
        .collect()
}

fn paths(entries: &[serde_json::Value]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| entry["path"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Absolute state path: `--state` is resolved against the process cwd, not
/// `--root`.
fn state_path(root: &Path) -> String {
    root.join(".filer/s1.toml").display().to_string()
}

fn seed(root: &Path, state: &str) {
    let output = filer(
        root,
        &[
            "--json",
            "bundle",
            "--depth",
            "2",
            "--state",
            state,
            "app/Service.java::Service.used",
        ],
    );
    assert!(
        output.status.success(),
        "seeding failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn refresh(root: &Path, state: &str, depth: &str) -> Vec<serde_json::Value> {
    entries(
        root,
        &[
            "--json",
            "bundle",
            "--from-state",
            "--depth",
            depth,
            "--state",
            state,
        ],
    )
}

#[test]
fn refresh_picks_up_a_call_added_by_an_edit() {
    let root = project("added-call");
    let state = state_path(&root);
    seed(&root, &state);

    let before = refresh(&root, &state, "2");
    assert_eq!(
        method_units(&before),
        vec![
            ("app/Service.java".to_string(), vec!["used".to_string()]),
            ("app/Repo.java".to_string(), vec!["size".to_string()]),
        ],
        "{before:?}"
    );

    // The edit an agent would make: call a second method of the same callee.
    fs::write(
        root.join("app/Service.java"),
        SERVICE.replace("return repo.size();", "return repo.size() + repo.other();"),
    )
    .unwrap();

    let after = refresh(&root, &state, "2");
    let units = method_units(&after);
    assert!(
        units.contains(&(
            "app/Repo.java".to_string(),
            vec!["other".to_string(), "size".to_string()]
        )) || units.contains(&(
            "app/Repo.java".to_string(),
            vec!["size".to_string(), "other".to_string()]
        )),
        "the new callee must be resolved: {units:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn refresh_drops_a_method_deleted_from_the_source() {
    let root = project("deleted-method");
    let state = state_path(&root);
    seed(&root, &state);

    fs::write(
        root.join("app/Repo.java"),
        REPO.replace("    public int size() { return 0; }\n", ""),
    )
    .unwrap();

    let after = refresh(&root, &state, "2");
    let units = method_units(&after);
    assert!(
        !units
            .iter()
            .any(|(path, methods)| path == "app/Repo.java" && methods.contains(&"size".to_string())),
        "the deleted method must not survive: {units:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn refresh_re_resolves_the_seeds_instead_of_merging_the_old_bundle() {
    let root = project("no-merge");
    let state = state_path(&root);

    // Two seeds: a method scope plus a whole file the caller added by hand.
    let seeded = filer(
        &root,
        &[
            "--json",
            "bundle",
            "--depth",
            "2",
            "--state",
            &state,
            "app/Service.java::Service.used",
            "app/Other.java",
        ],
    );
    assert!(seeded.status.success(), "{seeded:?}");
    assert!(paths(&refresh(&root, &state, "2")).contains(&"app/Repo.java".to_string()));

    // Drop the only call: with merge semantics `Repo` stays at depth 1 forever.
    fs::write(
        root.join("app/Service.java"),
        SERVICE.replace("return repo.size();", "return 7;"),
    )
    .unwrap();

    let after = refresh(&root, &state, "2");
    // Seeds keep their command-line order (see "Order and budget" in the
    // README), so the recovered seeds come back in the order the state file
    // saved them, with no derived entries in between.
    assert_eq!(
        paths(&after),
        vec!["app/Service.java".to_string(), "app/Other.java".to_string()],
        "a refresh must not carry derived entries over: {after:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_seed_whose_method_vanished_is_reported_not_written_as_empty() {
    let root = project("vanished-seed");
    let state = state_path(&root);
    seed(&root, &state);
    let saved = fs::read_to_string(Path::new(&state)).unwrap();

    fs::write(
        root.join("app/Service.java"),
        SERVICE.replace("    int used() { return repo.size(); }\n", ""),
    )
    .unwrap();

    let output = filer(
        &root,
        &[
            "--json",
            "bundle",
            "--from-state",
            "--state",
            &state,
            "--depth",
            "2",
        ],
    );
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no longer in the file"), "{stderr}");
    assert!(stderr.contains("no seed entries"), "{stderr}");
    assert_eq!(
        fs::read_to_string(Path::new(&state)).unwrap(),
        saved,
        "a failed refresh must not overwrite the state"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn from_state_without_a_saved_bundle_is_a_state_error() {
    let root = project("no-state");
    let output = filer(&root, &["--json", "bundle", "--from-state"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let envelope = object(&String::from_utf8_lossy(&output.stderr));
    assert_eq!(envelope["error"]["code"], "state_not_found", "{envelope}");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(".filer/bundle.toml"),
        "the default state path must be named: {envelope}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn from_state_and_reset_are_mutually_exclusive() {
    let root = project("conflict");
    let output = filer(&root, &["bundle", "--from-state", "--reset"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot be used with"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(&root);
}
