//! `tsconfig.json` / `jsconfig.json` reading: `compilerOptions.baseUrl` and
//! `compilerOptions.paths` aliases, plus relative `extends` chains.
//!
//! TypeScript configs are JSONC, so they are parsed with a JSON5 parser
//! (comments and trailing commas are both accepted). Only what module
//! resolution needs is kept: the config directory, the effective `baseUrl` and
//! the `paths` map with every target already resolved to a root-relative path.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::index::FileEntry;

/// Config file names probed in every source directory, in precedence order.
const CONFIG_NAMES: &[&str] = &["tsconfig.json", "jsconfig.json"];

/// How deep an `extends` chain may go before it is treated as a cycle.
const MAX_EXTENDS_DEPTH: usize = 8;

/// One config file reduced to what module resolution needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TsConfig {
    /// Directory holding the config, relative to the project root (`""` at the
    /// root). This is the scope the config applies to.
    pub dir: String,
    /// Effective `baseUrl`, root-relative (`""` means the project root).
    pub base_url: Option<String>,
    /// Effective `paths`: alias key -> root-relative targets, in order. Keys
    /// may contain a single `*`; targets keep the `*` placeholder.
    pub paths: BTreeMap<String, Vec<String>>,
}

impl TsConfig {
    /// Candidate module bases for a non-relative specifier, best match first:
    /// an exact `paths` key, else the longest matching wildcard key, else
    /// `baseUrl` joined with the specifier itself. TypeScript tries exactly one
    /// of the three: a `paths` pattern that matches replaces the `baseUrl`
    /// lookup instead of adding to it.
    pub fn candidates(&self, specifier: &str) -> Vec<String> {
        if let Some(targets) = self.paths.get(specifier) {
            return targets.clone();
        }

        // TypeScript precedence: the wildcard key with the longest prefix wins.
        let mut best: Option<(&str, &str, &str)> = None;
        for key in self.paths.keys() {
            let Some(star) = key.find('*') else {
                continue;
            };
            let (prefix, suffix) = (&key[..star], &key[star + 1..]);
            if specifier.len() < prefix.len() + suffix.len()
                || !specifier.starts_with(prefix)
                || !specifier.ends_with(suffix)
            {
                continue;
            }
            if best.is_none_or(|(_, best_prefix, _)| prefix.len() > best_prefix.len()) {
                best = Some((key, prefix, suffix));
            }
        }
        if let Some((key, prefix, suffix)) = best {
            let tail = &specifier[prefix.len()..specifier.len() - suffix.len()];
            return self
                .paths
                .get(key)
                .map(|targets| {
                    targets
                        .iter()
                        .map(|target| target.replacen('*', tail, 1))
                        .collect()
                })
                .unwrap_or_default();
        }

        // No alias matched: with a `baseUrl` the specifier is a root-relative
        // path (`import { x } from "src/lib/x"`).
        self.base_url
            .as_ref()
            .and_then(|base_url| join_relative(base_url, specifier))
            .into_iter()
            .collect()
    }

    /// The nearest config for `from_rel`: the config in the importing file's
    /// directory, else the one in the closest ancestor directory.
    pub fn nearest<'a>(configs: &'a [TsConfig], from_rel: &str) -> Option<&'a TsConfig> {
        let mut dir = parent_dir(from_rel);
        loop {
            if let Some(config) = configs.iter().find(|config| config.dir == dir) {
                return Some(config);
            }
            if dir.is_empty() {
                return None;
            }
            dir = parent_dir(&dir);
        }
    }
}

