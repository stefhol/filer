//! End-to-end checks for `.filer/config.toml`: defaults for depth, budget,
//! excludes and roots, with **flag > config > built-in** for every one of them.
//!
//! Each test writes its own config: the point is that the resolved value is
//! observable from the outside, not that the parser works.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SERVICE: &str = "package app;\n\npublic class Service {\n    private Repo repo;\n\n    int used() { return repo.size(); }\n}\n";

const REPO: &str = "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n}\n";

const UTIL_FE: &str = "function helper(integer a) {\n    return a + 1;\n}\n";

const GENERATED: &str =
    "package generated;\n\npublic class Gen {\n    int value() { return 1; }\n}\n";

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-config-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("app")).unwrap();
    fs::create_dir_all(dir.join("lib/deep")).unwrap();
    fs::create_dir_all(dir.join("generated")).unwrap();
    fs::create_dir_all(dir.join(".filer")).unwrap();
    fs::write(dir.join("app/Service.java"), SERVICE).unwrap();
    fs::write(dir.join("app/Repo.java"), REPO).unwrap();
    fs::write(dir.join("lib/deep/util.fe"), UTIL_FE).unwrap();
    fs::write(dir.join("generated/Gen.java"), GENERATED).unwrap();
    fs::write(dir.join(".gitignore"), "generated/\n").unwrap();
    dir
}

fn config(root: &Path, text: &str) {
    fs::write(root.join(".filer/config.toml"), text).unwrap();
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

fn source_files(root: &Path, extra: &[&str]) -> u64 {
    let mut args = vec!["--json", "index"];
    args.extend_from_slice(extra);
    json(root, &args)["source_files"]
        .as_u64()
        .unwrap_or_default()
}

/// `search --json` is JSON lines: one object per hit.
fn search_rows(root: &Path, extra: &[&str]) -> Vec<serde_json::Value> {
    let mut args = vec!["--json", "search"];
    args.extend_from_slice(extra);
    let output = filer(root, &args);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("search row is JSON"))
        .collect();
    match output.status.code() {
        Some(0) => rows,
        // An empty result is one hint object, not a row.
        Some(4) => Vec::new(),
        _ => panic!("search failed: {}", String::from_utf8_lossy(&output.stderr)),
    }
}

/// Every entry `(path, depth)` of a `bundle --json` run, plus the envelope depth.
fn bundle(root: &Path, extra: &[&str]) -> (u64, Vec<(String, u64)>) {
    let mut args = vec!["--json", "bundle"];
    args.extend_from_slice(extra);
    args.push("app/Service.java::Service.used");
    let value = json(root, &args);
    let entries = value["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["path"].as_str().unwrap_or_default().to_string(),
                        entry["depth"].as_u64().unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    (value["depth"].as_u64().unwrap_or_default(), entries)
}

#[test]
fn depth_comes_from_the_config_and_the_flag_wins() {
    let root = project("depth");

    // Built-in default: two hops, so the callee is in the bundle.
    let (depth, entries) = bundle(&root, &[]);
    assert_eq!(depth, 2);
    assert!(
        entries.iter().any(|(path, _)| path == "app/Repo.java"),
        "{entries:?}"
    );

    config(&root, "[bundle]\ndepth = 0\n");
    let (depth, entries) = bundle(&root, &[]);
    assert_eq!(depth, 0, "config depth must be used");
    assert_eq!(
        entries,
        vec![("app/Service.java".to_string(), 0)],
        "depth 0 bundles only the seed"
    );

    // Flag over config.
    let (depth, entries) = bundle(&root, &["--depth", "2"]);
    assert_eq!(depth, 2, "--depth must win over the config");
    assert!(
        entries.iter().any(|(path, _)| path == "app/Repo.java"),
        "{entries:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn max_file_bytes_comes_from_the_config_and_the_flag_wins() {
    let root = project("budget");
    config(&root, "[bundle]\nmax_file_bytes = 20\n");

    let (_, _) = bundle(&root, &[]);
    let small = json(
        &root,
        &["--json", "bundle", "app/Service.java::Service.used"],
    );
    assert_eq!(small["budget"]["max_file_bytes"], 20, "{small}");
    assert_eq!(small["budget"]["truncated"], true, "{small}");
    assert_eq!(small["entries"][0]["rendered"], false, "{small}");

    let large = json(
        &root,
        &[
            "--json",
            "bundle",
            "--max-file-bytes",
            "100000",
            "app/Service.java::Service.used",
        ],
    );
    assert_eq!(large["budget"]["max_file_bytes"], 100000, "{large}");
    assert_eq!(large["entries"][0]["rendered"], true, "{large}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn excludes_come_from_the_config_and_the_flag_replaces_them() {
    let root = project("excludes");
    // generated/ is gitignored, so 3 files: Service, Repo, util.fe.
    assert_eq!(source_files(&root, &[]), 3);

    config(&root, "[index]\nexclude = [\"app/Repo.java\"]\n");
    assert_eq!(source_files(&root, &[]), 2);

    // The flag replaces the config list instead of appending to it: Repo is back
    // and Service is out.
    let rows = search_rows(&root, &["--query", "Repo", "--exclude", "app/Service.java"]);
    assert!(!rows.is_empty(), "Repo must be indexed again");
    assert!(
        rows.iter().all(|row| row["path"] == "app/Repo.java"),
        "{rows:?}"
    );
    let rows = search_rows(
        &root,
        &["--query", "Service", "--exclude", "app/Service.java"],
    );
    assert!(rows.is_empty(), "Service must be excluded: {rows:?}");

    // Globs from the config are globs, not literal paths.
    config(&root, "[index]\nexclude = [\"*.fe\"]\n");
    assert_eq!(source_files(&root, &[]), 2);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn roots_come_from_the_config_and_the_flag_replaces_them() {
    let root = project("roots");

    config(&root, "[index]\nroots = [\"app\"]\n");
    assert_eq!(source_files(&root, &[]), 2);
    assert_eq!(source_files(&root, &["--roots", "lib"]), 1);
    assert_eq!(source_files(&root, &["--roots", "app,lib"]), 3);

    // A root outside the project is an error, not a silently empty index.
    let output = filer(&root, &["--roots", "../", "index"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("outside"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ignore_can_be_turned_off_in_the_config() {
    let root = project("ignore");
    assert_eq!(source_files(&root, &[]), 3);

    config(&root, "[index]\nignore = false\n");
    assert_eq!(source_files(&root, &[]), 4);
    // `--no-ignore` and a config that already says `false` cannot disagree.
    assert_eq!(source_files(&root, &["--no-ignore"]), 4);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_broken_config_is_an_error_naming_the_file() {
    let root = project("broken");

    config(&root, "[bundle]\ndepht = 2\n");
    let output = filer(&root, &["index"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(".filer/config.toml"), "{stderr}");
    assert!(stderr.contains("depht"), "{stderr}");

    config(&root, "[bundle\n");
    let output = filer(&root, &["--json", "index"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stderr).trim()).expect("envelope");
    assert_eq!(error["error"]["code"], "internal", "{error}");
    let _ = fs::remove_dir_all(&root);
}
