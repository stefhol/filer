//! Repository walking, filename/package/type index and its on-disk cache.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ignore::{DirEntry, WalkBuilder};
use serde::{Deserialize, Serialize};

use crate::atomic;
use crate::filter::Filter;
use crate::lang::{end_line_of, Import, Language, Member, SourceParser, Symbol};

/// Build output and tool caches, skipped even with `--no-ignore`: they are never
/// source, and walking them is what makes a first index slow.
pub const SKIP_DIRS: &[&str] = &[
    ".git",
    ".gradle",
    ".idea",
    ".mvn",
    ".filer",
    ".venv",
    ".next",
    ".nuxt",
    ".output",
    ".turbo",
    ".svelte-kit",
    "coverage",
    "target",
    "build",
    "out",
    "bin",
    "dist",
    "node_modules",
];

const CACHE_VERSION: u32 = 16;

#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Path relative to the project root, always with `/` separators.
    pub rel: String,
    pub abs: PathBuf,
    pub package: Option<String>,
    /// Top-level type declarations (classes, interfaces, enums, records).
    pub types: Vec<String>,
    pub imports: Vec<Import>,
    /// Type names referenced in the file (import-free same-package references
    /// included), sorted and deduplicated.
    pub uses: Vec<String>,
    /// Top-level declarations with byte spans, in source order. Empty when the
    /// file is the natural bundling unit (or the language has no parser for
    /// symbols yet).
    pub symbols: Vec<Symbol>,
    /// End of the file header (license, package, imports): the first symbol's
    /// start. Prepended to each symbol excerpt so it stands alone.
    pub preamble_end: usize,
    /// File size in bytes, cached so `search` can budget without a read.
    pub size: u64,
    /// 1-based line of the last byte, cached so `search` can budget without a
    /// read.
    pub end_line: usize,
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

    /// Language of the file, derived from its extension.
    pub fn language(&self) -> Option<Language> {
        Language::from_path(Path::new(&self.rel))
    }

    /// Symbol with this name, if the file declares one.
    pub fn symbol(&self, name: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|symbol| symbol.name == name)
    }

    /// Symbol whose span contains `offset` (used by `--at-line`), preferring
    /// the most specific span when Rust `impl` blocks make symbols overlap.
    pub fn symbol_at(&self, offset: usize) -> Option<&Symbol> {
        self.symbols
            .iter()
            .filter(|symbol| symbol.contains(offset))
            .min_by_key(|symbol| symbol.end - symbol.start)
    }

    /// Member (method) whose span contains `offset`, with its enclosing symbol.
    pub fn member_at(&self, offset: usize) -> Option<(&Symbol, &Member)> {
        self.symbols.iter().find_map(|symbol| {
            symbol
                .members
                .iter()
                .find(|member| (member.start..member.end).contains(&offset))
                .map(|member| (symbol, member))
        })
    }
}

#[derive(Debug, Default)]
pub struct Index {
    pub root: PathBuf,
    pub files: Vec<FileEntry>,
    /// Fully qualified type name -> files declaring it (one name can exist in
    /// several languages, e.g. Java and Kotlin in the same package).
    pub by_fqn: HashMap<String, Vec<usize>>,
    /// Simple type name -> files declaring it.
    pub by_simple: HashMap<String, Vec<usize>>,
    /// Simple symbol name -> (file position, symbol index), for excerpt-level
    /// resolution. Symbols are only in this map for languages that populate
    /// them; whole-file bundles never look here.
    pub by_symbol: HashMap<String, Vec<(usize, usize)>>,
    pub by_package: HashMap<String, Vec<usize>>,
    pub by_rel: HashMap<String, usize>,
    /// Number of files re-parsed on this run (for `filer index` stats).
    pub parsed_count: usize,
    pub reused_count: usize,
    /// Source files (supported extension) that could not be read or parsed and
    /// therefore are not in the index. Reported as `coverage.files_skipped`.
    pub files_skipped: usize,
}

impl Index {
    /// The default view: `.gitignore` honored, no `--include`/`--exclude`.
    /// Only the in-crate tests build an unfiltered index without a [`Filter`].
    #[cfg(test)]
    pub fn build(root: &Path) -> Index {
        Index::build_with(root, &Filter::default())
    }

