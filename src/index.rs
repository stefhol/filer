//! Repository walking, filename/package/type index and its on-disk cache.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use walkdir::{DirEntry, WalkDir};

use crate::java::{Import, JavaParser};

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".gradle",
    ".idea",
    ".mvn",
    ".filer",
    ".venv",
    "target",
    "build",
    "out",
    "bin",
    "dist",
    "node_modules",
];

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Path relative to the project root, always with `/` separators.
    pub rel: String,
    pub abs: PathBuf,
    pub package: Option<String>,
    /// Top-level type declarations (classes, interfaces, enums, records).
    pub types: Vec<String>,
    pub imports: Vec<Import>,
}

impl FileEntry {
    /// `com.example.app.Service`, or just the type name for the default package.
    pub fn primary_fqn(&self) -> String {
        match (self.package.as_deref(), self.types.first()) {
            (Some(package), Some(ty)) => format!("{package}.{ty}"),
            (None, Some(ty)) => ty.clone(),
            _ => String::new(),
        }
    }

    pub fn fqns(&self) -> impl Iterator<Item = String> + '_ {
        self.types.iter().map(|ty| match self.package.as_deref() {
            Some(package) => format!("{package}.{ty}"),
            None => ty.clone(),
        })
    }

    /// Name used as a single-line label in pickers.
    pub fn label(&self) -> String {
        let fqn = self.primary_fqn();
        if fqn.is_empty() {
            self.rel.clone()
        } else {
            fqn
        }
    }
}

#[derive(Debug, Default)]
pub struct Index {
    pub root: PathBuf,
    pub files: Vec<FileEntry>,
    pub by_fqn: HashMap<String, usize>,
    pub by_package: HashMap<String, Vec<usize>>,
    pub by_rel: HashMap<String, usize>,
    /// Number of files re-parsed on this run (for `filer index` stats).
    pub parsed_count: usize,
    pub reused_count: usize,
}

impl Index {
    pub fn build(root: &Path) -> Index {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let paths = collect_java_files(&root);

        let mut cache = Cache::load(&root);
        let stale: HashMap<String, CacheEntry> = cache
            .entries
            .drain(..)
            .map(|entry| (entry.rel.clone(), entry))
            .collect();

        let parsed = parse_all(&root, &paths, &stale);
        let mut files: Vec<FileEntry> = parsed.iter().map(|(_, entry)| entry.clone()).collect();
        files.sort_by(|a, b| a.rel.cmp(&b.rel));

        let mut index = Index {
            root: root.clone(),
            files,
            ..Default::default()
        };
        index.parsed_count = parsed.iter().filter(|(from_cache, _)| !from_cache).count();
        index.reused_count = parsed.len() - index.parsed_count;

        for (position, file) in index.files.iter().enumerate() {
            index.by_rel.insert(file.rel.clone(), position);
            if let Some(package) = &file.package {
                index
                    .by_package
                    .entry(package.clone())
                    .or_default()
                    .push(position);
            }
            for fqn in file.fqns() {
                index.by_fqn.entry(fqn).or_insert(position);
            }
        }

        Cache::store(&root, &index);
        index
    }
}

fn collect_java_files(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_skipped(entry))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "java"))
        .collect()
}

fn is_skipped(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    if entry.file_type().is_dir() {
        let name = entry.file_name().to_string_lossy();
        return SKIP_DIRS.contains(&name.as_ref());
    }
    false
}

/// Parses every file, reusing cache entries whose mtime and size still match.
/// Returns `(from_cache, entry)` pairs, ordered like `paths`.
fn parse_all(
    root: &Path,
    paths: &[PathBuf],
    stale: &HashMap<String, CacheEntry>,
) -> Vec<(bool, FileEntry)> {
    if paths.is_empty() {
        return Vec::new();
    }

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16)
        .min(paths.len());
    let chunk = paths.len().div_ceil(threads);

    let mut collected: Vec<(usize, bool, FileEntry)> = Vec::with_capacity(paths.len());
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (chunk_index, slice) in paths.chunks(chunk).enumerate() {
            let base = chunk_index * chunk;
            handles.push(scope.spawn(move || {
                let mut parser = JavaParser::new();
                let mut local = Vec::with_capacity(slice.len());
                for (offset, path) in slice.iter().enumerate() {
                    if let Some(entry) = load_one(root, path, stale, &mut parser) {
                        local.push((base + offset, entry.0, entry.1));
                    }
                }
                local
            }));
        }
        for handle in handles {
            if let Ok(local) = handle.join() {
                collected.extend(local);
            }
        }
    });

    collected.sort_by_key(|(position, _, _)| *position);
    collected
        .into_iter()
        .map(|(_, from_cache, entry)| (from_cache, entry))
        .collect()
}

fn load_one(
    root: &Path,
    path: &Path,
    stale: &HashMap<String, CacheEntry>,
    parser: &mut JavaParser,
) -> Option<(bool, FileEntry)> {
    let rel = relative(root, path);
    let metadata = fs::metadata(path).ok()?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let size = metadata.len();

    if let Some(cached) = stale.get(&rel) {
        if cached.mtime == mtime && cached.size == size {
            return Some((
                true,
                FileEntry {
                    rel,
                    abs: path.to_path_buf(),
                    package: cached.package.clone(),
                    types: cached.types.clone(),
                    imports: cached.imports.clone(),
                },
            ));
        }
    }

    let bytes = fs::read(path).ok()?;
    let source = String::from_utf8_lossy(&bytes);
    let parsed = parser.parse(&source)?;
    Some((
        false,
        FileEntry {
            rel,
            abs: path.to_path_buf(),
            package: parsed.package,
            types: parsed.types,
            imports: parsed.imports,
        },
    ))
}

pub fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    version: u32,
    root: String,
    entries: Vec<CacheEntry>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            root: String::new(),
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    rel: String,
    mtime: u64,
    size: u64,
    package: Option<String>,
    types: Vec<String>,
    imports: Vec<Import>,
}

impl Cache {
    fn load(root: &Path) -> Cache {
        let Some(path) = cache_path(root) else {
            return Cache::default();
        };
        let Ok(bytes) = fs::read(&path) else {
            return Cache::default();
        };
        match serde_json::from_slice::<Cache>(&bytes) {
            Ok(cache) if cache.version == CACHE_VERSION && cache.root == root.to_string_lossy() => {
                cache
            }
            _ => Cache::default(),
        }
    }

    fn store(root: &Path, index: &Index) {
        let Some(path) = cache_path(root) else {
            return;
        };
        let entries = index
            .files
            .iter()
            .filter_map(|file| {
                let metadata = fs::metadata(&file.abs).ok()?;
                let mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos() as u64)
                    .unwrap_or(0);
                Some(CacheEntry {
                    rel: file.rel.clone(),
                    mtime,
                    size: metadata.len(),
                    package: file.package.clone(),
                    types: file.types.clone(),
                    imports: file.imports.clone(),
                })
            })
            .collect();
        let cache = Cache {
            version: CACHE_VERSION,
            root: root.to_string_lossy().to_string(),
            entries,
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec(&cache) {
            let _ = fs::write(path, json);
        }
    }
}

fn cache_path(root: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    Some(
        base.join("filer")
            .join(format!("{:016x}.json", hasher.finish())),
    )
}
