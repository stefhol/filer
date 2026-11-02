//! End-to-end checks for per-session state (`--session`, `$FILER_SESSION`) and
//! the `context list|clear` surface an agent uses to introspect and reset.
//!
//! Sessions are repo-local (`.filer/sessions/<id>.toml`) and gitignored by the
//! same `*` file the default state uses, which is asserted here too.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SERVICE: &str = "package app;\n\npublic class Service {\n    private Repo repo;\n\n    int used() { return repo.size(); }\n}\n";

const REPO: &str = "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n}\n";

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-context-{name}-{}", std::process::id()));
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
        .env("XDG_CACHE_HOME", root.join(".cache"))
        .env_remove("FILER_SESSION");
    command
}

fn filer(root: &Path, args: &[&str]) -> Output {
    command(root, args).output().expect("run filer")
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

fn sessions(root: &Path) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(root.join(".filer/sessions"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| name.ends_with(".toml"))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

#[test]
fn sessions_are_separate_files_and_leave_the_default_state_alone() {
    let root = project("separate");

    run(
        &root,
        &[
            "bundle",
            "--session",
            "s1",
            "--depth",
            "0",
            "app/Service.java",
        ],
    );
    run(
        &root,
        &["bundle", "--session", "s2", "--depth", "0", "app/Repo.java"],
    );

    assert_eq!(sessions(&root), vec!["s1.toml", "s2.toml"]);
    assert!(
        !root.join(".filer/bundle.toml").exists(),
        "a session run must not touch the default state"
    );
    // Repo-local but never committed: the state writer drops a `*` ignore file.
    assert_eq!(
        fs::read_to_string(root.join(".filer/sessions/.gitignore")).unwrap(),
        "*\n"
    );

    // `show` and `state` read the session, not the default file.
    let shown = run(&root, &["show", "--session", "s1"]);
    assert!(shown.contains("app/Service.java"), "{shown}");
    assert!(!shown.contains("app/Repo.java"), "{shown}");
    let state = run(&root, &["state", "--session", "s2"]);
    assert!(state.contains("s2.toml"), "{state}");
    assert!(state.contains("app/Repo.java"), "{state}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn context_list_describes_the_default_state_and_every_session() {
    let root = project("list");

    let empty = json(&root, &["--json", "context", "list"]);
    assert_eq!(empty["schema_version"], 1, "{empty}");
    assert_eq!(empty["default"]["exists"], false, "{empty}");
    assert_eq!(
        empty["sessions"].as_array().map(Vec::len),
        Some(0),
        "{empty}"
    );

    // The default state is written when a state file is named: humans pass the
    // default path, sessions pass `--session`.
    let default = root.join(".filer/bundle.toml");
    run(
        &root,
        &[
            "bundle",
            "--depth",
            "1",
            "--state",
            default.to_str().unwrap(),
            "app/Service.java",
        ],
    );
    run(
        &root,
        &["bundle", "--session", "s1", "--depth", "0", "app/Repo.java"],
    );

    let listed = json(&root, &["--json", "context", "list"]);
    assert_eq!(listed["default"]["exists"], true, "{listed}");
    assert_eq!(listed["default"]["entries"], 2, "{listed}");
    let sessions = listed["sessions"].as_array().expect("sessions");
    assert_eq!(sessions.len(), 1, "{listed}");
    assert_eq!(sessions[0]["id"], "s1", "{listed}");
    assert_eq!(sessions[0]["entries"], 1, "{listed}");
    assert_eq!(sessions[0]["path"], ".filer/sessions/s1.toml", "{listed}");

    let human = run(&root, &["context", "list"]);
    assert!(human.contains("default"), "{human}");
    assert!(human.contains("s1"), "{human}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn context_clear_removes_one_session_or_everything() {
    let root = project("clear");
    let default = root.join(".filer/bundle.toml");
    run(
        &root,
        &[
            "bundle",
            "--depth",
            "0",
            "--state",
            default.to_str().unwrap(),
            "app/Service.java",
        ],
    );
    run(
        &root,
        &["bundle", "--session", "s1", "--depth", "0", "app/Repo.java"],
    );
    run(
        &root,
        &["bundle", "--session", "s2", "--depth", "0", "app/Repo.java"],
    );

    let cleared = json(&root, &["--json", "context", "clear", "s1"]);
    assert_eq!(cleared["count"], 1, "{cleared}");
    assert_eq!(
        cleared["cleared"][0], ".filer/sessions/s1.toml",
        "{cleared}"
    );
    assert_eq!(sessions(&root), vec!["s2.toml"]);

    let cleared = json(&root, &["--json", "context", "clear", "--all"]);
    assert_eq!(cleared["count"], 2, "{cleared}");
    assert!(sessions(&root).is_empty());
    assert!(
        !root.join(".filer/bundle.toml").exists(),
        "--all must clear the default state too"
    );

    // Clearing again is not an error, just nothing to do.
    let again = json(&root, &["--json", "context", "clear", "--all"]);
    assert_eq!(again["count"], 0, "{again}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_unsafe_session_id_is_rejected() {
    let root = project("unsafe");
    let output = filer(
        &root,
        &[
            "bundle",
            "--session",
            "../evil",
            "--depth",
            "0",
            "app/Repo.java",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid session id"), "{stderr}");
    assert!(stderr.contains("hint"), "{stderr}");
    assert!(!root.join(".filer").exists(), "nothing may be written");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn filer_session_env_selects_the_session_when_the_flag_is_absent() {
    let root = project("env");
    let output = command(&root, &["bundle", "--depth", "0", "app/Repo.java"])
        .env("FILER_SESSION", "s9")
        .output()
        .expect("run filer");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(sessions(&root), vec!["s9.toml"]);

    // An empty value means "no session", not a file named `.toml`.
    let output = command(&root, &["bundle", "--depth", "0", "app/Repo.java"])
        .env("FILER_SESSION", "  ")
        .output()
        .expect("run filer");
    assert!(output.status.success(), "{output:?}");
    assert!(
        !root.join(".filer/bundle.toml").exists(),
        "no default write either"
    );
    let _ = fs::remove_dir_all(&root);
}