    /// The filtered view of the tree: ignore files plus the caller's globs
    /// decide which paths exist as far as every other command is concerned.
    pub fn build_with(root: &Path, filter: &Filter) -> Index {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let paths = collect_source_files(&root, filter);

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
        // Candidate source files that got no entry: unreadable or unparsable.
        index.files_skipped = paths.len() - parsed.len();

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
                let candidates = index.by_fqn.entry(fqn).or_default();
                if candidates.last() != Some(&position) {
                    candidates.push(position);
                }
            }
            for ty in &file.types {
                // A Scala file can declare a class and its companion object
                // (same name, same file); keep one entry per file.
                let candidates = index.by_simple.entry(ty.clone()).or_default();
                if candidates.last() != Some(&position) {
                    candidates.push(position);
                }
            }
            for (symbol_index, symbol) in file.symbols.iter().enumerate() {
                index
                    .by_symbol
                    .entry(symbol.name.clone())
                    .or_default()
                    .push((position, symbol_index));
            }
        }

        Cache::store(&root, &index, &stale);
        index
    }

    /// Reparses only the named paths and folds them into the cached index, so a
    /// burst of edits does not need a full tree walk per run. Nothing else is
    /// touched: entries for files that were not named keep their memo (or are
    /// dropped when the file is gone), and the returned count is the cache size
    /// after the write.
    ///
    /// `Ok` carries unreadable/outside-root/unparsable inputs as warnings; those
    /// are expected (an editor may save a file type filer does not parse).
    pub fn update_cache(root: &Path, inputs: &[String]) -> (UpdateStats, Vec<String>) {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut stale: HashMap<String, CacheEntry> = Cache::load(&root)
            .entries
            .into_iter()
            .map(|entry| (entry.rel.clone(), entry))
            .collect();
        let mut stats = UpdateStats::default();
        let mut warnings = Vec::new();
        let mut parser = SourceParser::new();
        let mut files: Vec<FileEntry> = Vec::new();

        for input in inputs {
            let path = Path::new(input);
            let abs = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            // `strip_prefix` keeps `..` segments, and a symlink can point outside
            // the root, so both are checked before anything is updated.
            let outside = abs.strip_prefix(&root).is_err()
                || relative(&root, &abs).split('/').any(|part| part == "..")
                || (abs.is_file()
                    && abs
                        .canonicalize()
                        .map(|path| !path.starts_with(&root))
                        .unwrap_or(true));
            if outside {
                warnings.push(format!("skipping {input}: not inside {}", root.display()));
                stats.skipped += 1;
                continue;
            }
            let rel = relative(&root, &abs);

            if !abs.is_file() {
                if stale.remove(&rel).is_some() {
                    stats.removed += 1;
                } else {
                    warnings.push(format!("skipping {input}: no such file"));
                    stats.skipped += 1;
                }
                continue;
            }
            if Language::from_path(&abs).is_none() {
                warnings.push(format!("skipping {input}: not a source file"));
                stats.skipped += 1;
                continue;
            }
            match load_one(&root, &abs, &stale, &mut parser) {
                Some((from_cache, entry)) => {
                    if from_cache {
                        stats.reused += 1;
                    } else {
                        stats.parsed += 1;
                    }
                    files.push(entry);
                }
                None => {
                    warnings.push(format!("skipping {input}: cannot parse"));
                    stats.skipped += 1;
                }
            }
        }

        let index = Index {
            root: root.clone(),
            files,
            ..Default::default()
        };
        stats.entries = Cache::store(&root, &index, &stale);
        (stats, warnings)
    }
}

/// What `index --update` did, one counter per named path.
#[derive(Debug, Default)]
pub struct UpdateStats {
    /// Named paths that were parsed (new or changed).
    pub parsed: usize,
    /// Named paths whose cache memo was still valid.
    pub reused: usize,
    /// Named paths that no longer exist and left the cache.
    pub removed: usize,
    /// Named paths that were not updated (outside root, not source, unreadable).
    pub skipped: usize,
    /// Cache entries after the write.
    pub entries: usize,
}

