//! Per-branch bundle state: which files are already bundled on each git branch,
//! why, and when. Stored as TOML next to the project (`.filer/bundle.toml`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const VERSION: u32 = 2;

/// The whole state file: one bundle per branch.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StateDoc {
    pub root: String,
    #[serde(default)]
    pub version: u32,
    /// One entry per git branch. An empty `name` means "not a git repo".
    #[serde(default)]
    pub bundles: Vec<BranchBundle>,

    // Legacy single-bundle layout (version 1), read and migrated on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<Vec<StateFile>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BranchBundle {
    pub name: String,
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
    /// Top-level declaration that was bundled, when the entry is an excerpt
    /// (`path::symbol`). `None` means the whole file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default)]
    pub depth: u32,
    /// `seed`, `import`, `static-import`, `wildcard-import`, `same-package`,
    /// `usage`, `package`, `config`, `external`.
    #[serde(default)]
    pub reason: String,
}

impl StateDoc {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_string_lossy().to_string(),
            version: VERSION,
            bundles: Vec::new(),
            ..Default::default()
        }
    }

    pub fn bundle(&self, branch: &str) -> Option<&BranchBundle> {
        self.bundles.iter().find(|bundle| bundle.name == branch)
    }

    /// Inserts or replaces the bundle for its branch, keeping branches sorted.
    pub fn upsert(&mut self, bundle: BranchBundle) {
        match self
            .bundles
            .iter()
            .position(|existing| existing.name == bundle.name)
        {
            Some(position) => self.bundles[position] = bundle,
            None => self.bundles.push(bundle),
        }
        self.bundles.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn remove(&mut self, branch: &str) -> bool {
        let before = self.bundles.len();
        self.bundles.retain(|bundle| bundle.name != branch);
        self.bundles.len() != before
    }

    pub fn is_empty(&self) -> bool {
        self.bundles.is_empty()
    }

    /// Converts a version 1 state file (single branch at the top level).
    fn migrate(&mut self) {
        self.version = VERSION;
        if let Some(files) = self.files.take() {
            if !files.is_empty() || self.branch.is_some() {
                let bundle = BranchBundle {
                    name: self.branch.take().unwrap_or_default(),
                    created: self.created.take().unwrap_or(0),
                    depth: self.depth.take().unwrap_or(0),
                    files,
                };
                self.upsert(bundle);
            }
        }
        self.branch = None;
        self.created = None;
        self.depth = None;
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub fn default_path(root: &Path) -> PathBuf {
    root.join(".filer").join("bundle.toml")
}

pub fn load(path: &Path) -> Option<StateDoc> {
    let text = fs::read_to_string(path).ok()?;
    let mut doc: StateDoc = toml::from_str(&text).ok()?;
    doc.migrate();
    Some(doc)
}

pub fn save(path: &Path, doc: &StateDoc) -> std::io::Result<()> {
    if doc.is_empty() {
        return clear(path).map(|_| ());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        // Keep the state out of `git status` without touching the user's
        // .gitignore: a `*` ignore file inside the directory ignores itself too.
        let ignore = parent.join(".gitignore");
        if !ignore.exists() {
            let _ = fs::write(ignore, "*\n");
        }
    }
    fs::write(path, to_toml(doc))
}

pub fn clear(path: &Path) -> std::io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// State file contents, with a hint for whoever opens the file directly.
pub fn to_toml(doc: &StateDoc) -> String {
    let body = to_toml_plain(doc);
    format!(
        "# filer bundle state — `:FilerReset` clears the current branch, `:FilerResetAll` everything\n{body}"
    )
}

/// The same document without the human hint, for bundles that get pasted into
/// a chat: data only.
pub fn to_toml_plain(doc: &StateDoc) -> String {
    toml::to_string_pretty(doc).unwrap_or_default()
}

/// Branch name for the project at `root`, read from `.git/HEAD` (no subprocess).
/// Returns an empty string outside a git repository or on a read error.
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

/// Human-readable branch label, also used as the TOML key.
pub fn branch_label(branch: &str) -> &str {
    if branch.is_empty() {
        "(no branch)"
    } else {
        branch
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

    fn file(path: &str, depth: u32, reason: &str) -> StateFile {
        StateFile {
            path: path.to_string(),
            symbol: None,
            depth,
            reason: reason.to_string(),
        }
    }

    #[test]
    fn keeps_one_bundle_per_branch() {
        let mut doc = StateDoc::new(Path::new("/repo"));
        for (branch, path) in [("main", "A.java"), ("ticket-42", "B.java")] {
            doc.upsert(BranchBundle {
                name: branch.to_string(),
                created: 7,
                depth: 2,
                files: vec![file(path, 0, "seed")],
            });
        }

        let parsed: StateDoc = toml::from_str(&to_toml(&doc)).expect("valid toml");
        assert_eq!(parsed.version, VERSION);
        assert_eq!(parsed.bundles.len(), 2);
        assert_eq!(parsed.bundle("main").unwrap().files[0].path, "A.java");
        assert_eq!(parsed.bundle("ticket-42").unwrap().files[0].path, "B.java");
        assert!(parsed.bundle("other").is_none());
    }

    #[test]
    fn symbol_entries_round_trip() {
        let mut doc = StateDoc::new(Path::new("/repo"));
        doc.upsert(BranchBundle {
            name: "main".to_string(),
            created: 1,
            depth: 2,
            files: vec![
                StateFile {
                    path: "main.fe".to_string(),
                    symbol: Some("riskValue".to_string()),
                    depth: 0,
                    reason: "seed".to_string(),
                },
                file("lib.fe", 1, "usage"),
            ],
        });

        let text = to_toml(&doc);
        assert!(text.contains("symbol = \"riskValue\""), "{text}");
        let parsed: StateDoc = toml::from_str(&text).expect("valid toml");
        let files = &parsed.bundle("main").unwrap().files;
        assert_eq!(files[0].symbol.as_deref(), Some("riskValue"));
        assert_eq!(files[1].symbol, None);
    }

    #[test]
    fn reset_only_touches_one_branch() {
        let mut doc = StateDoc::new(Path::new("/repo"));
        for branch in ["main", "ticket-42"] {
            doc.upsert(BranchBundle {
                name: branch.to_string(),
                depth: 1,
                files: vec![file("A.java", 0, "seed")],
                ..Default::default()
            });
        }

        assert!(doc.remove("ticket-42"));
        assert!(!doc.remove("ticket-42"));
        assert!(doc.bundle("main").is_some());
        assert_eq!(doc.bundles.len(), 1);
    }

    #[test]
    fn migrates_version_one_state() {
        let legacy = r#"
            root = "/repo"
            branch = "main"
            created = 11
            depth = 2

            [[files]]
            path = "A.java"
            depth = 0
            reason = "seed"
        "#;
        let mut doc: StateDoc = toml::from_str(legacy).expect("legacy toml");
        doc.migrate();
        assert_eq!(doc.version, VERSION);
        assert_eq!(doc.bundles.len(), 1);
        assert_eq!(doc.bundles[0].name, "main");
        assert_eq!(doc.bundles[0].created, 11);
        assert_eq!(doc.bundles[0].files[0].path, "A.java");
        // Re-serializing must not write the legacy fields again.
        let text = to_toml(&doc);
        assert!(!text.contains("branch = "), "legacy field leaked: {text}");
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
}
