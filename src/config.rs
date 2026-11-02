//! `.filer/config.toml`: the project's defaults, so an agent and a human do not
//! silently disagree about what "context" means.
//!
//! Every field is optional and the precedence is always
//! **flag > config file > built-in**. Unknown keys are an error, not a warning:
//! a typo in `depth` would otherwise change the bundle without a trace.
//!
//! ```toml
//! [bundle]
//! depth = 2
//! max_file_bytes = 1048576
//!
//! [index]
//! roots = ["src", "lib"]
//! include = []
//! exclude = ["**/generated/**"]
//! ignore = true
//! ```

use std::path::Path;

use serde::Deserialize;

use crate::error::FilerError;

/// Path of the config file, relative to the project root.
pub const FILE: &str = ".filer/config.toml";

/// Built-in depth when neither the flag nor the config sets one.
pub const DEFAULT_DEPTH: u32 = 2;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub bundle: BundleConfig,
    #[serde(default)]
    pub index: IndexConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleConfig {
    /// Default `--depth` for `bundle`.
    pub depth: Option<u32>,
    /// Default per-file size limit (`bundle --max-file-bytes`).
    pub max_file_bytes: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexConfig {
    /// Walk only these subdirectories of the root instead of the whole tree.
    pub roots: Option<Vec<String>>,
    /// Default `--include` globs. The flag replaces the list, it does not append.
    pub include: Option<Vec<String>>,
    /// Default `--exclude` globs. The flag replaces the list, it does not append.
    pub exclude: Option<Vec<String>>,
    /// Honor `.gitignore`/`.ignore` files. `--no-ignore` still wins.
    pub ignore: Option<bool>,
}

/// Reads `<root>/.filer/config.toml`. A missing file is the empty config; an
/// unreadable or invalid one is an error, because silently falling back to the
/// built-ins is exactly the disagreement this file exists to prevent.
pub fn load(root: &Path) -> Result<Config, FilerError> {
    let path = root.join(FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => {
            return Err(FilerError::internal(format!(
                "cannot read {}: {error}",
                path.display()
            )))
        }
    };
    toml::from_str(&text).map_err(|error| {
        FilerError::internal(format!("cannot parse {}: {error}", path.display())).with_hint(
            "known keys: [bundle] depth, max_file_bytes; [index] roots, include, exclude, ignore",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_document_is_the_built_in_config() {
        let config: Config = toml::from_str("").expect("empty config parses");
        assert!(config.bundle.depth.is_none());
        assert!(config.index.exclude.is_none());
    }

    #[test]
    fn every_documented_key_parses() {
        let config: Config = toml::from_str(
            r#"
            [bundle]
            depth = 1
            max_file_bytes = 2048

            [index]
            roots = ["src"]
            include = ["**/*.java"]
            exclude = ["**/generated/**"]
            ignore = false
            "#,
        )
        .expect("documented config parses");
        assert_eq!(config.bundle.depth, Some(1));
        assert_eq!(config.bundle.max_file_bytes, Some(2048));
        assert_eq!(config.index.roots, Some(vec!["src".to_string()]));
        assert_eq!(config.index.ignore, Some(false));
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        let error = toml::from_str::<Config>("[bundle]\ndepht = 2\n")
            .expect_err("typo must not be ignored");
        assert!(error.to_string().contains("depht"), "{error}");
    }
}
