//! End-to-end checks for `tsconfig.json` / `jsconfig.json` path aliases.
//!
//! These run the real binary: the contract is that a non-relative import that
//! a config maps to a project file lands in the closure, and that everything
//! the config does not map stays third-party.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-tsconfig-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
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

fn deps_paths(root: &Path, seed: &str) -> Vec<String> {
    let deps = object(&run(root, &["--json", "deps", seed]));
    deps["deps"]
        .as_array()
        .unwrap_or_else(|| panic!("deps array: {deps}"))
        .iter()
        .map(|row| row["path"].as_str().expect("path").to_string())
        .collect()
}

fn unresolved_imports(root: &Path, seed: &str) -> Vec<String> {
    let bundle = object(&run(root, &["--json", "bundle", seed]));
    bundle["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("diagnostics array: {bundle}"))
        .iter()
        .filter(|diagnostic| diagnostic["kind"] == "unresolved-import")
        .map(|diagnostic| diagnostic["import"].as_str().expect("import").to_string())
        .collect()
}

#[test]
fn tsconfig_aliases_pull_files_into_the_closure() {
    let root = project("aliases");
    // JSONC: comments and a trailing comma, as tsconfig files in the wild.
    write(
        &root,
        "tsconfig.json",
        r#"{
  // editor + bundler config
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@/*": ["src/*"],
      "~lib/*": ["src/lib/*"],
    },
  },
}
"#,
    );
    write(
        &root,
        "src/web/app.ts",
        "import { User } from \"@/model/user\";\nimport { format } from \"~lib/format\";\n\nexport function run(): string {\n  return format(new User(\"ada\").name);\n}\n",
    );
    write(
        &root,
        "src/model/user.ts",
        "export class User {\n  constructor(public name: string) {}\n}\n",
    );
    write(
        &root,
        "src/lib/format.ts",
        "export function format(value: string): string {\n  return value.trim();\n}\n",
    );

    let paths = deps_paths(&root, "src/web/app.ts");
    assert!(paths.contains(&"src/web/app.ts".to_string()), "{paths:?}");
    assert!(
        paths.contains(&"src/model/user.ts".to_string()),
        "{paths:?}"
    );
    assert!(
        paths.contains(&"src/lib/format.ts".to_string()),
        "{paths:?}"
    );
    assert!(unresolved_imports(&root, "src/web/app.ts").is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn tsconfig_extends_and_base_url_only_imports_resolve() {
    let root = project("extends");
    write(
        &root,
        "tsconfig.base.json",
        r#"{"compilerOptions": {"baseUrl": "src"}}"#,
    );
    write(
        &root,
        "tsconfig.json",
        r#"{"extends": "./tsconfig.base.json", "compilerOptions": {"paths": {"@/*": ["./*"]}}}"#,
    );
    write(
        &root,
        "src/app.ts",
        "import { Repo } from \"@/repo\";\nimport { helper } from \"lib/helper\";\n\nexport const run = () => helper(new Repo());\n",
    );
    write(&root, "src/repo.ts", "export class Repo {}\n");
    write(
        &root,
        "src/lib/helper.ts",
        "export function helper(value: unknown): unknown {\n  return value;\n}\n",
    );

    let paths = deps_paths(&root, "src/app.ts");
    assert!(paths.contains(&"src/repo.ts".to_string()), "{paths:?}");
    assert!(
        paths.contains(&"src/lib/helper.ts".to_string()),
        "{paths:?}"
    );
    assert!(unresolved_imports(&root, "src/app.ts").is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_nearest_config_wins_in_a_monorepo() {
    let root = project("monorepo");
    write(
        &root,
        "tsconfig.json",
        r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["shared/*"]}}}"#,
    );
    write(
        &root,
        "web/tsconfig.json",
        r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["lib/*"]}}}"#,
    );
    write(
        &root,
        "web/app.ts",
        "import { local } from \"@/local\";\n\nexport const value = local;\n",
    );
    write(&root, "web/lib/local.ts", "export const local = 1;\n");
    write(&root, "shared/local.ts", "export const local = 2;\n");

    let paths = deps_paths(&root, "web/app.ts");
    assert!(paths.contains(&"web/lib/local.ts".to_string()), "{paths:?}");
    assert!(!paths.contains(&"shared/local.ts".to_string()), "{paths:?}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn packages_and_out_of_root_aliases_stay_unresolved() {
    let root = project("unresolved");
    write(
        &root,
        "tsconfig.json",
        r#"{"compilerOptions": {"baseUrl": ".", "paths": {
            "@/*": ["src/*"],
            "~out/*": ["../outside/*"]
        }}}"#,
    );
    write(
        &root,
        "src/app.ts",
        "import { readFile } from \"node:fs\";\nimport { z } from \"zod\";\nimport { y } from \"~out/escape\";\n\nexport const run = () => readFile(z(y));\n",
    );
    write(&root, "src/local.ts", "export const local = 1;\n");
    // A `.java` file the config could point at must stay out of the Web family.
    write(
        &root,
        "src/model/Model.java",
        "package model;\n\npublic class Model {}\n",
    );

    let unresolved = unresolved_imports(&root, "src/app.ts");
    assert!(
        unresolved.contains(&"node:fs".to_string()),
        "{unresolved:?}"
    );
    assert!(unresolved.contains(&"zod".to_string()), "{unresolved:?}");
    assert!(
        unresolved.contains(&"~out/escape".to_string()),
        "{unresolved:?}"
    );
    assert_eq!(unresolved.len(), 3, "{unresolved:?}");

    let _ = fs::remove_dir_all(&root);
}