/// Reads every `tsconfig.json`/`jsconfig.json` that can be the nearest config
/// for an indexed file. Only directories on the path from the root down to an
/// indexed file are probed, so a big monorepo costs one `stat` per directory
/// instead of a second walk. Configs are cheap and re-read on every build, so
/// they are not part of the index cache.
pub fn collect(root: &Path, files: &[FileEntry]) -> Vec<TsConfig> {
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for file in files {
        let mut dir = parent_dir(&file.rel);
        loop {
            let is_new = dirs.insert(dir.clone());
            if !is_new || dir.is_empty() {
                break;
            }
            dir = parent_dir(&dir);
        }
    }

    let mut configs: Vec<TsConfig> = Vec::new();
    for dir in dirs {
        for name in CONFIG_NAMES {
            let rel = if dir.is_empty() {
                (*name).to_string()
            } else {
                format!("{dir}/{name}")
            };
            if let Some(config) = load(root, &rel, 0, &mut Vec::new()) {
                configs.push(config);
            }
        }
    }
    configs
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    extends: Option<ExtendsValue>,
    #[serde(default, rename = "compilerOptions")]
    compiler_options: Option<RawCompilerOptions>,
}

/// `extends` is a string, and since TypeScript 5.0 also an array of strings.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ExtendsValue {
    One(String),
    Many(Vec<String>),
}

