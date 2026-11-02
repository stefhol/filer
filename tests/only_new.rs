//! End-to-end checks for `bundle --only-new` and `--skip`: an agent that already
//! holds part of the context asks for the delta only.
//!
//! Three things matter: an empty manifest behaves like a plain bundle, a full
//! manifest is an empty result (exit 4, with a hint) rather than an error, and a
//! delta run never truncates the saved state.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SERVICE: &str = "package app;\n\npublic class Service {\n    private Repo repo;\n\n    int used() { return repo.size(); }\n\n    int also() { return repo.other(); }\n}\n";

const REPO: &str = "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n    public int other() { return 1; }\n}\n";

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-only-new-{name}-{}", std::process::id()));
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
        .expect("write manifest");
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

fn bundle(output: Output, args: &[&str]) -> serde_json::Value {
    serde_json::from_str(run(output, args).trim()).expect("stdout must be one JSON object")
}

/// `(path, methods)` for every entry, in output order.
fn units(bundle: &serde_json::Value) -> Vec<(String, Vec<String>)> {
    bundle["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
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
        })
        .unwrap_or_default()
}

/// Both methods seeded, so both callees land in one `methods` entry.
const SEEDS: [&str; 4] = [
    "--depth",
    "2",
    "app/Service.java::Service.used",
    "app/Service.java::Service.also",
];

fn seeded(root: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["--json", "bundle"];
    args.extend_from_slice(&SEEDS);
    args.extend_from_slice(extra);
    filer(root, &args)
}

#[test]
fn an_empty_manifest_is_a_plain_bundle() {
    let root = project("empty");
    let manifest = root.join("empty.json");
    fs::write(&manifest, "").unwrap();

    let plain = bundle(seeded(&root, &[]), &["bundle"]);
    let with_manifest = bundle(
        seeded(&root, &["--only-new", manifest.to_str().unwrap()]),
        &["bundle", "--only-new"],
    );
    assert_eq!(units(&plain), units(&with_manifest));
    assert!(!units(&plain).is_empty(), "{plain}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_full_manifest_returns_nothing_new_with_a_hint() {
    let root = project("full");
    // The manifest the agent would build from this very bundle.
    let full = bundle(seeded(&root, &[]), &["bundle"]);
    let manifest = root.join("full.json");
    fs::write(&manifest, full.to_string()).unwrap();

    let output = seeded(&root, &["--only-new", manifest.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let error: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stderr).trim()).expect("envelope");
    assert_eq!(error["error"]["code"], "empty_result", "{error}");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("nothing new"),
        "{error}"
    );
    assert!(error["error"]["hint"].is_string(), "{error}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_partial_manifest_returns_only_the_delta() {
    let root = project("delta");
    let manifest = root.join("manifest.txt");
    fs::write(
        &manifest,
        "# what the agent already pasted\napp/Repo.java\n",
    )
    .unwrap();

    let delta = bundle(
        seeded(&root, &["--only-new", manifest.to_str().unwrap()]),
        &["bundle", "--only-new"],
    );
    assert_eq!(
        units(&delta),
        vec![(
            "app/Service.java".to_string(),
            vec!["also".to_string(), "used".to_string()]
        )],
        "{delta}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_manifest_entry_can_cover_one_method_of_a_class() {
    let root = project("methods");
    // `bundle --json` names a method-scoped entry; only that method is "had".
    let manifest = root.join("methods.json");
    fs::write(
        &manifest,
        r#"[{"path":"app/Repo.java","scope":"methods","symbol":"Repo","methods":["size"]}]"#,
    )
    .unwrap();

    let delta = bundle(
        seeded(&root, &["--only-new", manifest.to_str().unwrap()]),
        &["bundle", "--only-new"],
    );
    assert_eq!(
        units(&delta),
        vec![
            (
                "app/Service.java".to_string(),
                vec!["also".to_string(), "used".to_string()]
            ),
            ("app/Repo.java".to_string(), vec!["other".to_string()]),
        ],
        "{delta}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn skip_removes_one_method_and_a_manifest_can_come_from_stdin() {
    let root = project("skip");
    let with_skip = bundle(
        seeded(&root, &["--skip", "app/Repo.java::Repo.size"]),
        &["bundle", "--skip"],
    );
    assert_eq!(
        units(&with_skip),
        vec![
            (
                "app/Service.java".to_string(),
                vec!["also".to_string(), "used".to_string()]
            ),
            ("app/Repo.java".to_string(), vec!["other".to_string()]),
        ],
        "{with_skip}"
    );

    let piped = bundle(
        filer_with_stdin(
            &root,
            &[
                "--json",
                "bundle",
                "--only-new",
                "-",
                "--depth",
                "2",
                "app/Service.java::Service.used",
                "app/Service.java::Service.also",
            ],
            "app/Service.java::Service\n",
        ),
        &["bundle", "--only-new -"],
    );
    assert_eq!(
        units(&piped),
        vec![(
            "app/Repo.java".to_string(),
            vec!["other".to_string(), "size".to_string()]
        )],
        "{piped}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_unreadable_manifest_is_a_file_error() {
    let root = project("missing");
    let output = seeded(&root, &["--only-new", "@nope.json"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let error: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stderr).trim()).expect("envelope");
    assert_eq!(error["error"]["code"], "file_not_found", "{error}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_delta_run_does_not_truncate_the_saved_state() {
    let root = project("state");
    let state = root.join(".filer/s1.toml");
    let manifest = root.join("manifest.txt");
    fs::write(&manifest, "app/Repo.java\n").unwrap();

    let output = seeded(
        &root,
        &[
            "--state",
            state.to_str().unwrap(),
            "--only-new",
            manifest.to_str().unwrap(),
        ],
    );
    let delta = bundle(output, &["bundle", "--state"]);
    assert_eq!(
        units(&delta),
        vec![(
            "app/Service.java".to_string(),
            vec!["also".to_string(), "used".to_string()]
        )],
        "{delta}"
    );

    let saved = fs::read_to_string(&state).unwrap();
    assert!(saved.contains("app/Repo.java"), "{saved}");
    assert!(saved.contains("app/Service.java"), "{saved}");
    let _ = fs::remove_dir_all(&root);
}
