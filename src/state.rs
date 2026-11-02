//! Per-branch bundle state: what each branch's bundle contains, in which
//! granularity, why, and when. Stored as TOML next to the project
//! (`.filer/bundle.toml`).
//!
//! `version = 3` makes the granularity explicit: every entry has a
//! [`Selection`] and, depending on it, a symbol and a set of methods. Version 1
//! (single branch) and version 2 (implicit `path`/`symbol`/`member`) files are
//! read through [`StoredDoc`] and migrated on load.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::atomic;
use crate::error::FilerError;

const VERSION: u32 = 3;

/// Granularity of one bundle entry / picker row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Selection {
    /// The whole file.
    #[default]
    File,
    /// One top-level declaration (class, type, `.fe` function, ...).
    Symbol,
    /// A class shell plus the listed methods.
    Methods,
}

impl Selection {
    pub fn as_str(self) -> &'static str {
        match self {
            Selection::File => "file",
            Selection::Symbol => "symbol",
            Selection::Methods => "methods",
        }
    }
}

/// The whole state file: one bundle per branch.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StateDoc {
    pub root: String,
    #[serde(default)]
    pub version: u32,
    /// One entry per git branch. An empty `name` means "not a git repo".
    #[serde(default)]
    pub bundles: Vec<BranchBundle>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BranchBundle {
    pub name: String,
    /// Unix seconds.
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub depth: u32,
    /// One entry per bundled unit. Written as `[[bundles.entries]]`; version 2
    /// files used `[[bundles.files]]` and are still read (see [`StoredBundle`]).
    #[serde(default, rename = "entries")]
    pub files: Vec<StateFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFile {
    pub path: String,
    /// Granularity this entry is bundled at.
    pub scope: Selection,
    /// Top-level declaration, for `symbol` and `methods` entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Kept methods of `symbol`, for `methods` entries. Sorted, non-empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<String>,
    #[serde(default)]
    pub depth: u32,
    /// `seed`, `import`, `static-import`, `wildcard-import`, `same-package`,
    /// `usage`, `call`, `package`, `config`, `external`.
    #[serde(default)]
    pub reason: String,
}

impl StateFile {
    /// `path`, `path::Symbol` or `path::Symbol [m1, m2]`.
    pub fn label(&self) -> String {
        match self.scope {
            Selection::File => self.path.clone(),
            Selection::Symbol => match &self.symbol {
                Some(symbol) => format!("{}::{symbol}", self.path),
                None => self.path.clone(),
            },
            Selection::Methods => match &self.symbol {
                Some(symbol) => format!("{}::{symbol} [{}]", self.path, self.methods.join(", ")),
                None => self.path.clone(),
            },
        }
    }
}

impl StateDoc {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_string_lossy().to_string(),
            version: VERSION,
            bundles: Vec::new(),
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
}

/// On-disk layout as read back: any version. Migrated into [`StateDoc`] by
/// [`StoredDoc::into_state`], so the rest of the program only ever sees v3.
#[derive(Debug, Deserialize)]
struct StoredDoc {
    #[serde(default)]
    root: String,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    bundles: Vec<StoredBundle>,

    // Version 1: one bundle at the top level.
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    created: Option<u64>,
    #[serde(default)]
    depth: Option<u32>,
    #[serde(default)]
    files: Option<Vec<StoredFile>>,
}

#[derive(Debug, Deserialize)]
struct StoredBundle {
    #[serde(default)]
    name: String,
    #[serde(default)]
    created: u64,
    #[serde(default)]
    depth: u32,
    #[serde(default, alias = "entries")]
    files: Vec<StoredFile>,
}

#[derive(Debug, Deserialize)]
struct StoredFile {
    path: String,
    /// Version 3 only; absent in v2, where the scope is inferred.
    #[serde(default)]
    scope: Option<Selection>,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    methods: Vec<String>,
    /// Version 2's single kept method.
    #[serde(default)]
    member: Option<String>,
    #[serde(default)]
    depth: u32,
    #[serde(default)]
    reason: String,
}

impl StoredDoc {
    fn into_state(self) -> StateDoc {
        let StoredDoc {
            root,
            version,
            mut bundles,
            branch,
            created,
            depth,
            files,
        } = self;

        // v1 (and files written before `version` existed) kept a single bundle
        // at the top level.
        if let Some(files) = files {
            if version < 2 && (!files.is_empty() || branch.is_some()) {
                bundles.push(StoredBundle {
                    name: branch.unwrap_or_default(),
                    created: created.unwrap_or(0),
                    depth: depth.unwrap_or(0),
                    files,
                });
            }
        }

        StateDoc {
            root,
            version: VERSION,
            bundles: bundles
                .into_iter()
                .map(|bundle| BranchBundle {
                    name: bundle.name,
                    created: bundle.created,
                    depth: bundle.depth,
                    files: normalize_entries(bundle.files),
                })
                .collect(),
        }
    }
}

/// Turns any version's entries into v3 entries: infer the scope where it was
/// implicit, and group v2's one-entry-per-member into one `methods` entry per
/// class.
fn normalize_entries(entries: Vec<StoredFile>) -> Vec<StateFile> {
    let mut out: Vec<StateFile> = Vec::new();
    let mut method_slots: HashMap<(String, String), usize> = HashMap::new();

    for entry in entries {
        let scope = match (entry.scope, &entry.symbol, &entry.member) {
            (Some(scope), _, _) => scope,
            (None, Some(_), Some(_)) => Selection::Methods,
            (None, Some(_), None) => Selection::Symbol,
            (None, None, _) => Selection::File,
        };
        let mut methods: Vec<String> = match (scope, &entry.member) {
            (Selection::Methods, Some(member)) => vec![member.clone()],
            (Selection::Methods, None) => entry.methods,
            _ => Vec::new(),
        };

        // An entry without a declaration name can only be a whole file.
        let Some(symbol) = entry.symbol else {
            out.push(StateFile {
                path: entry.path,
                scope: Selection::File,
                symbol: None,
                methods: Vec::new(),
                depth: entry.depth,
                reason: entry.reason,
            });
            continue;
        };

        if scope != Selection::Methods || methods.is_empty() {
            out.push(StateFile {
                path: entry.path,
                scope: Selection::Symbol,
                symbol: Some(symbol),
                methods: Vec::new(),
                depth: entry.depth,
                reason: entry.reason,
            });
            continue;
        }

        methods.sort();
        methods.dedup();
        let key = (entry.path.clone(), symbol.clone());
        match method_slots.get(&key).copied() {
            Some(position) => {
                let existing = &mut out[position];
                for method in methods {
                    if !existing.methods.contains(&method) {
                        existing.methods.push(method);
                    }
                }
                existing.methods.sort();
                // `seed` wins; otherwise the minimum-depth reason.
                let takes = if entry.reason == "seed" {
                    existing.reason != "seed" || entry.depth < existing.depth
                } else {
                    entry.reason != "seed" && entry.depth < existing.depth
                };
                if takes {
                    existing.reason = entry.reason;
                }
                existing.depth = existing.depth.min(entry.depth);
            }
            None => {
                method_slots.insert(key, out.len());
                out.push(StateFile {
                    path: entry.path,
                    scope: Selection::Methods,
                    symbol: Some(symbol),
                    methods,
                    depth: entry.depth,
                    reason: entry.reason,
                });
            }
        }
    }

    out
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

/// Directory of per-session state files. `state::save` puts a `*` `.gitignore`
/// in `.filer/`, so sessions are repo-local but never committed.
pub fn sessions_dir(root: &Path) -> PathBuf {
    root.join(".filer").join("sessions")
}

/// `.filer/sessions/<id>.toml`.
pub fn session_path(root: &Path, id: &str) -> PathBuf {
    sessions_dir(root).join(format!("{id}.toml"))
}

/// A session id becomes a file name, so only a safe alphabet is accepted: no
/// separators, no `.`/`..`, nothing that leaves the sessions directory.
pub fn validate_session(id: &str) -> Result<(), FilerError> {
    let safe = !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        });
    if safe {
        Ok(())
    } else {
        Err(FilerError::internal(format!("invalid session id {id:?}"))
            .with_hint("session ids may contain letters, digits, '.', '-' and '_'"))
    }
}

