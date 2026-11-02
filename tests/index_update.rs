//! End-to-end checks for `filer index --update PATH`: refresh the cache for the
//! named files without walking the tree, and prove it by letting a full `index`
//! run afterwards reuse the memo.
//!
//! The cache lives in `XDG_CACHE_HOME`, so every test gets its own.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SERVICE: &str = "package app;\n\npublic class Service {\n    private Repo repo;\n\n    int used() { return repo.size(); }\n}\n";

const REPO: &str = "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n}\n";

fn project(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("filer-index-update-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("app")).unwrap();
    fs::write(dir.join("app/Service.java"), SERVICE).unwrap();
    fs::write(dir.join("app/Repo.java"), REPO).unwrap();
    dir
}

fn command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_filer"));
    command
        .arg("--root")
        .arg(root)
        .args(args)
        .env("XDG_CACHE_HOME", root.join(".cache"));
    command
}

fn filer(root: &Path, args: &[&str]) -> Output {
    command(root, args).output().expect("run filer")
}

fn filer_with_stdin(root: &Path, args: &[&str], input: &str) -> Output {
    let mut child = command(root, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn filer");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input.as_bytes())
        .expect("write paths");
    child.wait_with_output().expect("collect output")
}

fn json(root: &Path, args: &[&str]) -> serde_json::Value {
    let output = filer(root, args);
    assert!(
        output.status.success(),
        "filer {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("stdout must be one JSON object")
}

fn update(root: &Path, args: &[&str]) -> serde_json::Value {
    let mut full = vec!["--json", "index", "--update"];
    full.extend_from_slice(args);
    json(root, &full)
}

/// `(source_files, parsed, reused)` from a full `index --json`.
fn full_index(root: &Path) -> (u64, u64, u64) {
    let value = json(root, &["--json", "index"]);
    (
        value["source_files"].as_u64().unwrap_or_default(),
        value["cache"]["parsed"].as_u64().unwrap_or_default(),
        value["cache"]["reused"].as_u64().unwrap_or_default(),
    )
}

#[test]
fn updating_a_changed_file_makes_the_next_full_index_reuse_it() {
    let root = project("changed");
    assert_eq!(full_index(&root), (2, 2, 0));

    fs::write(
        root.join("app/Service.java"),
        SERVICE.replace("repo.size()", "repo.size() + 1"),
    )
    .unwrap();

    let stats = update(&root, &["app/Service.java"]);
    assert_eq!(stats["updated"]["parsed"], 1, "{stats}");
    assert_eq!(stats["updated"]["reused"], 0, "{stats}");
    assert_eq!(stats["entries"], 2, "{stats}");

    // No walk, no re-parse: the memo covers both files again.
    assert_eq!(full_index(&root), (2, 0, 2));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_unchanged_file_is_reused_not_reparsed() {
    let root = project("unchanged");
    assert_eq!(full_index(&root), (2, 2, 0));

    let stats = update(&root, &["app/Repo.java"]);
    assert_eq!(stats["updated"]["reused"], 1, "{stats}");
    assert_eq!(stats["updated"]["parsed"], 0, "{stats}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn new_files_from_stdin_and_deleted_files_are_handled() {
    let root = project("add-remove");
    assert_eq!(full_index(&root), (2, 2, 0));

    fs::write(
        root.join("app/Extra.java"),
        "package app;\n\npublic class Extra {\n    int x() { return 1; }\n}\n",
    )
    .unwrap();
    let output = filer_with_stdin(
        &root,
        &["--json", "index", "--update", "-"],
        "# touched by the edit\napp/Extra.java\n",
    );
    assert!(output.status.success(), "{output:?}");
    let stats: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("stats");
    assert_eq!(stats["updated"]["parsed"], 1, "{stats}");
    assert_eq!(full_index(&root), (3, 0, 3));

    // A deleted file leaves the cache on update, so recreating it re-parses.
    fs::remove_file(root.join("app/Repo.java")).unwrap();
    let stats = update(&root, &["app/Repo.java"]);
    assert_eq!(stats["updated"]["removed"], 1, "{stats}");
    fs::write(root.join("app/Repo.java"), REPO).unwrap();
    assert_eq!(full_index(&root), (3, 1, 2));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn paths_outside_the_root_or_not_source_are_skipped_with_a_warning() {
    let root = project("skipped");
    assert_eq!(full_index(&root), (2, 2, 0));

    let outside = root.parent().unwrap().join("filer-outside.java");
    fs::write(&outside, REPO).unwrap();
    let stats = update(&root, &[outside.to_str().unwrap()]);
    assert_eq!(stats["updated"]["skipped"], 1, "{stats}");

    fs::write(root.join("notes.txt"), "not source\n").unwrap();
    let output = filer(&root, &["index", "--update", "notes.txt"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not a source file"),
        "{output:?}"
    );
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_file(&outside);
}

#[test]
fn an_empty_update_list_is_an_empty_result() {
    let root = project("empty");
    let output = filer_with_stdin(&root, &["--json", "index", "--update", "-"], "# nothing\n");
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let error: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stderr).trim()).expect("envelope");
    assert_eq!(error["error"]["code"], "empty_result", "{error}");
    let _ = fs::remove_dir_all(&root);
}
