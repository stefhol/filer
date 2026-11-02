//! Persistent bundle state: which files are already in the bundle, why, and for
//! which branch. Stored as TOML next to the project (`.filer/bundle.toml`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BundleState {
    pub root: String,
    /// Git branch the state was written on. Empty when not in a git repo.
    #[serde(default)]
    pub branch: String,
    /// Unix seconds.
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub depth: u32,
    #[serde(default)]
    pub files: Vec<StateFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFile {
    pub path: String,
    #[serde(default)]
    pub depth: u32,
    /// `seed`, `import`, `static-import`, `wildcard-import`, `same-package`,
    /// `usage`, `package`, `config`, `external`.
    #[serde(default)]
    pub reason: String,
}

impl BundleState {
    pub fn new(root: &Path, depth: u32) -> Self {
        Self {
            root: root.to_string_lossy().to_string(),
            branch: current_branch(root),
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
            depth,
            files: Vec::new(),
        }
    }

    /// True when the state was written on a different branch than the current
    /// one, i.e. after switching branches the old bundle is meaningless.
    pub fn is_stale(&self, root: &Path) -> bool {
        !self.branch.is_empty() && self.branch != current_branch(root)
    }
}

pub fn default_path(root: &Path) -> PathBuf {
    root.join(".filer").join("bundle.toml")
}

pub fn load(path: &Path) -> Option<BundleState> {
    let text = fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

pub fn save(path: &Path, state: &BundleState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        // Keep the state out of `git status` without touching the user's
        // .gitignore: a `*` ignore file inside the directory ignores itself too.
        let ignore = parent.join(".gitignore");
        if !ignore.exists() {
            let _ = fs::write(ignore, "*\n");
        }
    }
    fs::write(path, to_toml(state))
}

pub fn clear(path: &Path) -> std::io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn to_toml(state: &BundleState) -> String {
    let body = toml::to_string_pretty(state).unwrap_or_default();
    format!("# filer bundle state — `:FilerReset` or `filer state --clear` removes it\n{body}")
}

/// Reads `.git/HEAD` by walking up from `root`, without spawning git.
pub fn current_branch(root: &Path) -> String {
    let Some(head) = head_file(root) else {
        return String::new();
    };
    let Ok(text) = fs::read_to_string(head) else {
        return String::new();
    };
    let text = text.trim();
    match text.strip_prefix("ref: ") {
        Some(reference) => reference.rsplit('/').next().unwrap_or_default().to_string(),
        None => format!("detached@{}", &text[..text.len().min(7)]),
    }
}

fn head_file(root: &Path) -> Option<PathBuf> {
    let mut directory = Some(root);
    while let Some(current) = directory {
        let dot_git = current.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git.join("HEAD"));
        }
        if dot_git.is_file() {
            // Worktree or submodule: `.git` is a file containing `gitdir: ...`.
            let text = fs::read_to_string(&dot_git).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            let gitdir = if Path::new(target).is_absolute() {
                PathBuf::from(target)
            } else {
                current.join(target)
            };
            return Some(gitdir.join("HEAD"));
        }
        directory = current.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let mut state = BundleState::new(Path::new("/repo"), 2);
        state.branch = "feature/x".to_string();
        state.files.push(StateFile {
            path: "src/main/java/app/A.java".to_string(),
            depth: 0,
            reason: "seed".to_string(),
        });
        state.files.push(StateFile {
            path: "src/main/java/app/B.java".to_string(),
            depth: 1,
            reason: "same-package".to_string(),
        });

        let parsed: BundleState = toml::from_str(&to_toml(&state)).expect("valid toml");
        assert_eq!(parsed.branch, "feature/x");
        assert_eq!(parsed.depth, 2);
        assert_eq!(parsed.files.len(), 2);
        assert_eq!(parsed.files[1].reason, "same-package");
    }

    #[test]
    fn branch_is_read_from_head_file() {
        let temporary = std::env::temp_dir().join(format!("filer-branch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temporary);
        fs::create_dir_all(temporary.join(".git")).unwrap();
        fs::write(temporary.join(".git/HEAD"), "ref: refs/heads/ticket-42\n").unwrap();
        assert_eq!(current_branch(&temporary), "ticket-42");

        fs::write(
            temporary.join(".git/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        assert!(current_branch(&temporary).starts_with("detached@0123456"));

        let _ = fs::remove_dir_all(&temporary);
    }

    #[test]
    fn stale_after_branch_change() {
        let temporary = std::env::temp_dir().join(format!("filer-stale-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temporary);
        fs::create_dir_all(temporary.join(".git")).unwrap();
        fs::write(temporary.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let state = BundleState::new(&temporary, 2);
        assert!(!state.is_stale(&temporary));

        fs::write(temporary.join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
        assert!(state.is_stale(&temporary));

        let _ = fs::remove_dir_all(&temporary);
    }
}
