//! Which files the index walks: `.gitignore`/`.ignore` files plus the
//! `--include`/`--exclude` globs.
//!
//! Two layers, on purpose:
//!
//! - Ignore files are the project's own answer to "what is not source". They are
//!   honored by default (`.gitignore`, `.git/info/exclude`, the global
//!   `core.excludesFile` and `.ignore`), and `--no-ignore` turns the whole layer
//!   off for a run. Directories in [`crate::index::SKIP_DIRS`] stay skipped either
//!   way: they are build output by definition.
//! - `--include`/`--exclude` globs are the caller's ad-hoc view, e.g. "only
//!   `src/**`" or "not `**/legacy/**`". They apply to files, never prune
//!   directories, and `--exclude` wins over `--include`.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::error::FilerError;

#[derive(Debug)]
pub struct Filter {
    /// Honor `.gitignore` and friends. `--no-ignore` clears it.
    ignore_files: bool,
    /// Empty means "every path the walk found".
    include_empty: bool,
    /// Globs containing a `/`: matched against the relative path.
    include_paths: GlobSet,
    /// Globs without a `/`: matched against the file name at any depth.
    include_names: GlobSet,
    exclude_paths: GlobSet,
    exclude_names: GlobSet,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            ignore_files: true,
            include_empty: true,
            include_paths: GlobSet::empty(),
            include_names: GlobSet::empty(),
            exclude_paths: GlobSet::empty(),
            exclude_names: GlobSet::empty(),
        }
    }
}

impl Filter {
    pub fn new(
        include: &[String],
        exclude: &[String],
        ignore_files: bool,
    ) -> Result<Self, FilerError> {
        let (include_paths, include_names) = compile(include)?;
        let (exclude_paths, exclude_names) = compile(exclude)?;
        Ok(Self {
            ignore_files,
            include_empty: include.is_empty(),
            include_paths,
            include_names,
            exclude_paths,
            exclude_names,
        })
    }

    /// Whether the walk should read `.gitignore` and `.ignore` files.
    pub fn ignore_files(&self) -> bool {
        self.ignore_files
    }

    /// `rel` is project-relative and uses `/` separators.
    pub fn allows(&self, rel: &str) -> bool {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        if self.exclude_paths.is_match(rel) || self.exclude_names.is_match(name) {
            return false;
        }
        if self.include_empty {
            return true;
        }
        self.include_paths.is_match(rel) || self.include_names.is_match(name)
    }
}

/// Split the patterns into "has a path separator" (whole path) and "just a
/// name" (any depth), so `--exclude '*.md'` behaves like the same line in a
/// `.gitignore` while `--include 'src/**'` stays anchored at the root.
///
/// `literal_separator` keeps `*` inside one path segment: `src/*.java` does not
/// reach `src/app/Service.java`; `**` still crosses segments.
fn compile(patterns: &[String]) -> Result<(GlobSet, GlobSet), FilerError> {
    let mut paths = GlobSetBuilder::new();
    let mut names = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|error| {
                FilerError::internal(format!("invalid glob {pattern:?}: {error}"))
                    .with_hint("globs are path globs: `src/**/*.java`, `*.md`, `**/legacy/**`")
            })?;
        if pattern.contains('/') {
            paths.add(glob);
        } else {
            names.add(glob);
        }
    }
    let build = |builder: GlobSetBuilder| {
        builder
            .build()
            .map_err(|error| FilerError::internal(format!("cannot compile globs: {error}")))
    };
    Ok((build(paths)?, build(names)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(include: &[&str], exclude: &[&str]) -> Filter {
        Filter::new(
            &include
                .iter()
                .map(|glob| glob.to_string())
                .collect::<Vec<_>>(),
            &exclude
                .iter()
                .map(|glob| glob.to_string())
                .collect::<Vec<_>>(),
            true,
        )
        .expect("globs compile")
    }

    #[test]
    fn the_default_filter_allows_everything() {
        let filter = Filter::default();
        assert!(filter.allows("app/Service.java"));
        assert!(filter.allows("anything/at/all/x.fe"));
        assert!(filter.ignore_files());
    }

    #[test]
    fn an_include_glob_restricts_to_matching_files() {
        let filter = filter(&["src/**"], &[]);
        assert!(filter.allows("src/app/Service.java"));
        assert!(filter.allows("src/main.fe"));
        assert!(!filter.allows("app/Service.java"));
        assert!(
            !filter.allows("srcs/app/Service.java"),
            "a prefix is not a match"
        );
    }

    #[test]
    fn a_name_include_glob_matches_at_any_depth() {
        let filter = filter(&["*.fe"], &[]);
        assert!(filter.allows("main.fe"));
        assert!(filter.allows("lib/deep/deeper/util.fe"));
        assert!(!filter.allows("lib/deep/util.java"));
    }

    #[test]
    fn exclude_wins_over_include_and_star_stays_in_one_segment() {
        let filter = filter(&["src/**"], &["src/*.java", "**/legacy/**", "*.md"]);
        assert!(filter.allows("src/app/Service.java"));
        assert!(
            !filter.allows("src/Service.java"),
            "src/*.java is one level"
        );
        assert!(!filter.allows("src/app/legacy/Old.java"));
        assert!(!filter.allows("src/README.md"));
    }

    #[test]
    fn a_broken_glob_is_an_internal_error_with_a_hint() {
        let error = Filter::new(&[needs_escaping()], &[], true).expect_err("bad glob");
        assert_eq!(error.code, crate::error::Code::Internal);
        assert!(error.hint.is_some());
    }

    fn needs_escaping() -> String {
        "[".to_string()
    }
}
