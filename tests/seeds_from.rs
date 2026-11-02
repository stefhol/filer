//! End-to-end checks for `bundle --seeds-from`: seed lists come from stdin or a
//! file, one path per line, so `rg -l ... | filer bundle --seeds-from -` works
//! and long lists never reach argv.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SERVICE: &str = "package app;\n\npublic class Service {\n    private Repo repo;\n\n    public Service(Repo repo) { this.repo = repo; }\n\n    int used() { return repo.size(); }\n}\n";

const REPO: &str = "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n}\n";

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-seeds-from-{name}-{}", std::process::id()));
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
        .expect("write seeds");
    child.wait_with_output().expect("collect output")
}

fn run(output: Output, args: &[&str]) -> String {
    assert!(
        output.status.success(),
        "filer {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

fn entries(output: Output, args: &[&str]) -> Vec<serde_json::Value> {
    let text = run(output, args);
    let bundle: serde_json::Value =
        serde_json::from_str(text.trim()).expect("stdout must be one JSON object");
    bundle["entries"].as_array().cloned().unwrap_or_default()
}

fn labels(entries: &[serde_json::Value]) -> Vec<String> {
    let mut labels: Vec<String> = entries
        .iter()
        .map(|entry| {
            let path = entry["path"].as_str().unwrap_or_default();
            match entry["symbol"].as_str() {
                Some(symbol) => format!("{path}::{symbol}"),
                None => path.to_string(),
            }
        })
        .collect();
    labels.sort();
    labels
}

#[test]
fn seeds_from_a_file_skips_comments_blank_lines_and_crlf() {
    let root = project("file");
    let seeds = root.join("seeds.txt");
    // The `\r` matters: lists produced on Windows or by some harnesses carry it.
    fs::write(
        &seeds,
        "# what the agent already looked at\r\n\r\n  app/Service.java::Service.used  \r\napp/Repo.java\r\n",
    )
    .unwrap();

    let after = entries(
        filer(
            &root,
            &[
                "--json",
                "bundle",
                "--depth",
                "1",
                "--seeds-from",
                &format!("@{}", seeds.display()),
            ],
        ),
        &["bundle"],
    );
    assert_eq!(
        labels(&after),
        vec![
            "app/Repo.java".to_string(),
            "app/Service.java::Service".to_string()
        ],
        "{after:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn seeds_from_stdin_and_a_bare_path_both_work() {
    let root = project("stdin");
    let expected = vec!["app/Repo.java".to_string(), "app/Service.java".to_string()];

    let piped = entries(
        filer_with_stdin(
            &root,
            &[
                "--json",
                "bundle",
                "--depth",
                "1",
                "--seeds-from",
                "-",
                "app/Service.java",
            ],
            "app/Repo.java\n",
        ),
        &["bundle", "--seeds-from -"],
    );
    assert_eq!(labels(&piped), expected, "{piped:?}");

    let seeds = root.join("seeds.txt");
    fs::write(&seeds, "app/Repo.java\napp/Service.java\n").unwrap();
    let from_path = entries(
        filer(
            &root,
            &[
                "--json",
                "bundle",
                "--depth",
                "1",
                "--seeds-from",
                seeds.to_str().unwrap(),
            ],
        ),
        &["bundle", "--seeds-from PATH"],
    );
    assert_eq!(labels(&from_path), expected, "{from_path:?}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_missing_seed_file_is_a_file_error() {
    let root = project("missing");
    let output = filer(
        &root,
        &["--json", "bundle", "--seeds-from", "@does-not-exist.txt"],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let envelope: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stderr).trim()).expect("envelope");
    assert_eq!(envelope["error"]["code"], "file_not_found", "{envelope}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_empty_seed_list_is_an_empty_result_and_writes_no_state() {
    let root = project("empty");
    let seeds = root.join("seeds.txt");
    fs::write(&seeds, "# nothing to bundle yet\n\n").unwrap();
    let state = root.join(".filer/s1.toml");

    let output = filer(
        &root,
        &[
            "--json",
            "bundle",
            "--seeds-from",
            seeds.to_str().unwrap(),
            "--state",
            state.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let envelope: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stderr).trim()).expect("envelope");
    assert_eq!(envelope["error"]["code"], "empty_result", "{envelope}");
    assert!(envelope["error"]["hint"].is_string(), "{envelope}");
    assert!(!state.exists(), "no state for an empty seed list");
    let _ = fs::remove_dir_all(&root);
}
