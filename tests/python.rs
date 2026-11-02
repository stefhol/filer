//! End-to-end checks for Python support: indexing, the picker rows, import
//! resolution through packages and re-exports, and the call-path bundle.
//!
//! These run the real binary because the contract is about stdout, and because
//! the module layout (a file's package is its directory) only exists on disk.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-python-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("app/models")).unwrap();
    fs::write(dir.join("app/__init__.py"), "").unwrap();
    fs::write(
        dir.join("app/models/__init__.py"),
        "from .user import User\n",
    )
    .unwrap();
    fs::write(
        dir.join("app/models/user.py"),
        "class User:\n    name: str = \"\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("app/repo.py"),
        "from app.models import User\n\n\ndef load_all(users: list[User]) -> list[str]:\n    return [user.name for user in users]\n\n\nclass Repo:\n    def load(self, user: User) -> str:\n        return user.name\n\n    def spare(self) -> int:\n        return 0\n",
    )
    .unwrap();
    fs::write(
        dir.join("app/service.py"),
        "import json\n\nfrom app.models import User\nfrom app.repo import Repo\n\n\nclass Service:\n    def __init__(self, repo: Repo) -> None:\n        self.repo = repo\n\n    def greet(self, user: User) -> str:\n        return self.repo.load(user)\n\n    def unused(self) -> int:\n        return 1\n",
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
fn index_and_search_know_python() {
    let root = project("index");

    let index = object(&run(&root, &["--json", "index"]));
    assert_eq!(index["schema_version"], 1, "{index}");
    assert_eq!(index["source_files"], 5, "{index}");
    let python = index["languages"]
        .as_array()
        .expect("languages")
        .iter()
        .find(|row| row["language"] == "python")
        .unwrap_or_else(|| panic!("no python row: {index}"));
    assert_eq!(python["files"], 5, "{index}");

    // The picker offers a method row for a class method, with the package as
    // the class's FQN prefix.
    let rows: Vec<serde_json::Value> = run(&root, &["--json", "search", "--query", "greet"])
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(object)
        .collect();
    let greet = rows
        .iter()
        .find(|row| row["scope"] == "methods")
        .unwrap_or_else(|| panic!("no method row: {rows:?}"));
    assert_eq!(greet["path"], "app/service.py", "{greet}");
    assert_eq!(greet["fqn"], "app.Service.greet", "{greet}");
    assert_eq!(greet["symbol"], "Service", "{greet}");
    assert_eq!(greet["member"], "greet", "{greet}");
    assert_eq!(greet["line"], 11, "{greet}");

    // A module-level function is a symbol row, not a class row.
    let rows: Vec<serde_json::Value> = run(&root, &["--json", "search", "--query", "load_all"])
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(object)
        .collect();
    let load_all = rows
        .iter()
        .find(|row| row["symbol"] == "load_all")
        .unwrap_or_else(|| panic!("no function row: {rows:?}"));
    assert_eq!(load_all["scope"], "symbol", "{load_all}");
    assert_eq!(load_all["kind"], "function", "{load_all}");
    assert_eq!(load_all["fqn"], "load_all", "{load_all}");
    assert_eq!(load_all["path"], "app/repo.py", "{load_all}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn deps_resolves_packages_reexports_and_calls() {
    let root = project("deps");
    let deps = object(&run(
        &root,
        &["--json", "deps", "app/service.py::Service.greet", "--unit"],
    ));

    let seed = deps["deps"]
        .as_array()
        .expect("deps")
        .first()
        .expect("seed row");
    assert_eq!(seed["fqn"], "app.Service", "{deps}");

    let units: Vec<&str> = deps["deps"]
        .as_array()
        .expect("deps")
        .iter()
        .map(|row| row["unit"].as_str().expect("unit label"))
        .collect();
    assert!(units.contains(&"app/service.py::Service.greet"), "{deps}");
    // `self.repo.load(user)` — the field is typed by `repo: Repo`.
    assert!(units.contains(&"app/repo.py::Repo.load"), "{deps}");
    // Off the call path of `greet`, so not in the method closure.
    assert!(
        !units.iter().any(|unit| unit.ends_with(".unused")),
        "{deps}"
    );
    assert!(!units.iter().any(|unit| unit.ends_with(".spare")), "{deps}");

    let edges = deps["edges"].as_array().expect("edges");
    let call = edges
        .iter()
        .find(|edge| edge["to"] == "app/repo.py::Repo.load")
        .unwrap_or_else(|| panic!("call edge to Repo.load: {deps}"));
    assert_eq!(call["reason"], "call", "{deps}");
    assert_eq!(call["line"], 12, "{deps}");

    // `from app.models import User` re-exports it from `models/user.py`, so
    // nothing in the package chain is unresolved; the stdlib import is.
    let unresolved: Vec<&serde_json::Value> = deps["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|diagnostic| diagnostic["kind"] == "unresolved-import")
        .collect();
    assert_eq!(
        unresolved,
        vec![&serde_json::json!({
            "kind": "unresolved-import",
            "path": "app/service.py",
            "import": "json",
        })],
        "{deps}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn bundle_renders_python_fences_and_prunes_methods() {
    let root = project("bundle");
    let markdown = run(
        &root,
        &["bundle", "app/service.py::Service.greet", "--depth", "1"],
    );

    assert!(markdown.contains("```python"), "{markdown}");
    assert!(markdown.contains("app/service.py"), "{markdown}");
    assert!(markdown.contains("app/repo.py"), "{markdown}");
    assert!(markdown.contains("def greet"), "{markdown}");
    assert!(markdown.contains("def load"), "{markdown}");
    // The class shell keeps signatures, not the bodies of unused methods, and
    // the stub it leaves behind is still valid Python.
    assert!(
        markdown.contains("def unused(self) -> int: ...  # body omitted"),
        "{markdown}"
    );
    assert!(
        markdown.contains("def spare(self) -> int: ...  # body omitted"),
        "{markdown}"
    );
    assert!(!markdown.contains("return 1"), "{markdown}");
    assert!(!markdown.contains("return 0"), "{markdown}");

    let _ = fs::remove_dir_all(&root);
}