impl ExtendsValue {
    fn values(&self) -> Vec<&str> {
        match self {
            ExtendsValue::One(value) => vec![value.as_str()],
            ExtendsValue::Many(values) => values.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCompilerOptions {
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    paths: Option<BTreeMap<String, Vec<String>>>,
}

/// Parses one config, folding in its relative `extends` chain. `rel` is the
/// config path relative to the project root, so `baseUrl` and `paths` targets
/// can be stored root-relative right away.
fn load(root: &Path, rel: &str, depth: usize, seen: &mut Vec<String>) -> Option<TsConfig> {
    if depth > MAX_EXTENDS_DEPTH || seen.iter().any(|visited| visited == rel) {
        return None;
    }
    let text = fs::read_to_string(root.join(rel)).ok()?;
    let raw: RawConfig = match json5::from_str(&text) {
        Ok(raw) => raw,
        // A broken config is a project problem, not a filer failure: imports
        // through it stay unresolved, everything else keeps working.
        Err(error) => {
            eprintln!("filer: ignoring unparsable {rel}: {error}");
            return None;
        }
    };

    seen.push(rel.to_string());
    let dir = parent_dir(rel);
    let mut config = TsConfig {
        dir: dir.clone(),
        ..TsConfig::default()
    };

    // Inherited values come first; a later base (extends array) overrides an
    // earlier one, and the child overrides both.
    for value in raw
        .extends
        .as_ref()
        .map(ExtendsValue::values)
        .unwrap_or_default()
    {
        let Some(base_rel) = extends_candidates(&dir, value)
            .into_iter()
            .find(|candidate| root.join(candidate).is_file())
        else {
            continue;
        };
        let Some(base) = load(root, &base_rel, depth + 1, seen) else {
            continue;
        };
        if base.base_url.is_some() {
            config.base_url = base.base_url;
        }
        if !base.paths.is_empty() {
            config.paths = base.paths;
        }
    }

    if let Some(options) = raw.compiler_options {
        if let Some(base_url) = options.base_url {
            // A `baseUrl` that points outside the project is ignored, matching
            // the rule that aliases may not escape the root.
            config.base_url = join_relative(&dir, &base_url);
        }
        if let Some(paths) = options.paths {
            // TypeScript replaces inherited `paths` wholesale when a config
            // declares its own map, and resolves targets against `baseUrl`
            // (or the declaring config's directory when there is none).
            let base = config.base_url.clone().unwrap_or_else(|| dir.clone());
            config.paths = paths
                .into_iter()
                .filter_map(|(key, targets)| {
                    let targets: Vec<String> = targets
                        .iter()
                        .filter_map(|target| join_relative(&base, target))
                        .collect();
                    (!targets.is_empty()).then_some((key, targets))
                })
                .collect();
        }
    }

    seen.pop();
    Some(config)
}

/// Candidate paths for a relative `extends` value. Package-name extends
/// (`@tsconfig/node20`) is out of scope: those live in `node_modules`.
fn extends_candidates(dir: &str, value: &str) -> Vec<String> {
    if !(value.starts_with("./") || value.starts_with("../")) {
        return Vec::new();
    }
    let mut values = vec![value.to_string()];
    if !value.ends_with(".json") {
        values.push(format!("{value}.json"));
    }
    values
        .into_iter()
        .filter_map(|value| join_relative(dir, &value))
        .collect()
}

/// Directory part of a root-relative path (`""` for top-level entries).
fn parent_dir(rel: &str) -> String {
    match rel.rfind('/') {
        Some(slash) => rel[..slash].to_string(),
        None => String::new(),
    }
}

/// Joins a config-relative path onto a root-relative directory, collapsing
/// `.` and `..`. Returns `None` when the result is absolute or escapes the
/// project root.
fn join_relative(base: &str, path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path.starts_with('/') || path.contains(':') {
        return None;
    }
    let mut parts: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            segment => parts.push(segment),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileEntry;
    use std::path::PathBuf;

    fn entry(rel: &str) -> FileEntry {
        FileEntry {
            rel: rel.to_string(),
            abs: PathBuf::from(format!("/repo/{rel}")),
            package: None,
            types: Vec::new(),
            imports: Vec::new(),
            uses: Vec::new(),
            symbols: Vec::new(),
            preamble_end: 0,
            step_defs: Vec::new(),
            size: 0,
            end_line: 0,
        }
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    /// A throwaway project rooted in the temp dir.
    fn project(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("filer-tsconfig-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_parse() {
        let root = project("jsonc");
        write(
            &root.join("tsconfig.json"),
            r#"// JSONC, not JSON.
{
  /* block comment with a url: https://example.com/x */
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@/*": ["src/*"], // trailing comma next
    },
  },
}
"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        let config = TsConfig::nearest(&configs, "src/app.ts").expect("config");
        assert_eq!(config.base_url.as_deref(), Some(""));
        assert_eq!(config.paths["@/*"], vec!["src/*".to_string()]);
    }

    #[test]
    fn exact_key_wins_over_wildcard() {
        let root = project("exact");
        write(
            &root.join("tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {
                "@/*": ["src/*"],
                "@/special": ["src/special/exact.ts"]
            }}}"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        let config = TsConfig::nearest(&configs, "src/app.ts").expect("config");
        assert_eq!(config.candidates("@/special"), vec!["src/special/exact.ts"]);
    }

    #[test]
    fn wildcard_key_with_longest_prefix_wins() {
        let root = project("longest");
        write(
            &root.join("tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {
                "@/*": ["src/*"],
                "@/lib/*": ["src/lib/*"]
            }}}"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        let config = TsConfig::nearest(&configs, "src/app.ts").expect("config");
        assert_eq!(config.candidates("@/lib/x"), vec!["src/lib/x".to_string()]);
    }

    #[test]
    fn multiple_targets_keep_their_order() {
        let root = project("targets");
        write(
            &root.join("tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {
                "@/*": ["src/*", "generated/*"]
            }}}"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        let config = TsConfig::nearest(&configs, "src/app.ts").expect("config");
        assert_eq!(
            config.candidates("@/x"),
            vec!["src/x".to_string(), "generated/x".to_string()]
        );
    }

    #[test]
    fn base_url_only_candidates_and_equivalents() {
        let root = project("baseurl");
        write(
            &root.join("tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": "src"}}"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        let config = TsConfig::nearest(&configs, "src/app.ts").expect("config");
        assert_eq!(config.candidates("lib/x"), vec!["src/lib/x".to_string()]);
    }

    #[test]
    fn extends_chain_is_followed() {
        let root = project("extends");
        write(
            &root.join("tsconfig.base.json"),
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}"#,
        );
        write(
            &root.join("tsconfig.json"),
            r#"{"extends": "./tsconfig.base.json", "compilerOptions": {"paths": {"~lib/*": ["src/lib/*"]}}}"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        let config = TsConfig::nearest(&configs, "src/app.ts").expect("config");
        // `paths` in the child replaces the inherited map; `baseUrl` is kept.
        assert_eq!(config.base_url.as_deref(), Some(""));
        assert_eq!(config.candidates("~lib/x"), vec!["src/lib/x".to_string()]);
        assert_eq!(config.candidates("@/x"), vec!["@/x".to_string()]);
    }

    #[test]
    fn extends_without_extensions_and_without_own_base_url() {
        let root = project("extends-baseurl");
        write(
            &root.join("config/base.json"),
            r#"{"compilerOptions": {"baseUrl": "../src", "paths": {"@/*": ["./*"]}}}"#,
        );
        write(
            &root.join("config/tsconfig.json"),
            r#"{"extends": "./base"}"#,
        );
        let configs = collect(&root, &[entry("config/app.ts")]);
        let config = TsConfig::nearest(&configs, "config/app.ts").expect("config");
        assert_eq!(config.base_url.as_deref(), Some("src"));
        assert_eq!(config.candidates("@/x"), vec!["src/x".to_string()]);
    }

    #[test]
    fn extends_cycle_is_not_fatal() {
        let root = project("cycle");
        write(
            &root.join("tsconfig.json"),
            r#"{"extends": "./tsconfig.json", "compilerOptions": {"baseUrl": "."}}"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        let config = TsConfig::nearest(&configs, "src/app.ts").expect("config");
        assert_eq!(config.base_url.as_deref(), Some(""));
    }

    #[test]
    fn package_name_extends_is_skipped() {
        let root = project("package-extends");
        write(
            &root.join("tsconfig.json"),
            r#"{"extends": "@tsconfig/node20/tsconfig.json"}"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        let config = TsConfig::nearest(&configs, "src/app.ts").expect("config");
        assert_eq!(config.base_url, None);
        assert!(config.paths.is_empty());
    }

    #[test]
    fn nearest_config_wins_per_directory() {
        let root = project("nearest");
        write(
            &root.join("tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}"#,
        );
        write(
            &root.join("web/tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["lib/*"]}}}"#,
        );
        let files = [entry("src/app.ts"), entry("web/app.ts")];
        let configs = collect(&root, &files);
        assert_eq!(configs.len(), 2);

        let root_config = TsConfig::nearest(&configs, "src/app.ts").expect("root config");
        assert_eq!(root_config.dir, "");
        assert_eq!(root_config.candidates("@/x")[0], "src/x");

        let web_config = TsConfig::nearest(&configs, "web/deep/app.ts").expect("web config");
        assert_eq!(web_config.dir, "web");
        assert_eq!(web_config.candidates("@/x")[0], "web/lib/x");
    }

    #[test]
    fn jsconfig_is_read_when_no_tsconfig_exists() {
        let root = project("jsconfig");
        write(
            &root.join("jsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": "."}}"#,
        );
        let configs = collect(&root, &[entry("app.js")]);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].candidates("lib/x"), vec!["lib/x".to_string()]);
    }

    #[test]
    fn targets_escaping_the_root_are_dropped() {
        let root = project("escape");
        write(
            &root.join("tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {
                "@/*": ["../outside/*", "src/*"],
                "~lib/*": ["../outside/lib/*"]
            }}}"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        let config = TsConfig::nearest(&configs, "src/app.ts").expect("config");
        assert_eq!(config.candidates("@/x")[0], "src/x");
        assert_eq!(config.candidates("~lib/x"), vec!["~lib/x".to_string()]);
    }

    #[test]
    fn escaping_base_url_is_ignored() {
        let root = project("escape-baseurl");
        write(
            &root.join("tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": "../outside"}}"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        let config = TsConfig::nearest(&configs, "src/app.ts").expect("config");
        assert_eq!(config.base_url, None);
        assert!(config.candidates("lib/x").is_empty());
    }

    #[test]
    fn unparsable_config_leaves_imports_unresolved() {
        let root = project("broken");
        write(&root.join("tsconfig.json"), "{ not json at all ");
        let configs = collect(&root, &[entry("src/app.ts")]);
        assert!(configs.is_empty());
    }

    #[test]
    fn directories_without_indexed_files_are_not_probed() {
        let root = project("unindexed");
        write(
            &root.join("docs/tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": "."}}"#,
        );
        let configs = collect(&root, &[entry("src/app.ts")]);
        assert!(configs.is_empty());
    }
}
