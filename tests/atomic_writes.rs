//! Multi-process acceptance tests for the atomic state and cache writes.
//!
//! These run the real binary, because the guarantee is about what a reader
//! sees while another *process* is writing: either the old file or the new
//! one, never a partial one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const FILES: usize = 24;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-atomic-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    for index in 0..FILES {
        fs::write(
            dir.join(format!("src/Type{index}.java")),
            format!(
                "package app;\n\nclass Type{index} {{\n    int value() {{\n        return {index};\n    }}\n}}\n"
            ),
        )
        .unwrap();
    }
    dir
}

fn seeds() -> Vec<String> {
    (0..FILES)
        .map(|index| format!("src/Type{index}.java"))
        .collect()
}

/// The binary under test, seeded with every file so each writer produces a
/// state with all entries and the file is big enough to be interesting.
fn filer(root: &Path, state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_filer"));
    command
        .arg("--root")
        .arg(root)
        .arg("bundle")
        .arg("--depth")
        .arg("0")
        .arg("--state")
        .arg(state);
    command
        .env("XDG_CACHE_HOME", root.join(".cache"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn parse_state(text: &str) -> toml::Value {
    toml::from_str(text).expect("state must never be torn")
}

fn version(value: &toml::Value) -> Option<i64> {
    value.get("version").and_then(toml::Value::as_integer)
}

#[test]
fn concurrent_writers_never_expose_a_partial_state() {
    let root = project("concurrent");
    let state = root.join(".filer/bundle.toml");
    let seeds = seeds();

    let mut children: Vec<_> = (0..6)
        .map(|_| {
            let mut command = filer(&root, &state);
            command.args(&seeds);
            command.spawn().expect("spawn writer")
        })
        .collect();

    let mut snapshots = 0;
    while children
        .iter_mut()
        .any(|child| child.try_wait().unwrap().is_none())
    {
        if let Ok(text) = fs::read_to_string(&state) {
            let value = parse_state(&text);
            assert_eq!(version(&value), Some(3));
            snapshots += 1;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    assert!(snapshots > 0, "the writers finished before a snapshot");

    let value = parse_state(&fs::read_to_string(&state).unwrap());
    assert_eq!(version(&value), Some(3));
    let bundles = value.get("bundles").and_then(toml::Value::as_array);
    assert_eq!(bundles.map(Vec::len), Some(1));
    assert_eq!(
        bundles.unwrap()[0]
            .get("entries")
            .and_then(toml::Value::as_array)
            .map(Vec::len),
        Some(FILES)
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn kill9_during_a_bundle_write_leaves_a_valid_state() {
    let root = project("kill9");
    let state = root.join(".filer/bundle.toml");
    let seeds = seeds();

    let mut command = filer(&root, &state);
    command.args(&seeds);
    assert!(command.status().unwrap().success());

    for round in 0..10 {
        let mut command = filer(&root, &state);
        command.args(&seeds);
        let mut child = command.spawn().expect("spawn writer");
        std::thread::sleep(Duration::from_millis(2 + round * 3));
        let _ = child.kill();
        let _ = child.wait();

        let text = fs::read_to_string(&state)
            .unwrap_or_else(|error| panic!("round {round}: state disappeared: {error}"));
        let value = parse_state(&text);
        assert_eq!(version(&value), Some(3), "round {round}");
    }

    // Leftover temp and lock files must not stop the next run from writing a
    // complete state.
    let mut command = filer(&root, &state);
    command.args(&seeds);
    assert!(command.status().unwrap().success());
    let value = parse_state(&fs::read_to_string(&state).unwrap());
    assert_eq!(version(&value), Some(3));
    assert_eq!(
        value
            .get("bundles")
            .and_then(toml::Value::as_array)
            .and_then(|bundles| bundles[0].get("entries"))
            .and_then(toml::Value::as_array)
            .map(Vec::len),
        Some(FILES)
    );

    let _ = fs::remove_dir_all(&root);
}
