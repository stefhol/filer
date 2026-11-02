//! End-to-end checks for what the index walks: `.gitignore`/`.ignore` files by
//! default (`--no-ignore` opts out) and the `--include`/`--exclude` globs.
//!
//! The interesting part is the cache: an index entry must follow the filter, and
//! a path that stops being visible must not linger in the index just because the
//! cache still knows it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SERVICE: &str = "package app;\n\npublic class Service {\n    private Repo repo;\n\n    int used() { return repo.size(); }\n}\n";

const REPO: &str = "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n}\n";

const GENERATED: &str =
    "package generated;\n\npublic class Gen {\n    int value() { return 1; }\n}\n";

const UTIL_FE: &str = "function helper(integer a) {\n    return a + 1;\n}\n";

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-filtering-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("app")).unwrap();
    fs::create_dir_all(dir.join("generated")).unwrap();
    fs::create_dir_all(dir.join("lib/deep")).unwrap();
    fs::write(dir.join("app/Service.java"), SERVICE).unwrap();
    fs::write(dir.join("app/Repo.java"), REPO).unwrap();
    fs::write(dir.join("generated/Gen.java"), GENERATED).unwrap();
    fs::write(dir.join("lib/deep/util.fe"), UTIL_FE).unwrap();
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

fn json(root: &Path, args: &[&str]) -> serde_json::Value {
    serde_json::from_str(run(root, args).trim()).expect("stdout must be one JSON object")
}

/// `search` exits 4 on an empty result, which is a valid answer here.
fn search(root: &Path, args: &[&str]) -> (Option<i32>, String) {
    let output = filer(root, args);
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// `(source_files, parsed, reused)` from `index --json`.
fn index_stats(root: &Path, args: &[&str]) -> (u64, u64, u64) {
    let mut full = vec!["--json", "index"];
    full.extend_from_slice(args);
    let value = json(root, &full);
    (
        value["source_files"].as_u64().unwrap_or_default(),
        value["cache"]["parsed"].as_u64().unwrap_or_default(),
        value["cache"]["reused"].as_u64().unwrap_or_default(),
    )
}

#[test]
fn gitignore_and_ignore_files_are_honored_by_default() {
    let root = project("ignore");
    fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    fs::write(root.join(".ignore"), "lib/\n").unwrap();

    // app/Service.java + app/Repo.java; `generated/` and `lib/` are ignored.
    assert_eq!(index_stats(&root, &[]), (2, 2, 0));
    assert_eq!(index_stats(&root, &["--no-ignore"]), (4, 2, 2));

    // `search` sees the same view, so a picker cannot resurrect ignored files.
    let (code, rows) = search(&root, &["--json", "search", "--query", "Gen"]);
    assert_eq!(code, Some(4), "{rows}");
    let (code, rows) = search(
        &root,
        &["--no-ignore", "--json", "search", "--query", "Gen"],
    );
    assert_eq!(code, Some(0), "{rows}");
    assert!(rows.contains("generated/Gen.java"), "{rows}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn include_and_exclude_globs_shape_the_index() {
    let root = project("globs");

    // A path glob is anchored at the root; `*` stays inside one segment.
    assert_eq!(index_stats(&root, &["--include", "app/**"]).0, 2);
    assert_eq!(index_stats(&root, &["--include", "app/*.java"]).0, 2);
    assert_eq!(index_stats(&root, &["--include", "generated/**"]).0, 1);

    // A name glob matches at any depth.
    assert_eq!(index_stats(&root, &["--include", "*.fe"]).0, 1);
    assert_eq!(index_stats(&root, &["--include", "*.java"]).0, 3);

    // Exclude wins over include; the same file can be named by path or by name.
    assert_eq!(
        index_stats(
            &root,
            &["--include", "app/**", "--exclude", "app/Repo.java"]
        )
        .0,
        1
    );
    assert_eq!(index_stats(&root, &["--exclude", "*.java"]).0, 1);
    assert_eq!(index_stats(&root, &["--exclude", "**/deep/**"]).0, 3);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_newly_ignored_path_leaves_the_index_and_comes_back_from_the_cache() {
    let root = project("cache");
    fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    // app/Service.java, app/Repo.java and lib/deep/util.fe.
    assert_eq!(index_stats(&root, &[]), (3, 3, 0));

    // The file is ignored now: it has to disappear, cache or not.
    fs::write(root.join(".gitignore"), "generated/\napp/Repo.java\n").unwrap();
    assert_eq!(index_stats(&root, &[]), (2, 0, 2));
    let (code, rows) = search(&root, &["--json", "search", "--query", "Repo"]);
    assert_eq!(code, Some(4), "{rows}");
    let deps = filer(&root, &["deps", "app/Repo.java"]);
    assert_eq!(deps.status.code(), Some(3), "{deps:?}");

    // Un-ignoring it must not need a re-parse: the memo survived the filter.
    fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    assert_eq!(index_stats(&root, &[]), (3, 0, 3));
    let (_, rows) = search(&root, &["--json", "search", "--query", "Repo"]);
    assert!(rows.contains("app/Repo.java"), "{rows}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_deleted_file_is_dropped_from_the_cache() {
    let root = project("deleted");
    assert_eq!(index_stats(&root, &[]), (4, 4, 0));

    fs::remove_file(root.join("app/Repo.java")).unwrap();
    assert_eq!(index_stats(&root, &[]), (3, 0, 3));

    // Recreting it is a fresh parse: the memo of the deleted file is gone.
    fs::write(root.join("app/Repo.java"), REPO).unwrap();
    assert_eq!(index_stats(&root, &[]), (4, 1, 3));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_filtered_out_seed_is_bundled_as_is_with_a_warning() {
    let root = project("seed");
    let output = filer(
        &root,
        &[
            "--json",
            "bundle",
            "--exclude",
            "app/Repo.java",
            "--depth",
            "0",
            "app/Repo.java",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not in the index"), "{stderr}");
    let bundle: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("bundle");
    assert_eq!(bundle["entries"][0]["path"], "app/Repo.java", "{bundle}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_broken_glob_is_a_clean_error() {
    let root = project("bad-glob");
    let output = filer(&root, &["--json", "--include", "[", "index"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stderr).trim()).expect("envelope");
    assert_eq!(error["error"]["code"], "internal", "{error}");
    assert!(error["error"]["hint"].is_string(), "{error}");
    let _ = fs::remove_dir_all(&root);
}