fn collect_source_files(root: &Path, filter: &Filter) -> Vec<PathBuf> {
    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(false)
        // Dot directories still hold sources in some projects; only ignore files
        // and `SKIP_DIRS` remove paths.
        .hidden(false)
        // Ignore files belong to the project root, not to whatever directory the
        // user happens to call filer from.
        .parents(false)
        // `.gitignore` is honored in a plain directory too, not only in a repo.
        .require_git(false)
        .ignore(filter.ignore_files())
        .git_ignore(filter.ignore_files())
        .git_global(filter.ignore_files())
        .git_exclude(filter.ignore_files());
    builder.filter_entry(|entry| !is_skipped(entry));

    builder
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.into_path())
        .filter(|path| Language::from_path(path).is_some())
        .filter(|path| filter.allows(&relative(root, path)))
        .collect()
}

fn is_skipped(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    if entry.file_type().is_some_and(|kind| kind.is_dir()) {
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
                let mut parser = SourceParser::new();
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
    parser: &mut SourceParser,
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
                    uses: cached.uses.clone(),
                    symbols: cached.symbols.clone(),
                    preamble_end: cached.preamble_end,
                    size: cached.size,
                    end_line: cached.end_line,
                },
            ));
        }
    }

    let language = Language::from_path(path)?;
    let bytes = fs::read(path).ok()?;
    let source = String::from_utf8_lossy(&bytes);
    let parsed = parser.parse(language, &source)?;
    Some((
        false,
        FileEntry {
            rel,
            abs: path.to_path_buf(),
            package: parsed.package,
            types: parsed.types,
            imports: parsed.imports,
            uses: parsed.uses,
            symbols: parsed.symbols,
            preamble_end: parsed.preamble_end,
            size,
            end_line: end_line_of(&source, 0, source.len()),
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
    #[serde(default)]
    uses: Vec<String>,
    #[serde(default)]
    symbols: Vec<Symbol>,
    #[serde(default)]
    preamble_end: usize,
    #[serde(default)]
    end_line: usize,
}

impl Cache {
    fn load(root: &Path) -> Cache {
        let Some(path) = cache_path(root) else {
            return Cache::default();
        };
        Cache::load_path(&path, root)
    }

    /// A torn or truncated cache rebuilds silently into a fresh parse; only a
    /// cache that is *present but unreadable* is worth a warning.
    fn load_path(path: &Path, root: &Path) -> Cache {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Cache::default(),
            Err(error) => {
                eprintln!(
                    "filer: ignoring unreadable index cache {}: {error}",
                    path.display()
                );
                return Cache::default();
            }
        };
        match serde_json::from_slice::<Cache>(&bytes) {
            Ok(cache) if cache.version == CACHE_VERSION && cache.root == root.to_string_lossy() => {
                cache
            }
            // A new CACHE_VERSION after an upgrade is normal; rebuild quietly.
            Ok(_) => Cache::default(),
            Err(error) => {
                eprintln!(
                    "filer: ignoring corrupt index cache {}: {error}",
                    path.display()
                );
                Cache::default()
            }
        }
    }

    /// Writes the parsed entries, then carries over the previous run's entries
    /// for files this run did not walk. A path hidden by a `.gitignore` edit or by
    /// `--include`/`--exclude` keeps its parse memo (it is only ever used when the
    /// path is walked again, and mtime+size still decide whether it is valid), so
    /// un-hiding it costs no parse. A file that no longer exists is dropped.
    fn store(root: &Path, index: &Index, stale: &HashMap<String, CacheEntry>) -> usize {
        let Some(path) = cache_path(root) else {
            return 0;
        };
        let mut entries: Vec<CacheEntry> = index
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
                    uses: file.uses.clone(),
                    symbols: file.symbols.clone(),
                    preamble_end: file.preamble_end,
                    end_line: file.end_line,
                })
            })
            .collect();

        let current: std::collections::HashSet<&str> =
            index.files.iter().map(|file| file.rel.as_str()).collect();
        for (rel, entry) in stale {
            if !current.contains(rel.as_str()) && fs::metadata(root.join(rel)).is_ok() {
                entries.push(entry.clone());
            }
        }

        let cache = Cache {
            version: CACHE_VERSION,
            root: root.to_string_lossy().to_string(),
            entries,
        };
        let count = cache.entries.len();
        if let Ok(json) = serde_json::to_vec(&cache) {
            let _ = atomic::write(&path, &json);
        }
        count
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::{closure_detailed, Reason, ResolveOptions};

    #[test]
    fn corrupt_cache_is_rebuilt_not_fatal() {
        let dir = std::env::temp_dir().join(format!("filer-cache-corrupt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.json");

        fs::write(&path, "{\"version\":15,\"root\":\"/repo\",\"entries\":[").unwrap();
        let cache = Cache::load_path(&path, Path::new("/repo"));
        assert!(cache.entries.is_empty());

        fs::write(&path, "{\"version\":0,\"root\":\"/repo\",\"entries\":[]}").unwrap();
        let cache = Cache::load_path(&path, Path::new("/repo"));
        assert!(cache.version == CACHE_VERSION);
        assert!(cache.entries.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn indexes_every_supported_extension() {
        let root = std::env::temp_dir().join(format!("filer-langs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let files = [
            ("A.java", "package p; class A {}"),
            ("B.scala", "package p\nclass B"),
            ("C.kt", "package p\nclass C"),
            ("D.groovy", "package p\nclass D {}"),
            ("E.cs", "namespace p; class E {}"),
            ("F.ts", "export class F {}"),
            ("G.jsx", "export class G {}"),
            (
                "H.vue",
                "<script setup lang=\"ts\">import X from \"./x\"</script>",
            ),
            ("I.fe", "function i() { return 1; }"),
            ("J.rs", "pub struct J;"),
        ];
        for (name, source) in files {
            fs::write(root.join(name), source).unwrap();
        }

        let index = Index::build(&root);
        assert_eq!(index.files.len(), files.len());
        assert_eq!(
            index
                .files
                .iter()
                .filter_map(FileEntry::language)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            Language::ALL.len()
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// The cache stores parsed symbols verbatim, so the new call-site and span
    /// lines have to survive a save/load round trip.
    #[test]
    fn cache_round_trip_keeps_call_lines_and_end_lines() {
        let root = std::env::temp_dir().join(format!("filer-cache-lines-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/app")).unwrap();
        fs::write(
            root.join("src/app/Service.java"),
            "package app;\n\nclass Service {\n    int size() {\n        return helper();\n    }\n\n    int helper() {\n        return 1;\n    }\n}\n",
        )
        .unwrap();

        let parsed = Index::build(&root);
        let cached = Index::build(&root);
        assert_eq!(cached.reused_count, 1, "second build reads the cache");

        for index in [&parsed, &cached] {
            let service = index
                .files
                .iter()
                .find(|file| file.types == ["Service"])
                .unwrap();
            let symbol = &service.symbols[0];
            assert_eq!((symbol.line, symbol.end_line), (3, 11));
            let size = &symbol.members[0];
            assert_eq!((size.line, size.end_line), (4, 6));
            assert_eq!(size.calls[0].name, "helper");
            assert_eq!(size.calls[0].line, 5);
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scala_companion_types_are_indexed_once_per_file() {
        let root = std::env::temp_dir().join(format!("filer-index-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let app = root.join("src/main/scala/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("DemoDocument.scala"),
            "package app\ncase class DemoDocument()\nobject DemoDocument { def create() = new DemoDocument }\n",
        )
        .unwrap();
        fs::write(
            app.join("Service.scala"),
            "package app\nclass Service { val doc = DemoDocument.create() }\n",
        )
        .unwrap();

        let index = Index::build(&root);
        assert_eq!(index.by_simple.get("DemoDocument").map(Vec::len), Some(1));

        let service = index.by_rel["src/main/scala/app/Service.scala"];
        let order = closure_detailed(
            &index,
            &[service],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: false,
            },
        );
        assert!(
            order.iter().any(|&(position, _, reason)| {
                index.files[position].rel.ends_with("DemoDocument.scala")
                    && reason == Reason::SamePackage
            }),
            "companion object use should pull its file in: {order:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