/// Saved session files as `(id, path)`, sorted by id.
pub fn session_files(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(sessions_dir(root)) else {
        return Vec::new();
    };
    let mut sessions: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .filter_map(|path| {
            let id = path.file_stem()?.to_str()?.to_string();
            Some((id, path))
        })
        .collect();
    sessions.sort();
    sessions
}

/// Read the state file. A missing file is `None`; a corrupt or unreadable one
/// warns and is also `None`, so the caller rebuilds the bundle instead of
/// aborting.
pub fn load(path: &Path) -> Option<StateDoc> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!(
                "filer: ignoring unreadable state {}: {error}",
                path.display()
            );
            return None;
        }
    };
    match parse(&text) {
        Ok(doc) => Some(doc),
        Err(error) => {
            eprintln!("filer: ignoring corrupt state {}: {error}", path.display());
            None
        }
    }
}

fn parse(text: &str) -> Result<StateDoc, toml::de::Error> {
    toml::from_str::<StoredDoc>(text).map(StoredDoc::into_state)
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
    // Temp file in the same directory, fsync, rename: a crash or a concurrent
    // writer can only leave the old file or the new one, never a torn TOML.
    atomic::write(path, to_toml(doc).as_bytes())
}

pub fn clear(path: &Path) -> std::io::Result<bool> {
    atomic::remove(path)
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

    #[test]
    fn corrupt_state_is_ignored_not_fatal() {
        let dir = std::env::temp_dir().join(format!("filer-state-corrupt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bundle.toml");

        fs::write(&path, "version = [3\n").unwrap();
        assert!(load(&path).is_none(), "torn TOML must not be fatal");
        assert!(load(&dir.join("missing.toml")).is_none());

        fs::write(&path, to_toml(&StateDoc::new(Path::new("/repo")))).unwrap();
        assert!(load(&path).is_some(), "a rebuilt state must load again");

        let _ = fs::remove_dir_all(&dir);
    }

    fn file(path: &str, depth: u32, reason: &str) -> StateFile {
        StateFile {
            path: path.to_string(),
            scope: Selection::File,
            symbol: None,
            methods: Vec::new(),
            depth,
            reason: reason.to_string(),
        }
    }

    fn method(path: &str, symbol: &str, methods: &[&str], depth: u32, reason: &str) -> StateFile {
        StateFile {
            path: path.to_string(),
            scope: Selection::Methods,
            symbol: Some(symbol.to_string()),
            methods: methods.iter().map(|method| method.to_string()).collect(),
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

        let parsed = parse(&to_toml(&doc)).expect("valid toml");
        assert_eq!(parsed.version, VERSION);
        assert_eq!(parsed.bundles.len(), 2);
        assert_eq!(parsed.bundle("main").unwrap().files[0].path, "A.java");
        assert_eq!(parsed.bundle("ticket-42").unwrap().files[0].path, "B.java");
        assert!(parsed.bundle("other").is_none());
    }

    #[test]
    fn entries_carry_their_scope() {
        let mut doc = StateDoc::new(Path::new("/repo"));
        doc.upsert(BranchBundle {
            name: "main".to_string(),
            created: 1,
            depth: 2,
            files: vec![
                file("build.gradle", 0, "seed"),
                StateFile {
                    path: "Main.scala".to_string(),
                    scope: Selection::Symbol,
                    symbol: Some("Main".to_string()),
                    methods: Vec::new(),
                    depth: 1,
                    reason: "import".to_string(),
                },
                method("Service.java", "Service", &["used", "internal"], 0, "seed"),
            ],
        });

        let text = to_toml(&doc);
        assert!(text.contains("scope = \"file\""), "{text}");
        assert!(text.contains("scope = \"symbol\""), "{text}");
        assert!(text.contains("scope = \"methods\""), "{text}");
        assert!(text.contains("methods = ["), "{text}");
        // v3 uses `entries`; the v2 `files` key must not come back.
        assert!(text.contains("[[bundles.entries]]"), "{text}");
        assert!(!text.contains("[[bundles.files]]"), "{text}");

        let parsed = parse(&text).expect("valid toml");
        let files = &parsed.bundle("main").unwrap().files;
        assert_eq!(files[0].scope, Selection::File);
        assert_eq!(files[1].scope, Selection::Symbol);
        assert_eq!(files[2].scope, Selection::Methods);
        assert_eq!(files[2].methods, ["internal", "used"]);
    }

    #[test]
    fn migrates_version_two_members_into_one_methods_entry() {
        let legacy = r#"
            root = "/repo"
            version = 2

            [[bundles]]
            name = "main"
            created = 11
            depth = 2

            [[bundles.files]]
            path = "Service.java"
            depth = 1
            reason = "same-package"

            [[bundles.files]]
            path = "Service.java"
            symbol = "Service"
            member = "used"
            depth = 0
            reason = "seed"

            [[bundles.files]]
            path = "Service.java"
            symbol = "Service"
            member = "inner"
            depth = 2
            reason = "call"

            [[bundles.files]]
            path = "Repo.java"
            symbol = "Repo"
            depth = 2
            reason = "call"
        "#;
        let doc = parse(legacy).expect("legacy toml");
        assert_eq!(doc.version, VERSION);
        let files = &doc.bundle("main").unwrap().files;
        assert_eq!(files.len(), 3, "{files:?}");
        assert_eq!(files[0].scope, Selection::File);
        assert_eq!(files[1].scope, Selection::Methods);
        assert_eq!(files[1].methods, ["inner", "used"]);
        // seed wins over the deeper `call`.
        assert_eq!(files[1].reason, "seed");
        assert_eq!(files[1].depth, 0);
        assert_eq!(files[2].scope, Selection::Symbol);
        assert_eq!(files[2].symbol.as_deref(), Some("Repo"));
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
            symbol = "A"
            depth = 0
            reason = "seed"
        "#;
        let doc = parse(legacy).expect("legacy toml");
        assert_eq!(doc.version, VERSION);
        assert_eq!(doc.bundles.len(), 1);
        assert_eq!(doc.bundles[0].name, "main");
        assert_eq!(doc.bundles[0].created, 11);
        assert_eq!(doc.bundles[0].files[0].path, "A.java");
        assert_eq!(doc.bundles[0].files[0].scope, Selection::Symbol);
        // Re-serializing must not write the legacy fields again.
        let text = to_toml(&doc);
        assert!(!text.contains("branch = "), "legacy field leaked: {text}");
        assert!(!text.contains("[[files]]"), "legacy field leaked: {text}");
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
