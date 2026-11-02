//! Import resolution and transitive closure.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::index::{FileEntry, Index};
use crate::lang::{Family, Granularity, Import, ImportKind, Member, MemberKind, Receiver, Symbol};

/// Extensions tried when resolving a TypeScript/JavaScript module specifier,
/// in preference order (TS sources win over compiled JS next to them).
const MODULE_EXTENSIONS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "vue"];

/// Why a file ended up in a bundle. Serialized as a kebab-case string in the
/// state file, e.g. `reason = "same-package"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// Picked explicitly.
    Seed,
    Import,
    StaticImport,
    WildcardImport,
    /// A relative TypeScript/JavaScript module specifier.
    ModuleImport,
    /// Referenced by name without an import, in the same package.
    SamePackage,
    /// Referenced by name and unique in the repo.
    Usage,
    /// A method call resolved to a member of another class.
    Call,
    /// Pulled in by `--with-same-package`.
    Package,
    /// Build file (pom.xml, build.gradle, build.sbt, ...).
    Config,
    /// Handed in but not part of the index.
    External,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Seed => "seed",
            Reason::Import => "import",
            Reason::StaticImport => "static-import",
            Reason::WildcardImport => "wildcard-import",
            Reason::ModuleImport => "module-import",
            Reason::SamePackage => "same-package",
            Reason::Usage => "usage",
            Reason::Call => "call",
            Reason::Package => "package",
            Reason::Config => "config",
            Reason::External => "external",
        }
    }

    pub fn from_str(text: &str) -> Reason {
        match text {
            "seed" => Reason::Seed,
            "import" => Reason::Import,
            "static-import" => Reason::StaticImport,
            "wildcard-import" => Reason::WildcardImport,
            "module-import" => Reason::ModuleImport,
            "same-package" => Reason::SamePackage,
            "call" => Reason::Call,
            "package" => Reason::Package,
            "config" => Reason::Config,
            "external" => Reason::External,
            _ => Reason::Usage,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResolveOptions {
    /// How many import hops away from the seeds to include. 0 = seeds only.
    pub depth: u32,
    /// Also pull in every file sharing a package with an included file.
    pub same_package: bool,
    /// Use the call graph for whole-file and class seeds of member-capable
    /// languages (Java), instead of the import/type closure. `filer bundle`
    /// sets this; `filer deps` keeps the file-level view.
    pub minimal: bool,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            depth: 2,
            same_package: false,
            minimal: false,
        }
    }
}

fn family_of(file: &FileEntry) -> Option<Family> {
    file.language().map(|language| language.family())
}

/// Whether `position` may resolve against a file of `family`. Files whose
/// extension is unknown are compatible with everything.
fn compatible(index: &Index, position: usize, family: Option<Family>) -> bool {
    let Some(family) = family else {
        return true;
    };
    index
        .files
        .get(position)
        .and_then(FileEntry::language)
        .is_none_or(|language| language.family() == family)
}

fn pick_family(index: &Index, candidates: &[usize], family: Option<Family>) -> Option<usize> {
    candidates
        .iter()
        .copied()
        .find(|&position| compatible(index, position, family))
}

fn package_files(index: &Index, package: &str, family: Option<Family>) -> Vec<usize> {
    index
        .by_package
        .get(package)
        .map(|files| {
            files
                .iter()
                .copied()
                .filter(|&position| compatible(index, position, family))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolves one import to file indexes in the project.
///
/// Static member imports and nested types (`a.b.C.Inner`) are handled by
/// trimming trailing segments until a known type name is hit. Module imports
/// (`./x`, `../y.js`) go through [`resolve_module`].
pub fn resolve_import(index: &Index, file: &FileEntry, import: &Import) -> Vec<usize> {
    if import.kind == ImportKind::Module {
        return resolve_module(index, &file.rel, &import.path);
    }

    let family = family_of(file);

    if family == Some(Family::Rust) {
        return resolve_rust_import(index, file, import);
    }

    if import.is_wildcard() {
        // `a.b._` is usually a package, but Scala also lets you import the
        // members of an object (`a.b.C._`, `a.b.C.given`) and Java has static
        // wildcard imports. A path can be both a package and a type, so take
        // the union instead of guessing.
        let mut resolved = package_files(index, &import.path, family);
        if let Some(candidates) = index.by_fqn.get(&import.path) {
            if let Some(position) = pick_family(index, candidates, family) {
                if !resolved.contains(&position) {
                    resolved.push(position);
                }
            }
        }
        return resolved;
    }

    let segments: Vec<&str> = import.path.split('.').collect();
    if segments.len() < 2 {
        // A single-segment import can only be a type in the default package.
        return index
            .by_fqn
            .get(&import.path)
            .and_then(|candidates| pick_family(index, candidates, family))
            .map(|position| vec![position])
            .unwrap_or_default();
    }

    // `a.b.C.member` -> try `a.b.C.member`, `a.b.C`, `a.b`, ... stop at 2.
    for end in (2..=segments.len()).rev() {
        let candidate = segments[..end].join(".");
        if let Some(candidates) = index.by_fqn.get(&candidate) {
            if let Some(position) = pick_family(index, candidates, family) {
                return vec![position];
            }
        }
    }

    // Fall back to the conventional package-layout path.
    if let Some(name) = segments.last() {
        let package = segments[..segments.len() - 1].join(".");
        let files = package_files(index, &package, family);
        if !files.is_empty() {
            if let Some(position) = files
                .iter()
                .copied()
                .find(|&position| index.files[position].types.iter().any(|ty| ty == name))
            {
                return vec![position];
            }

            // Scala package objects and files with only top-level definitions
            // declare no indexed type, but they are where the imported member
            // lives (`import a.b.Version` with `package object b`).
            if files
                .iter()
                .any(|&position| index.files[position].types.is_empty())
            {
                return files
                    .into_iter()
                    .filter(|&position| index.files[position].types.is_empty())
                    .collect();
            }
        }
    }

    Vec::new()
}

/// Resolves a relative module specifier against the index, with the same
/// probing rules TypeScript/Node use: extensionless imports, directory index
/// files, and `.js` specifiers that point at a `.ts` source.
fn resolve_module(index: &Index, from_rel: &str, specifier: &str) -> Vec<usize> {
    if !(specifier.starts_with("./") || specifier.starts_with("../")) {
        return Vec::new();
    }
    let Some(base) = normalize_module(from_rel, specifier) else {
        return Vec::new();
    };
    if base.is_empty() {
        return Vec::new();
    }

    let mut candidates: Vec<String> = Vec::new();
    let extension = Path::new(&base)
        .extension()
        .and_then(|extension| extension.to_str());
    match extension {
        // `./x.js` should find `x.ts` next to it, then `x.js` itself.
        Some(extension @ ("js" | "jsx" | "mjs" | "cjs")) => {
            let stem = base.strip_suffix(&format!(".{extension}")).unwrap_or(&base);
            for candidate_extension in MODULE_EXTENSIONS {
                candidates.push(format!("{stem}.{candidate_extension}"));
            }
            candidates.push(base.clone());
        }
        // An explicit `.ts`/`.vue`/... specifier keeps its own file first.
        Some(_) => candidates.push(base.clone()),
        None => {
            for candidate_extension in MODULE_EXTENSIONS {
                candidates.push(format!("{base}.{candidate_extension}"));
            }
        }
    }
    let directory = base.trim_end_matches('/');
    for candidate_extension in MODULE_EXTENSIONS {
        candidates.push(format!("{directory}/index.{candidate_extension}"));
    }

    for candidate in candidates {
        if let Some(&position) = index.by_rel.get(&candidate) {
            return vec![position];
        }
    }
    Vec::new()
}

/// Joins a relative specifier onto the importing file's directory and collapses
/// `.`/`..`. Returns `None` when the path escapes the project root.
fn normalize_module(from_rel: &str, specifier: &str) -> Option<String> {
    let mut parts: Vec<&str> = from_rel.split('/').collect();
    parts.pop(); // drop the importing file name
    let parts: Vec<String> = parts.into_iter().map(str::to_string).collect();
    let mut parts = parts;
    for segment in specifier.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            segment => parts.push(segment.to_string()),
        }
    }
    Some(parts.join("/"))
}

/// Resolves a Rust `use` path or `mod` declaration against the filesystem
/// layout of a Cargo project. Imports are stored with `.` separators
/// (`crate.app.Service`), and the longest module prefix wins:
/// `crate.app.Service` -> `src/app.rs` (or `src/app/mod.rs`).
fn resolve_rust_import(index: &Index, file: &FileEntry, import: &Import) -> Vec<usize> {
    let direct = rust_direct_import(index, &file.rel, &import.path);
    if import.is_wildcard() || direct.is_empty() {
        return direct;
    }

    // `use crate::app::Service` where `app.rs` only re-exports `Service` from
    // another module: follow re-exports whose leaf matches, so the file that
    // actually declares the symbol is the import target.
    let Some(leaf) = import.path.rsplit('.').next() else {
        return direct;
    };
    let mut queue: VecDeque<usize> = direct.iter().copied().collect();
    let mut seen = HashSet::new();
    while let Some(position) = queue.pop_front() {
        if !seen.insert(position) {
            continue;
        }
        let Some(target) = index.files.get(position) else {
            continue;
        };
        if target.types.iter().any(|ty| ty == leaf)
            || target.symbols.iter().any(|symbol| symbol.name == leaf)
        {
            return vec![position];
        }
        for reexport in &target.imports {
            if reexport.is_wildcard() || reexport.kind == ImportKind::Module {
                continue;
            }
            if reexport.path.rsplit('.').next() != Some(leaf) {
                continue;
            }
            queue.extend(rust_direct_import(index, &target.rel, &reexport.path));
        }
    }
    direct
}

/// Resolves the module file named by a Rust path, without following
/// re-exports. The longest module prefix wins: `crate.app.Service` first tries
/// `src/app/Service.rs`, then `src/app.rs` / `src/app/mod.rs`.
fn rust_direct_import(index: &Index, rel: &str, path: &str) -> Vec<usize> {
    if path.is_empty() {
        return Vec::new();
    }
    let segments: Vec<String> = path.split('.').map(str::to_string).collect();
    let (root, current) = rust_module_roots(rel);
    let mut resolved = Vec::new();

    match segments.first().map(String::as_str) {
        Some("crate") => {
            if let Some(position) = rust_find_module(index, &root, &root, &segments[1..], true) {
                resolved.push(position);
            }
        }
        Some("self") => {
            if let Some(position) = rust_find_module(index, &current, &root, &segments[1..], true) {
                resolved.push(position);
            }
        }
        Some("super") => {
            let mut base = current.clone();
            let mut index_of = 0;
            while segments.get(index_of).map(String::as_str) == Some("super") {
                base = parent_module(&base);
                index_of += 1;
            }
            if let Some(position) =
                rust_find_module(index, &base, &root, &segments[index_of..], true)
            {
                resolved.push(position);
            }
        }
        // A 2018+ path that does not start with crate/self/super is usually an
        // external crate. Try it as a local module anyway, but never fall back
        // to the crate root: `use std::fmt::Debug` must stay unresolved.
        _ => {
            if let Some(position) = rust_find_module(index, &root, &root, &segments, false)
                .or_else(|| rust_find_module(index, &current, &root, &segments, false))
            {
                resolved.push(position);
            }
        }
    }

    // The module file is the import target; symbol-level resolution picks the
    // imported item out of it via the import's last path segment.
    resolved
}

/// Crate root directory and current module directory for a source file.
/// `src/app/service.rs` has root `src` and module directory `src/app/service`;
/// `src/app/mod.rs` has module directory `src/app`.
fn rust_module_roots(rel: &str) -> (String, String) {
    let mut parts: Vec<&str> = rel.split('/').collect();
    let filename = parts.last().copied().unwrap_or("");
    // `lib.rs`/`main.rs` are crate roots; a binary under `src/bin/<name>/`
    // is its own crate root, otherwise the nearest `src` directory is.
    let root = if filename == "lib.rs" || filename == "main.rs" {
        parts
            .get(..parts.len().saturating_sub(1))
            .map(|parts| parts.join("/"))
            .unwrap_or_default()
    } else if let Some(src) = parts.iter().rposition(|part| *part == "src") {
        if parts.get(src + 1) == Some(&"bin") {
            if parts.len() > src + 3 {
                parts[..src + 3].join("/")
            } else {
                parts[..=src + 1].join("/")
            }
        } else {
            parts[..=src].join("/")
        }
    } else {
        String::new()
    };
    let current = if filename == "mod.rs" {
        parts.pop();
        parts.join("/")
    } else if filename == "lib.rs" || filename == "main.rs" {
        root.clone()
    } else {
        // `src/app/service.rs` is module `crate::app::service`, so `mod x;`
        // and `self::x` look under `src/app/service/`.
        rel.strip_suffix(".rs").unwrap_or(rel).to_string()
    };
    (root, current)
}

fn parent_module(base: &str) -> String {
    base.rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or_default()
}

/// Longest-prefix module lookup. `allow_root` enables the degenerate case
/// (`use crate::Thing` where `Thing` lives in `lib.rs`/`main.rs`/`mod.rs`).
fn rust_find_module(
    index: &Index,
    base: &str,
    root: &str,
    segments: &[String],
    allow_root: bool,
) -> Option<usize> {
    for end in (0..=segments.len()).rev() {
        if end == 0 && !allow_root {
            continue;
        }
        let candidates = rust_module_candidates(base, root, &segments[..end]);
        for candidate in candidates {
            if let Some(&position) = index.by_rel.get(&candidate) {
                return Some(position);
            }
        }
    }
    None
}

fn rust_module_candidates(base: &str, root: &str, segments: &[String]) -> Vec<String> {
    let path = if segments.is_empty() {
        base.to_string()
    } else if base.is_empty() {
        segments.join("/")
    } else {
        format!("{base}/{}", segments.join("/"))
    };

    if segments.is_empty() {
        if path.is_empty() || path == root {
            let mut out = Vec::new();
            if path.is_empty() {
                out.push("lib.rs".to_string());
                out.push("main.rs".to_string());
                out.push("mod.rs".to_string());
            } else {
                out.push(format!("{path}/lib.rs"));
                out.push(format!("{path}/main.rs"));
                out.push(format!("{path}/mod.rs"));
            }
            out
        } else {
            vec![format!("{path}.rs"), format!("{path}/mod.rs")]
        }
    } else {
        vec![format!("{path}.rs"), format!("{path}/mod.rs")]
    }
}

/// Unique free function by name, for Rust calls that do not go through a
/// receiver (`helper()`, `Self::new()` after the type lookup failed, ...).
fn rust_function_units(
    index: &Index,
    name: &str,
    family: Option<Family>,
    prefer: Option<usize>,
) -> Vec<Unit> {
    let Some(candidates) = index.by_symbol.get(name) else {
        return Vec::new();
    };
    let mut matching: Vec<(usize, usize)> = candidates
        .iter()
        .copied()
        .filter(|(position, _symbol_index)| compatible(index, *position, family))
        .filter(|(position, symbol_index)| {
            index
                .files
                .get(*position)
                .and_then(|file| file.symbols.get(*symbol_index))
                .is_some_and(|symbol| symbol.kind == crate::lang::SymbolKind::Function)
        })
        .collect();
    if let Some(preferred) = prefer {
        let local: Vec<(usize, usize)> = matching
            .iter()
            .copied()
            .filter(|(position, _)| *position == preferred)
            .collect();
        if !local.is_empty() {
            matching = local;
        }
    }
    let mut files: Vec<usize> = matching.iter().map(|(position, _)| *position).collect();
    files.sort_unstable();
    files.dedup();
    if files.len() != 1 {
        return Vec::new();
    }
    matching
        .first()
        .and_then(|(position, symbol_index)| {
            index.files[*position]
                .symbols
                .get(*symbol_index)
                .map(|symbol| Unit::Symbol {
                    file: *position,
                    symbol: symbol.name.clone(),
                })
        })
        .into_iter()
        .collect()
}

/// Resolves type names *used* in the file. Java needs no import for types in
/// the same package, so a class that only references `DemoDocument.class`
/// still depends on `DemoDocument.java`.
///
/// Explicitly imported names are skipped (the import resolution handles those),
/// and ambiguous simple names are left out rather than guessed wrong. Names
/// only ever match files of the same language family.
pub fn resolve_uses(index: &Index, file: &FileEntry) -> Vec<(usize, Reason)> {
    let family = family_of(file);
    let imported: HashSet<&str> = file
        .imports
        .iter()
        .filter(|import| import.kind != ImportKind::Module)
        .filter(|import| !import.is_wildcard())
        .filter_map(|import| import.path.rsplit('.').next())
        .collect();

    let mut resolved = Vec::new();
    for name in &file.uses {
        if imported.contains(name.as_str()) {
            continue;
        }
        let Some(candidates) = index.by_simple.get(name) else {
            continue;
        };
        let candidates: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|&position| compatible(index, position, family))
            .collect();
        // FE has one global namespace, so "same package" would match every
        // `.fe` file in the project. Fall through to unique-name lookup.
        let same_package: Vec<usize> = if family == Some(Family::Fe) {
            Vec::new()
        } else {
            candidates
                .iter()
                .copied()
                .filter(|&position| index.files[position].package == file.package)
                .collect()
        };
        match same_package.len() {
            1 => resolved.push((same_package[0], Reason::SamePackage)),
            0 if candidates.len() == 1 => resolved.push((candidates[0], Reason::Usage)),
            _ => {}
        }
    }
    resolved
}

/// One bundling unit: a whole file, or one symbol (function/type/declaration)
/// excerpt of a file. Symbol units are what `path::Name` and the picker seed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Unit {
    File(usize),
    Symbol {
        file: usize,
        symbol: String,
    },
    /// A class shell plus one method kept on the call path.
    Member {
        file: usize,
        class: String,
        member: String,
    },
}

impl Unit {
    pub fn file(&self) -> usize {
        match self {
            Unit::File(file) | Unit::Symbol { file, .. } | Unit::Member { file, .. } => *file,
        }
    }

    /// The class or top-level symbol this unit belongs to.
    pub fn symbol(&self) -> Option<&str> {
        match self {
            Unit::File(_) => None,
            Unit::Symbol { symbol, .. } => Some(symbol),
            Unit::Member { class, .. } => Some(class),
        }
    }

    pub fn member(&self) -> Option<&str> {
        match self {
            Unit::Member { member, .. } => Some(member),
            _ => None,
        }
    }
}

/// One resolved dependency edge. `line` is the call site for `Reason::Call`;
/// for import/type edges it is the source member/symbol line (0 for a whole
/// file, which has no single line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub to: Unit,
    pub reason: Reason,
    pub line: u32,
}

/// Something filer declined to include. Collected during the graph build and
/// rendered by `bundle --json` / `deps --json`.
///
/// This is a closed enum: the JSON surface emits exactly these kinds, and each
/// one carries a fixed payload field beyond `kind` and `path`:
///
/// | kind | payload |
/// | --- | --- |
/// | `unresolved-import` | `import` |
/// | `unknown-receiver` | `line`, `call` |
/// | `unresolved-call` | `line`, `name` |
/// | `method-not-found` | `line`, `name` |
/// | `type-only-skipped` | `symbol` |
/// | `ambiguous` | `line`, `name` |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticKind {
    /// An import no indexed file provides (JDK, stdlib, npm, NuGet, ...).
    UnresolvedImport,
    /// A call through a receiver whose type is unknown (chained expressions).
    UnknownReceiver,
    /// A call whose name matched no method, even through supertypes.
    UnresolvedCall,
    /// The receiver class is known but declares no such method.
    MethodNotFound,
    /// A type in scope that a minimal (call-path) bundle deliberately skips.
    TypeOnlySkipped,
    /// The simple name matched more than one candidate, so it was not guessed.
    Ambiguous,
}

impl DiagnosticKind {
    /// The wire name, also the documented list for consumers.
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticKind::UnresolvedImport => "unresolved-import",
            DiagnosticKind::UnknownReceiver => "unknown-receiver",
            DiagnosticKind::UnresolvedCall => "unresolved-call",
            DiagnosticKind::MethodNotFound => "method-not-found",
            DiagnosticKind::TypeOnlySkipped => "type-only-skipped",
            DiagnosticKind::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub path: String,
    /// Call/declaration line, 0 when the diagnostic has no single line.
    pub line: u32,
    /// The payload name: import path, call name or symbol name.
    pub name: String,
}

impl Diagnostic {
    /// The versioned wire form: `kind`, `path` and the kind's payload field.
    pub fn to_json(&self) -> serde_json::Value {
        let kind = self.kind.as_str();
        let path = &self.path;
        match self.kind {
            DiagnosticKind::UnresolvedImport => serde_json::json!({
                "kind": kind,
                "path": path,
                "import": self.name,
            }),
            // The parsers record only the call name when the receiver is an
            // arbitrary expression (`a.b().c()`), not the expression's type.
            DiagnosticKind::UnknownReceiver => serde_json::json!({
                "kind": kind,
                "path": path,
                "line": self.line,
                "call": self.name,
            }),
            DiagnosticKind::TypeOnlySkipped => serde_json::json!({
                "kind": kind,
                "path": path,
                "symbol": self.name,
            }),
            DiagnosticKind::UnresolvedCall
            | DiagnosticKind::MethodNotFound
            | DiagnosticKind::Ambiguous => serde_json::json!({
                "kind": kind,
                "path": path,
                "line": self.line,
                "name": self.name,
            }),
        }
    }

    /// One human line for stderr, so text mode is as honest as JSON mode.
    pub fn to_line(&self) -> String {
        let location = if self.line == 0 {
            self.path.clone()
        } else {
            format!("{}:{}", self.path, self.line)
        };
        match self.kind {
            DiagnosticKind::UnresolvedImport => format!(
                "unresolved import {} ({}): probably JDK/third-party",
                self.name, location
            ),
            DiagnosticKind::UnknownReceiver => {
                format!("unknown receiver for {}() ({location})", self.name)
            }
            DiagnosticKind::UnresolvedCall => {
                format!("unresolved call {}() ({location})", self.name)
            }
            DiagnosticKind::MethodNotFound => {
                format!(
                    "method {}() not found on the receiver ({location})",
                    self.name
                )
            }
            DiagnosticKind::TypeOnlySkipped => format!(
                "type-only dependency {} skipped ({}): not on the call path",
                self.name, location
            ),
            DiagnosticKind::Ambiguous => format!(
                "ambiguous name {} ({location}): matched several files, not guessed",
                self.name
            ),
        }
    }
}

/// One incoming call edge found by [`callers`]. `line` is the call site in
/// `unit` that invokes the queried unit. `depth` is 1 for a direct caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub unit: Unit,
    pub line: u32,
    pub depth: u32,
}

/// One hop of a [`why`] chain. `line` is 0 for the seed, otherwise the call
/// site in the previous hop that pulled this unit in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhyHop {
    pub unit: Unit,
    pub line: u32,
    pub reason: Reason,
}

/// Whether a [`Reference`] is a name use or a call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Use,
    Call,
}

impl RefKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RefKind::Use => "use",
            RefKind::Call => "call",
        }
    }
}

/// One indexed site that mentions a name: the inverse of `Symbol.uses`,
/// `Symbol.calls` and `Member.calls`. `line` is the call line for calls and
/// the enclosing declaration line for uses (uses are stored per symbol, not
/// per occurrence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub file: usize,
    pub symbol: String,
    pub member: Option<String>,
    pub line: u32,
    pub kind: RefKind,
}

#[derive(Debug, Default)]
pub struct Graph {
    /// Outgoing edges for every expanded unit, in resolution order.
    pub out: HashMap<Unit, Vec<Edge>>,
    #[allow(dead_code)] // rendered by #10
    pub diagnostics: Vec<Diagnostic>,
    inc: Option<HashMap<Unit, Vec<Edge>>>,
}

impl Graph {
    /// Reverse edges are only needed for callers/`--direction in`/ranking, so
    /// they are built on first use; the default closure pays nothing for them.
    #[allow(dead_code)]
    pub fn reverse(&mut self) -> &HashMap<Unit, Vec<Edge>> {
        if self.inc.is_none() {
            let mut inc: HashMap<Unit, Vec<Edge>> = HashMap::new();
            for (from, edges) in &self.out {
                for edge in edges {
                    inc.entry(edge.to.clone()).or_default().push(Edge {
                        to: from.clone(),
                        reason: edge.reason,
                        line: edge.line,
                    });
                }
            }
            self.inc = Some(inc);
        }
        self.inc.as_ref().expect("inc was just built")
    }

    #[allow(dead_code)]
    pub fn callers(&mut self, unit: &Unit) -> &[Edge] {
        self.reverse().get(unit).map(Vec::as_slice).unwrap_or(&[])
    }

    /// BFS over the materialized `out` edges, one row per discovered unit.
    /// This is the traversal `closure_units` used to perform while resolving.
    fn walk(&self, seeds: &[Unit]) -> Vec<(Unit, u32, Reason)> {
        let mut depth_of: HashMap<Unit, u32> = HashMap::new();
        let mut order: Vec<(Unit, u32, Reason)> = Vec::new();
        let mut queue: VecDeque<Unit> = VecDeque::new();
        for seed in seeds {
            if depth_of.insert(seed.clone(), 0).is_none() {
                order.push((seed.clone(), 0, Reason::Seed));
                queue.push_back(seed.clone());
            }
        }
        while let Some(current) = queue.pop_front() {
            let depth = depth_of[&current];
            for edge in self.out.get(&current).into_iter().flatten() {
                if depth_of.contains_key(&edge.to) {
                    continue;
                }
                let next = depth + 1;
                depth_of.insert(edge.to.clone(), next);
                order.push((edge.to.clone(), next, edge.reason));
                queue.push_back(edge.to.clone());
            }
        }
        order
    }

    /// Closure as units, keeping excerpts of a file that is also bundled whole.
    /// `deps --unit` renders this so a method closure is inspectable.
    pub fn units(&self, seeds: &[Unit]) -> Vec<(Unit, u32, Reason)> {
        self.walk(seeds)
    }

    /// Closure as bundling units: a whole-file unit supersedes excerpts of the
    /// same file. This is what the bundle surface renders.
    pub fn closure(&self, seeds: &[Unit]) -> Vec<(Unit, u32, Reason)> {
        drop_covered_symbols(self.walk(seeds))
    }

    /// Imports of every file the closure touched that no indexed file provides.
    /// Collected after the walk so leaf seeds (depth 0) report them too.
    fn note_unresolved_imports(&mut self, index: &Index) {
        let mut positions: Vec<usize> = self
            .out
            .keys()
            .map(Unit::file)
            .chain(self.out.values().flatten().map(|edge| edge.to.file()))
            .collect();
        positions.sort_unstable();
        positions.dedup();
        for position in positions {
            let Some(file) = index.files.get(position) else {
                continue;
            };
            for import in &file.imports {
                if resolve_import(index, file, import).is_empty() {
                    self.diagnostics.push(Diagnostic {
                        kind: DiagnosticKind::UnresolvedImport,
                        path: file.rel.clone(),
                        line: 0,
                        name: import.path.clone(),
                    });
                }
            }
        }
    }
}

/// Every unit that can record an outgoing call: one member unit per method,
/// or one symbol unit for languages that keep calls on the symbol (Fe, Rust
/// free functions). Whole-file units are only used for file-granularity files.
fn call_units(index: &Index) -> Vec<Unit> {
    let mut units = Vec::new();
    for (position, file) in index.files.iter().enumerate() {
        let granularity = file
            .language()
            .map(|language| language.granularity())
            .unwrap_or(Granularity::File);
        match granularity {
            Granularity::File => units.push(Unit::File(position)),
            Granularity::Function => {
                for symbol in &file.symbols {
                    units.push(Unit::Symbol {
                        file: position,
                        symbol: symbol.name.clone(),
                    });
                }
            }
            Granularity::Member => {
                for symbol in &file.symbols {
                    let methods: Vec<&Member> = symbol
                        .members
                        .iter()
                        .filter(|member| member.kind == MemberKind::Method)
                        .collect();
                    if methods.is_empty() {
                        units.push(Unit::Symbol {
                            file: position,
                            symbol: symbol.name.clone(),
                        });
                    } else {
                        for member in methods {
                            units.push(Unit::Member {
                                file: position,
                                class: symbol.name.clone(),
                                member: member.name.clone(),
                            });
                        }
                    }
                }
            }
        }
    }
    units
}

fn unit_sort_key(index: &Index, unit: &Unit) -> (String, String, String) {
    (
        index
            .files
            .get(unit.file())
            .map(|file| file.rel.clone())
            .unwrap_or_default(),
        unit.symbol().unwrap_or("").to_string(),
        unit.member().unwrap_or("").to_string(),
    )
}

fn sort_edges(index: &Index, edges: &mut [&Edge]) {
    edges.sort_by(|left, right| {
        unit_sort_key(index, &left.to)
            .cmp(&unit_sort_key(index, &right.to))
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.reason.cmp(&right.reason))
    });
}

/// Direct and transitive callers of `target`, nearest first. The graph is
/// materialized one hop out of every call-recording unit in the index, then
/// traversed over its reverse (`inc`) edges. Only `Reason::Call` edges count:
/// an imported type is not a caller.
pub fn callers(index: &Index, target: &Unit, max_depth: u32) -> Vec<Caller> {
    if max_depth == 0 {
        return Vec::new();
    }
    let seeds = call_units(index);
    let mut graph = build_graph(
        index,
        &seeds,
        ResolveOptions {
            depth: 1,
            same_package: false,
            minimal: true,
        },
    );
    let inc = graph.reverse();

    let mut seen: HashMap<Unit, u32> = HashMap::new();
    seen.insert(target.clone(), 0);
    let mut queue: VecDeque<(Unit, u32)> = VecDeque::new();
    queue.push_back((target.clone(), 0));
    let mut found: Vec<Caller> = Vec::new();

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let Some(edges) = inc.get(&current) else {
            continue;
        };
        let mut edges: Vec<&Edge> = edges
            .iter()
            .filter(|edge| edge.reason == Reason::Call)
            .collect();
        sort_edges(index, &mut edges);
        for edge in edges {
            let next = depth + 1;
            if let Some(&known) = seen.get(&edge.to) {
                if known <= next {
                    continue;
                }
            }
            seen.insert(edge.to.clone(), next);
            found.push(Caller {
                unit: edge.to.clone(),
                line: edge.line,
                depth: next,
            });
            queue.push_back((edge.to.clone(), next));
        }
    }

    found.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| unit_sort_key(index, &left.unit).cmp(&unit_sort_key(index, &right.unit)))
            .then_with(|| left.line.cmp(&right.line))
    });
    found.dedup_by(|left, right| left.depth == right.depth && left.unit == right.unit);
    found
}

/// Walk the reverse graph from `target` back to the nearest seed, returning the
/// forward chain seed -> ... -> target. Each hop carries the call site that
/// pulled it in; the seed has no call site.
pub fn why(index: &Index, seeds: &[Unit], target: &Unit, max_depth: u32) -> Option<Vec<WhyHop>> {
    let seed_set: HashSet<Unit> = seeds.iter().cloned().collect();
    if seed_set.contains(target) {
        return Some(vec![WhyHop {
            unit: target.clone(),
            line: 0,
            reason: Reason::Seed,
        }]);
    }
    let mut graph = build_graph(
        index,
        seeds,
        ResolveOptions {
            depth: max_depth,
            same_package: false,
            minimal: true,
        },
    );
    let inc = graph.reverse();

    // parent[caller] = (callee, line of caller -> callee, edge reason).
    let mut parent: HashMap<Unit, (Unit, u32, Reason)> = HashMap::new();
    let mut seen: HashSet<Unit> = HashSet::new();
    seen.insert(target.clone());
    let mut queue: VecDeque<Unit> = VecDeque::new();
    queue.push_back(target.clone());

    let mut from_seed: Option<Unit> = None;
    while let Some(current) = queue.pop_front() {
        if seed_set.contains(&current) {
            from_seed = Some(current);
            break;
        }
        let Some(edges) = inc.get(&current) else {
            continue;
        };
        let mut edges: Vec<&Edge> = edges.iter().collect();
        sort_edges(index, &mut edges);
        for edge in edges {
            if !seen.insert(edge.to.clone()) {
                continue;
            }
            parent.insert(edge.to.clone(), (current.clone(), edge.line, edge.reason));
            queue.push_back(edge.to.clone());
        }
    }

    // Walking the reverse graph backwards left `parent[caller] = (callee,
    // line, reason)` for every caller we reached, so the forward chain can be
    // read from the seed towards the target.
    let seed = from_seed?;
    let mut chain: Vec<WhyHop> = vec![WhyHop {
        unit: seed.clone(),
        line: 0,
        reason: Reason::Seed,
    }];
    let mut node = seed;
    while node != *target {
        let (next, line, reason) = parent.get(&node)?.clone();
        chain.push(WhyHop {
            unit: next.clone(),
            line,
            reason,
        });
        node = next;
    }
    Some(chain)
}

/// All indexed use/call sites of `name`, in deterministic path/line order.
/// This is an inversion of the parser's recorded edges, not a text search:
/// comments, string literals and body text are deliberately out of scope.
pub fn references(index: &Index, name: &str) -> Vec<Reference> {
    let mut found = Vec::new();
    for (position, file) in index.files.iter().enumerate() {
        for symbol in &file.symbols {
            if symbol.uses.iter().any(|used| used == name) {
                found.push(Reference {
                    file: position,
                    symbol: symbol.name.clone(),
                    member: None,
                    line: symbol.line as u32,
                    kind: RefKind::Use,
                });
            }
            for call in &symbol.calls {
                if call.name == name {
                    found.push(Reference {
                        file: position,
                        symbol: symbol.name.clone(),
                        member: None,
                        line: call.line as u32,
                        kind: RefKind::Call,
                    });
                }
            }
            for member in &symbol.members {
                for call in &member.calls {
                    if call.name == name {
                        found.push(Reference {
                            file: position,
                            symbol: symbol.name.clone(),
                            member: Some(member.name.clone()),
                            line: call.line as u32,
                            kind: RefKind::Call,
                        });
                    }
                }
            }
        }
    }
    found.sort_by(|left, right| {
        index.files[left.file]
            .rel
            .cmp(&index.files[right.file].rel)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.symbol.cmp(&right.symbol))
            .then_with(|| left.member.cmp(&right.member))
            .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
    });
    found
}

fn import_reason(kind: ImportKind) -> Reason {
    match kind {
        ImportKind::Type => Reason::Import,
        ImportKind::Static => Reason::StaticImport,
        ImportKind::Wildcard => Reason::WildcardImport,
        ImportKind::Module => Reason::ModuleImport,
    }
}

/// Neighbours of a whole-file unit, all as whole-file units. This is the
/// original file-granularity behaviour.
fn file_neighbours(index: &Index, position: usize, options: ResolveOptions) -> Vec<(Unit, Reason)> {
    let Some(file) = index.files.get(position) else {
        return Vec::new();
    };
    let mut neighbours: Vec<(Unit, Reason)> = Vec::new();
    for import in &file.imports {
        neighbours.extend(
            resolve_import(index, file, import)
                .into_iter()
                .map(|target| (Unit::File(target), import_reason(import.kind))),
        );
    }
    neighbours.extend(
        resolve_uses(index, file)
            .into_iter()
            .map(|(target, reason)| (Unit::File(target), reason)),
    );

    if options.same_package {
        if let Some(package) = &file.package {
            let family = family_of(file);
            neighbours.extend(
                package_files(index, package, family)
                    .into_iter()
                    .map(|target| (Unit::File(target), Reason::Package)),
            );
        }
    }
    neighbours
}

/// Every symbol of `file`, as symbol units. Used for same-package expansion.
fn file_symbols(index: &Index, position: usize) -> Vec<Unit> {
    index
        .files
        .get(position)
        .map(|file| {
            file.symbols
                .iter()
                .map(|symbol| Unit::Symbol {
                    file: position,
                    symbol: symbol.name.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Simple name of an import target (`a.b.C` -> `C`, `a.b.*` -> none).
fn import_leaf(import: &Import) -> Option<&str> {
    if import.is_wildcard() || import.kind == ImportKind::Module {
        return None;
    }
    import
        .path
        .rsplit('.')
        .next()
        .filter(|name| !name.is_empty())
}

/// Resolves the imports of a symbol's file. When the import names a type, the
/// matching symbol is the unit; otherwise the whole file is.
fn symbol_imports(index: &Index, file: &FileEntry) -> Vec<(Unit, Reason)> {
    let mut out = Vec::new();
    for import in &file.imports {
        let reason = import_reason(import.kind);
        for target in resolve_import(index, file, import) {
            let symbol = import_leaf(import).and_then(|leaf| {
                index
                    .files
                    .get(target)
                    .and_then(|target_file| target_file.symbol(leaf))
                    .map(|symbol| Unit::Symbol {
                        file: target,
                        symbol: symbol.name.clone(),
                    })
            });
            out.push((symbol.unwrap_or(Unit::File(target)), reason));
        }
    }
    out
}

/// Neighbours of one symbol excerpt: the file's imports (mapped to symbols),
/// the names the symbol itself uses, and optionally its package siblings.
fn symbol_neighbours(
    index: &Index,
    file_position: usize,
    symbol_name: &str,
    options: ResolveOptions,
) -> Vec<(Unit, Reason)> {
    let Some(file) = index.files.get(file_position) else {
        return Vec::new();
    };
    let Some(symbol) = file.symbol(symbol_name) else {
        return Vec::new();
    };
    let family = family_of(file);

    let mut neighbours = symbol_imports(index, file);

    let imported: HashSet<&str> = file
        .imports
        .iter()
        .filter(|import| import.kind != ImportKind::Module)
        .filter(|import| !import.is_wildcard())
        .filter_map(|import| import.path.rsplit('.').next())
        .collect();

    for name in symbol
        .calls
        .iter()
        .map(|call| &call.name)
        .chain(symbol.uses.iter())
    {
        if name == symbol_name || imported.contains(name.as_str()) {
            continue;
        }
        // A sibling in the same file wins, exactly like the IDE resolves it.
        if let Some(sibling) = file.symbol(name) {
            neighbours.push((
                Unit::Symbol {
                    file: file_position,
                    symbol: sibling.name.clone(),
                },
                if family == Some(Family::Fe) {
                    Reason::Usage
                } else {
                    Reason::SamePackage
                },
            ));
            continue;
        }

        let Some(candidates) = index.by_symbol.get(name) else {
            continue;
        };
        // A Scala class and its companion object declare the same name twice
        // in one file; they are one target, not an ambiguity.
        let mut candidates: Vec<&(usize, usize)> = candidates
            .iter()
            .filter(|(position, _)| *position != file_position)
            .filter(|(position, _)| compatible(index, *position, family))
            .collect();
        candidates.dedup_by(|a, b| a.0 == b.0);
        let same_package: Vec<&(usize, usize)> = if family == Some(Family::Fe) {
            Vec::new()
        } else {
            candidates
                .iter()
                .copied()
                .filter(|(position, _)| index.files[*position].package == file.package)
                .collect()
        };
        let picked = match same_package.len() {
            1 => Some((same_package[0], Reason::SamePackage)),
            0 if candidates.len() == 1 => Some((candidates[0], Reason::Usage)),
            _ => None,
        };
        if let Some(((target_file, symbol_index), reason)) = picked {
            if let Some(target) = index.files[*target_file].symbols.get(*symbol_index) {
                neighbours.push((
                    Unit::Symbol {
                        file: *target_file,
                        symbol: target.name.clone(),
                    },
                    reason,
                ));
            }
        }
    }

    if options.same_package {
        if let Some(package) = &file.package {
            for target in index.by_package.get(package).into_iter().flatten() {
                neighbours.extend(
                    file_symbols(index, *target)
                        .into_iter()
                        .map(|unit| (unit, Reason::Package)),
                );
            }
        }
    }

    neighbours
}

/// Neighbours of one method excerpt: the methods this member calls. The class
/// shell already carries package/imports/fields/signatures, so member bundles
/// follow calls only.
fn member_neighbours(index: &Index, unit: &Unit, diagnostics: &mut Vec<Diagnostic>) -> Vec<Edge> {
    let Unit::Member {
        file: file_position,
        class: class_name,
        member: member_name,
    } = unit
    else {
        return Vec::new();
    };
    let Some(file) = index.files.get(*file_position) else {
        return Vec::new();
    };
    let Some(class) = file.symbol(class_name) else {
        return Vec::new();
    };
    let Some(member) = class
        .members
        .iter()
        .find(|member| member.kind == MemberKind::Method && member.name == *member_name)
    else {
        return Vec::new();
    };
    let family = family_of(file);

    let mut edges = Vec::new();
    for call in &member.calls {
        edges.extend(resolve_call(
            index,
            *file_position,
            file,
            class,
            member,
            call,
            family,
            diagnostics,
        ));
    }
    edges
}

fn call_edges(units: Vec<Unit>, line: u32) -> Vec<Edge> {
    units
        .into_iter()
        .map(|to| Edge {
            to,
            reason: Reason::Call,
            line,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)] // file, class, member, call, diagnostics
fn resolve_call(
    index: &Index,
    file_position: usize,
    file: &FileEntry,
    class: &Symbol,
    member: &Member,
    call: &crate::lang::Call,
    family: Option<Family>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Edge> {
    let line = call.line as u32;
    let path = file.rel.clone();

    match &call.receiver {
        Receiver::Bare | Receiver::This => {
            let mut visited = HashSet::new();
            let mut found = find_methods(
                index,
                file_position,
                class,
                &call.name,
                family,
                &mut visited,
            );
            if found.is_empty() && call.receiver == Receiver::Bare {
                found = static_import_methods(index, file, &call.name);
            }
            if found.is_empty() && family == Some(Family::Rust) {
                found = rust_function_units(index, &call.name, family, Some(file_position));
            }
            if found.is_empty() {
                diagnostics.push(Diagnostic {
                    kind: DiagnosticKind::UnresolvedCall,
                    path,
                    line,
                    name: call.name.clone(),
                });
            }
            call_edges(found, line)
        }
        Receiver::Super => {
            let found = super_methods(index, file_position, class, &call.name, family);
            if found.is_empty() {
                diagnostics.push(Diagnostic {
                    kind: DiagnosticKind::MethodNotFound,
                    path,
                    line,
                    name: call.name.clone(),
                });
            }
            call_edges(found, line)
        }
        Receiver::Name(name) => {
            let type_name = member
                .params
                .iter()
                .chain(member.locals.iter())
                .find(|(local, _)| local == name)
                .map(|(_, ty)| ty.clone())
                .or_else(|| {
                    class
                        .fields
                        .iter()
                        .find(|(field, _)| field == name)
                        .map(|(_, ty)| ty.clone())
                })
                .unwrap_or_else(|| name.clone());

            if type_name == class.name {
                let mut visited = HashSet::new();
                let found = find_methods(
                    index,
                    file_position,
                    class,
                    &call.name,
                    family,
                    &mut visited,
                );
                if found.is_empty() {
                    diagnostics.push(Diagnostic {
                        kind: DiagnosticKind::MethodNotFound,
                        path,
                        line,
                        name: call.name.clone(),
                    });
                }
                return call_edges(found, line);
            }

            let Some((target_file, target_index)) = resolve_class(index, file, &type_name, family)
            else {
                if family == Some(Family::Rust) {
                    let found = rust_function_units(index, &call.name, family, Some(file_position));
                    if found.is_empty() {
                        diagnostics.push(Diagnostic {
                            kind: DiagnosticKind::UnresolvedCall,
                            path,
                            line,
                            name: type_name.clone(),
                        });
                    }
                    return call_edges(found, line);
                }
                let kind = if class_is_ambiguous(index, &type_name, family) {
                    DiagnosticKind::Ambiguous
                } else {
                    DiagnosticKind::UnresolvedCall
                };
                diagnostics.push(Diagnostic {
                    kind,
                    path,
                    line,
                    name: type_name,
                });
                return Vec::new();
            };

            let target = &index.files[target_file].symbols[target_index];
            let mut visited = HashSet::new();
            let methods =
                find_methods(index, target_file, target, &call.name, family, &mut visited);
            if methods.is_empty() {
                // We know the class but not the member: keep the whole class
                // rather than silently dropping a method.
                diagnostics.push(Diagnostic {
                    kind: DiagnosticKind::MethodNotFound,
                    path,
                    line,
                    name: call.name.clone(),
                });
                vec![Edge {
                    to: Unit::Symbol {
                        file: target_file,
                        symbol: target.name.clone(),
                    },
                    reason: Reason::Call,
                    line,
                }]
            } else {
                call_edges(methods, line)
            }
        }
        Receiver::Unknown => {
            diagnostics.push(Diagnostic {
                kind: DiagnosticKind::UnknownReceiver,
                path,
                line,
                name: call.name.clone(),
            });
            Vec::new()
        }
    }
}

/// Whether a simple name is declared by more than one file. Only reachable
/// after [`resolve_class`] has already failed, so an ambiguity here means the
/// global fallback had several candidates.
fn class_is_ambiguous(index: &Index, name: &str, family: Option<Family>) -> bool {
    let Some(candidates) = index.by_symbol.get(name) else {
        return false;
    };
    let mut by_file: Vec<usize> = candidates
        .iter()
        .map(|(position, _)| *position)
        .filter(|position| compatible(index, *position, family))
        .collect();
    by_file.dedup();
    by_file.len() > 1
}

fn methods_named<'a>(class: &'a Symbol, name: &str) -> Vec<&'a Member> {
    class
        .members
        .iter()
        .filter(|member| member.kind == MemberKind::Method && member.name == name)
        .collect()
}

fn method_units(file: usize, class: &Symbol, name: &str) -> Vec<Unit> {
    let mut seen = HashSet::new();
    methods_named(class, name)
        .into_iter()
        .filter(|member| seen.insert(member.name.clone()))
        .map(|member| Unit::Member {
            file,
            class: class.name.clone(),
            member: member.name.clone(),
        })
        .collect()
}

/// Methods with `name` in `class` or its supertypes (resolved inside the repo).
fn find_methods(
    index: &Index,
    file_position: usize,
    class: &Symbol,
    name: &str,
    family: Option<Family>,
    visited: &mut HashSet<(usize, String)>,
) -> Vec<Unit> {
    if !visited.insert((file_position, class.name.clone())) {
        return Vec::new();
    }
    let direct = method_units(file_position, class, name);
    if !direct.is_empty() {
        return direct;
    }
    let Some(file) = index.files.get(file_position) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for supertype in &class.supertypes {
        if let Some((target_file, target_index)) = resolve_class(index, file, supertype, family) {
            let target = &index.files[target_file].symbols[target_index];
            out.extend(find_methods(
                index,
                target_file,
                target,
                name,
                family,
                visited,
            ));
        }
    }
    out
}

fn super_methods(
    index: &Index,
    file_position: usize,
    class: &Symbol,
    name: &str,
    family: Option<Family>,
) -> Vec<Unit> {
    let mut visited = HashSet::new();
    visited.insert((file_position, class.name.clone()));
    let Some(file) = index.files.get(file_position) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for supertype in &class.supertypes {
        if let Some((target_file, target_index)) = resolve_class(index, file, supertype, family) {
            let target = &index.files[target_file].symbols[target_index];
            out.extend(find_methods(
                index,
                target_file,
                target,
                name,
                family,
                &mut visited,
            ));
        }
    }
    out
}

/// Bare call resolved through `import static pkg.Helpers.helper` or
/// `import static pkg.Helpers.*`. Package wildcards are ignored: they import
/// types, not methods.
fn static_import_methods(index: &Index, file: &FileEntry, name: &str) -> Vec<Unit> {
    let mut out = Vec::new();
    for import in &file.imports {
        let single =
            import.kind == ImportKind::Static && import.path.rsplit('.').next() == Some(name);
        if !single && import.kind != ImportKind::Wildcard {
            continue;
        }
        let targets = resolve_import(index, file, import);
        if import.kind == ImportKind::Wildcard && targets.len() != 1 {
            continue;
        }
        for target in targets {
            let target_file = &index.files[target];
            for symbol in &target_file.symbols {
                let units = method_units(target, symbol, name);
                if !units.is_empty() {
                    out.extend(units);
                }
            }
        }
    }
    out
}

/// Resolves a simple type name to the top-level symbol that declares it, using
/// the same precedence as Java: same file, explicit imports, same package,
/// wildcard imports, unique in the project.
fn resolve_class(
    index: &Index,
    file: &FileEntry,
    name: &str,
    family: Option<Family>,
) -> Option<(usize, usize)> {
    if name.is_empty() {
        return None;
    }
    if file.symbol(name).is_some() {
        let position = *index.by_rel.get(&file.rel)?;
        let index_in_file = file.symbols.iter().position(|s| s.name == name)?;
        return Some((position, index_in_file));
    }

    // Module languages resolve types only through their explicit imports.
    if family == Some(Family::Web) {
        for import in &file.imports {
            if import.kind != ImportKind::Module {
                continue;
            }
            for target in resolve_import(index, file, import) {
                if let Some(symbol_index) = index.files[target]
                    .symbols
                    .iter()
                    .position(|symbol| symbol.name == name)
                {
                    return Some((target, symbol_index));
                }
            }
        }
        return None;
    }

    for import in &file.imports {
        if import.kind == ImportKind::Module || import.is_wildcard() {
            continue;
        }
        for target in resolve_import(index, file, import) {
            if let Some(symbol_index) = index.files[target]
                .symbols
                .iter()
                .position(|symbol| symbol.name == name)
            {
                return Some((target, symbol_index));
            }
        }
    }

    if let Some(package) = &file.package {
        if let Some(files) = index.by_package.get(package) {
            for target in files {
                if !compatible(index, *target, family) {
                    continue;
                }
                if let Some(symbol_index) = index.files[*target]
                    .symbols
                    .iter()
                    .position(|symbol| symbol.name == name)
                {
                    return Some((*target, symbol_index));
                }
            }
        }
    }

    for import in &file.imports {
        if import.kind == ImportKind::Module || !import.is_wildcard() {
            continue;
        }
        let targets = resolve_import(index, file, import);
        if targets.len() != 1 {
            continue;
        }
        for target in targets {
            if let Some(symbol_index) = index.files[target]
                .symbols
                .iter()
                .position(|symbol| symbol.name == name)
            {
                return Some((target, symbol_index));
            }
        }
    }

    let candidates = index
        .by_symbol
        .get(name)?
        .iter()
        .filter(|(position, _)| compatible(index, *position, family))
        .collect::<Vec<_>>();
    let mut by_file = candidates.clone();
    by_file.dedup_by(|a, b| a.0 == b.0);
    (by_file.len() == 1).then(|| (by_file[0].0, by_file[0].1))
}

/// Source line of a unit for edges that have no call site (imports, type
/// uses). Whole files have no single line, so they use 0.
fn unit_line(index: &Index, unit: &Unit) -> u32 {
    let Some(file) = index.files.get(unit.file()) else {
        return 0;
    };
    match unit {
        Unit::File(_) => 0,
        Unit::Symbol { symbol, .. } => file.symbol(symbol).map_or(0, |symbol| symbol.line as u32),
        Unit::Member { class, member, .. } => file
            .symbol(class)
            .and_then(|class| {
                class.members.iter().find(|candidate| {
                    candidate.kind == MemberKind::Method && candidate.name == *member
                })
            })
            .map_or(0, |member| member.line as u32),
    }
}

fn with_line(units: Vec<(Unit, Reason)>, line: u32) -> Vec<Edge> {
    units
        .into_iter()
        .map(|(to, reason)| Edge { to, reason, line })
        .collect()
}

/// Neighbours of one unit. Every edge carries a line: the call site for call
/// edges, the source unit's line for import/type edges.
fn unit_neighbours(
    index: &Index,
    unit: &Unit,
    options: ResolveOptions,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Edge> {
    match unit {
        Unit::File(position) if options.minimal => call_graph_seed(index, *position, diagnostics)
            .unwrap_or_else(|| with_line(file_neighbours(index, *position, options), 0)),
        Unit::Symbol { file, symbol } if options.minimal => {
            call_graph_class(index, *file, symbol, diagnostics).unwrap_or_else(|| {
                with_line(
                    symbol_neighbours(index, *file, symbol, options),
                    unit_line(index, unit),
                )
            })
        }
        Unit::File(position) => with_line(file_neighbours(index, *position, options), 0),
        Unit::Symbol { file, symbol } => with_line(
            symbol_neighbours(index, *file, symbol, options),
            unit_line(index, unit),
        ),
        Unit::Member { .. } => member_neighbours(index, unit, diagnostics),
    }
}

/// Calls of every method in every class of a file, for a whole-file seed.
/// `None` keeps the old import/type closure for files that are not
/// member-capable or have no symbols.
fn call_graph_seed(
    index: &Index,
    position: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<Edge>> {
    let file = index.files.get(position)?;
    if file.language()?.granularity() != Granularity::Member || file.symbols.is_empty() {
        return None;
    }
    let family = family_of(file);
    let mut edges = Vec::new();
    for class in &file.symbols {
        edges.extend(class_calls(
            index,
            position,
            file,
            class,
            family,
            diagnostics,
        ));
    }
    let classes: Vec<&Symbol> = file.symbols.iter().collect();
    type_only_diagnostics(index, file, &classes, &edges, family, diagnostics);
    Some(edges)
}

/// Calls of every method of one class, for a class seed (`path::Class`).
fn call_graph_class(
    index: &Index,
    position: usize,
    class_name: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<Edge>> {
    let file = index.files.get(position)?;
    if file.language()?.granularity() != Granularity::Member {
        return None;
    }
    let class = file.symbol(class_name)?;
    let family = family_of(file);
    let edges = class_calls(index, position, file, class, family, diagnostics);
    type_only_diagnostics(index, file, &[class], &edges, family, diagnostics);
    Some(edges)
}

fn class_calls(
    index: &Index,
    position: usize,
    file: &FileEntry,
    class: &Symbol,
    family: Option<Family>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Edge> {
    let mut out = Vec::new();
    for member in class
        .members
        .iter()
        .filter(|member| member.kind == MemberKind::Method)
    {
        for call in &member.calls {
            out.extend(resolve_call(
                index,
                position,
                file,
                class,
                member,
                call,
                family,
                diagnostics,
            ));
        }
    }
    // Rust free functions are top-level symbols with no members. Their calls
    // are recorded by name, so resolve them directly.
    if family == Some(Family::Rust) {
        for call in &class.calls {
            let found = rust_function_units(index, &call.name, family, Some(position));
            if found.is_empty() {
                diagnostics.push(Diagnostic {
                    kind: DiagnosticKind::UnresolvedCall,
                    path: file.rel.clone(),
                    line: call.line as u32,
                    name: call.name.clone(),
                });
            }
            out.extend(call_edges(found, call.line as u32));
        }
    }
    out
}

/// Types that are in scope but not on the call path. Minimal bundles
/// deliberately skip them; #8 records the fact, #10 renders it.
fn type_only_diagnostics(
    index: &Index,
    file: &FileEntry,
    classes: &[&Symbol],
    edges: &[Edge],
    family: Option<Family>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let targets: HashSet<&str> = edges.iter().filter_map(|edge| edge.to.symbol()).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    for class in classes {
        for name in &class.uses {
            if name == &class.name || targets.contains(name.as_str()) || !seen.insert(name.as_str())
            {
                continue;
            }
            if resolve_class(index, file, name, family).is_some() {
                diagnostics.push(Diagnostic {
                    kind: DiagnosticKind::TypeOnlySkipped,
                    path: file.rel.clone(),
                    line: class.line as u32,
                    name: name.clone(),
                });
            }
        }
    }
}

/// Breadth-first closure over units, keeping track of why each one was pulled
/// in. Returns units in discovery order, seeds first (depth 0), duplicates
/// removed. A whole-file unit supersedes symbol units of the same file.
/// Resolves every reachable unit up to `options.depth` and keeps the outgoing
/// edges. `out` is enough for the closure; `inc` is built on demand.
pub fn build_graph(index: &Index, seeds: &[Unit], options: ResolveOptions) -> Graph {
    let mut graph = Graph::default();
    let mut visited: HashSet<Unit> = HashSet::new();
    let mut queue: VecDeque<(Unit, u32)> = VecDeque::new();

    for seed in seeds {
        if visited.insert(seed.clone()) {
            graph.out.entry(seed.clone()).or_default();
            queue.push_back((seed.clone(), 0));
        }
    }

    while let Some((unit, depth)) = queue.pop_front() {
        if depth >= options.depth {
            continue;
        }
        let mut diagnostics = Vec::new();
        let edges = unit_neighbours(index, &unit, options, &mut diagnostics);
        graph.diagnostics.append(&mut diagnostics);
        graph.out.insert(unit.clone(), edges);
        let edges = &graph.out[&unit];
        for edge in edges {
            if visited.insert(edge.to.clone()) {
                queue.push_back((edge.to.clone(), depth + 1));
            }
        }
    }
    graph.note_unresolved_imports(index);
    // Stable order and no duplicates: the same unit can be expanded once but
    // contribute the same note from several seeds.
    graph.diagnostics.sort_by(|left, right| {
        (
            left.path.as_str(),
            left.line,
            left.kind.as_str(),
            left.name.as_str(),
        )
            .cmp(&(
                right.path.as_str(),
                right.line,
                right.kind.as_str(),
                right.name.as_str(),
            ))
    });
    graph.diagnostics.dedup();
    graph
}

/// Breadth-first closure over units, keeping track of why each one was pulled
/// in. Returns units in discovery order, seeds first (depth 0), duplicates
/// removed. A whole-file unit supersedes symbol units of the same file.
///
/// Production callers use [`build_graph`] directly so they also get the
/// diagnostics; this shorthand stays for the tests.
#[cfg(test)]
pub fn closure_units(
    index: &Index,
    seeds: &[Unit],
    options: ResolveOptions,
) -> Vec<(Unit, u32, Reason)> {
    build_graph(index, seeds, options).closure(seeds)
}

/// A file that is bundled whole already contains its symbols, so keep only the
/// file unit and drop the excerpts.
pub fn drop_covered_symbols(units: Vec<(Unit, u32, Reason)>) -> Vec<(Unit, u32, Reason)> {
    let files: HashSet<usize> = units
        .iter()
        .filter_map(|(unit, _, _)| match unit {
            Unit::File(file) => Some(*file),
            _ => None,
        })
        .collect();
    let classes: HashSet<(usize, String)> = units
        .iter()
        .filter_map(|(unit, _, _)| match unit {
            Unit::Symbol { file, symbol } => Some((*file, symbol.clone())),
            _ => None,
        })
        .collect();
    units
        .into_iter()
        .filter(|(unit, _, _)| match unit {
            Unit::File(_) => true,
            Unit::Symbol { file, .. } => !files.contains(file),
            Unit::Member { file, class, .. } => {
                !files.contains(file) && !classes.contains(&(*file, class.clone()))
            }
        })
        .collect()
}

/// Breadth-first closure over imports and type usages, keeping track of why
/// each file was pulled in. Returns `(file index, depth, reason)` in discovery
/// order, seeds first (depth 0), duplicates removed.
#[cfg(test)]
pub fn closure_detailed(
    index: &Index,
    seeds: &[usize],
    options: ResolveOptions,
) -> Vec<(usize, u32, Reason)> {
    let units: Vec<Unit> = seeds.iter().copied().map(Unit::File).collect();
    closure_units(index, &units, options)
        .into_iter()
        .filter_map(|(unit, depth, reason)| match unit {
            Unit::File(position) => Some((position, depth, reason)),
            Unit::Symbol { .. } | Unit::Member { .. } => None,
        })
        .collect()
}

/// Closure without provenance, for plain listings.
#[cfg(test)]
pub fn closure(index: &Index, seeds: &[usize], options: ResolveOptions) -> Vec<(usize, u32)> {
    closure_detailed(index, seeds, options)
        .into_iter()
        .map(|(position, depth, _)| (position, depth))
        .collect()
}

/// How many import statements in the whole index no indexed file provides.
/// This is `coverage.imports_unresolved` in the bundle JSON.
pub fn unresolved_import_count(index: &Index) -> usize {
    (0..index.files.len())
        .map(|position| unresolved_imports(index, position).len())
        .sum()
}

/// All imports of `file` that could not be resolved inside the project (JDK,
/// Scala stdlib, npm packages, ...). Useful for heads-up messages.
pub fn unresolved_imports(index: &Index, file_position: usize) -> Vec<String> {
    let Some(file) = index.files.get(file_position) else {
        return Vec::new();
    };
    file.imports
        .iter()
        .filter(|import| resolve_import(index, file, import).is_empty())
        .map(|import| import.path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileEntry;
    use crate::lang::{ImportKind, Symbol, SymbolKind};
    use std::path::PathBuf;

    fn entry(rel: &str, package: Option<&str>, types: &[&str], imports: Vec<Import>) -> FileEntry {
        FileEntry {
            rel: rel.to_string(),
            abs: PathBuf::from(format!("/repo/{rel}")),
            package: package.map(str::to_string),
            types: types.iter().map(|ty| ty.to_string()).collect(),
            imports,
            uses: Vec::new(),
            symbols: Vec::new(),
            preamble_end: 0,
        }
    }

    fn import(path: &str, kind: ImportKind) -> Import {
        Import {
            path: path.to_string(),
            kind,
        }
    }

    fn index_from(files: Vec<FileEntry>) -> Index {
        let mut index = Index {
            root: PathBuf::from("/repo"),
            files,
            ..Default::default()
        };
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
        index
    }

    fn fe_symbol(
        name: &str,
        kind: SymbolKind,
        start: usize,
        end: usize,
        uses: &[&str],
        calls: &[&str],
    ) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            start,
            end,
            line: 1,
            end_line: 1,
            uses: uses.iter().map(|name| name.to_string()).collect(),
            calls: calls
                .iter()
                .map(|name| crate::lang::Call {
                    name: name.to_string(),
                    receiver: Receiver::Bare,
                    line: 1,
                })
                .collect(),
            members: Vec::new(),
            fields: Vec::new(),
            supertypes: Vec::new(),
        }
    }

    fn fe_fixture() -> Index {
        let mut main = entry("main.fe", None, &["main", "other"], vec![]);
        main.symbols = vec![
            fe_symbol("main", SymbolKind::Function, 0, 10, &["limit"], &["helper"]),
            fe_symbol("other", SymbolKind::Function, 11, 20, &[], &[]),
        ];
        let mut lib = entry("lib.fe", None, &["helper", "limit", "unused"], vec![]);
        lib.symbols = vec![
            fe_symbol("helper", SymbolKind::Function, 0, 5, &[], &["fmt"]),
            fe_symbol("fmt", SymbolKind::Function, 6, 9, &[], &[]),
            fe_symbol("limit", SymbolKind::Declaration, 10, 14, &[], &[]),
            fe_symbol("unused", SymbolKind::Function, 15, 22, &[], &[]),
        ];
        index_from(vec![main, lib])
    }

    fn java_method(
        name: &str,
        params: &[(&str, &str)],
        locals: &[(&str, &str)],
        calls: &[(&str, Receiver)],
    ) -> Member {
        Member {
            name: name.to_string(),
            kind: MemberKind::Method,
            start: 0,
            end: 10,
            line: 1,
            end_line: 1,
            body_start: Some(5),
            params: params
                .iter()
                .map(|(name, ty)| (name.to_string(), ty.to_string()))
                .collect(),
            locals: locals
                .iter()
                .map(|(name, ty)| (name.to_string(), ty.to_string()))
                .collect(),
            calls: calls
                .iter()
                .map(|(name, receiver)| crate::lang::Call {
                    name: name.to_string(),
                    receiver: receiver.clone(),
                    line: 1,
                })
                .collect(),
        }
    }

    fn java_class(
        name: &str,
        members: Vec<Member>,
        fields: &[(&str, &str)],
        supertypes: &[&str],
    ) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind: SymbolKind::Type,
            start: 0,
            end: 100,
            line: 1,
            end_line: 1,
            uses: Vec::new(),
            calls: Vec::new(),
            members,
            fields: fields
                .iter()
                .map(|(name, ty)| (name.to_string(), ty.to_string()))
                .collect(),
            supertypes: supertypes.iter().map(|name| name.to_string()).collect(),
        }
    }

    fn java_fixture() -> Index {
        let mut service = entry("app/Service.java", Some("app"), &["Service"], vec![]);
        service.symbols = vec![java_class(
            "Service",
            vec![
                java_method(
                    "used",
                    &[("helper", "Helper"), ("local", "Local")],
                    &[],
                    &[
                        ("internal", Receiver::Bare),
                        ("pong", Receiver::Name("helper".to_string())),
                        ("ping", Receiver::Name("local".to_string())),
                        ("staticCall", Receiver::Name("Repo".to_string())),
                        ("inherited", Receiver::Bare),
                        ("parent", Receiver::Super),
                        ("size", Receiver::Name("repo".to_string())),
                    ],
                ),
                java_method("internal", &[], &[], &[]),
                java_method("omitted", &[], &[], &[]),
            ],
            &[("repo", "Repo")],
            &["Base"],
        )];

        let mut base = entry("app/Base.java", Some("app"), &["Base"], vec![]);
        base.symbols = vec![java_class(
            "Base",
            vec![
                java_method("inherited", &[], &[], &[]),
                java_method("parent", &[], &[], &[]),
                java_method("baseOnly", &[], &[], &[]),
            ],
            &[],
            &[],
        )];

        let mut repo = entry("app/Repo.java", Some("app"), &["Repo"], vec![]);
        repo.symbols = vec![java_class(
            "Repo",
            vec![
                java_method("size", &[], &[], &[]),
                java_method("unused", &[], &[], &[]),
                java_method("staticCall", &[], &[], &[]),
            ],
            &[],
            &[],
        )];

        let mut helper = entry("app/Helper.java", Some("app"), &["Helper"], vec![]);
        helper.symbols = vec![java_class(
            "Helper",
            vec![java_method("pong", &[], &[], &[])],
            &[],
            &[],
        )];

        let mut local = entry("app/Local.java", Some("app"), &["Local"], vec![]);
        local.symbols = vec![java_class(
            "Local",
            vec![java_method("ping", &[], &[], &[])],
            &[],
            &[],
        )];

        index_from(vec![service, base, repo, helper, local])
    }

    fn member_seed(file: usize, class: &str, member: &str) -> Unit {
        Unit::Member {
            file,
            class: class.to_string(),
            member: member.to_string(),
        }
    }

    /// Writes a small project to a temp directory and indexes it, so a test
    /// exercises the real parser plus the resolver.
    fn index_from_disk(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("filer-resolve-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, source) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("nested file")).unwrap();
            std::fs::write(path, source).unwrap();
        }
        root
    }

    #[test]
    fn typescript_method_and_file_seeds_use_the_call_graph() {
        let root = index_from_disk(
            "typescript",
            &[
                (
                    "app/Service.ts",
                    "import { Repo } from \"./Repo\";\nimport { Helper } from \"./Helper\";\nimport { Base } from \"./Base\";\n\nexport class Service extends Base {\n  private repo: Repo;\n\n  constructor(repo: Repo) { this.repo = repo; }\n\n  used(helper: Helper): number {\n    helper.pong();\n    return this.repo.size();\n  }\n\n  omitted(): number { return 1; }\n}\n",
                ),
                (
                    "app/Repo.ts",
                    "export class Repo {\n  size(): number { return 0; }\n  unused(): number { return 1; }\n}\n",
                ),
                (
                    "app/Helper.ts",
                    "export class Helper {\n  pong(): void {}\n  quiet(): void {}\n}\n",
                ),
                (
                    "app/Base.ts",
                    "export class Base {\n  parent(): void {}\n}\n",
                ),
            ],
        );
        let index = Index::build(&root);
        let service = index.by_rel["app/Service.ts"];

        let order = closure_units(
            &index,
            &[member_seed(service, "Service", "used")],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        let members = member_names(&order);
        for expected in [("Helper", "pong"), ("Repo", "size")] {
            assert!(
                members
                    .iter()
                    .any(|(class, member)| class == expected.0 && member == expected.1),
                "missing {expected:?} in {members:?}"
            );
        }
        assert!(!members
            .iter()
            .any(|(class, member)| class == "Repo" && member == "unused"));

        let order = closure_units(
            &index,
            &[Unit::File(service)],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        assert!(order.iter().any(|(unit, _, _)| {
            unit.symbol() == Some("Repo") && unit.member() == Some("size")
        }));
        assert!(!order
            .iter()
            .any(|(unit, _, _)| unit.member() == Some("unused")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scala_method_and_file_seeds_use_the_call_graph() {
        let root = index_from_disk(
            "scala",
            &[
                (
                    "app/Service.scala",
                    "package app\n\nclass Service(val repo: Repo) extends Base {\n  def used(helper: Helper): Int = {\n    helper.pong()\n    repo.size()\n  }\n\n  def omitted: Int = 1\n}\n",
                ),
                (
                    "app/Repo.scala",
                    "package app\n\nclass Repo {\n  def size(): Int = 0\n  def unused(): Int = 1\n}\n",
                ),
                (
                    "app/Helper.scala",
                    "package app\n\nclass Helper {\n  def pong(): Unit = {}\n  def quiet(): Unit = {}\n}\n",
                ),
                (
                    "app/Base.scala",
                    "package app\n\nclass Base {\n  def parent(): Unit = {}\n}\n",
                ),
            ],
        );
        let index = Index::build(&root);
        let service = index.by_rel["app/Service.scala"];

        let order = closure_units(
            &index,
            &[member_seed(service, "Service", "used")],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        let members = member_names(&order);
        for expected in [("Helper", "pong"), ("Repo", "size")] {
            assert!(
                members
                    .iter()
                    .any(|(class, member)| class == expected.0 && member == expected.1),
                "missing {expected:?} in {members:?}"
            );
        }
        assert!(!members
            .iter()
            .any(|(class, member)| class == "Repo" && member == "unused"));

        let order = closure_units(
            &index,
            &[Unit::File(service)],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        assert!(order.iter().any(|(unit, _, _)| {
            unit.symbol() == Some("Repo") && unit.member() == Some("size")
        }));
        assert!(!order
            .iter()
            .any(|(unit, _, _)| unit.member() == Some("unused")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn csharp_method_and_file_seeds_use_the_call_graph() {
        let root = index_from_disk(
            "csharp",
            &[
                (
                    "app/Service.cs",
                    "namespace app;\n\npublic class Service : Base {\n    private Repo repo;\n\n    public int Used(Helper helper) {\n        helper.Pong();\n        return repo.Size();\n    }\n\n    public int Omitted() { return 1; }\n}\n",
                ),
                (
                    "app/Repo.cs",
                    "namespace app;\n\npublic class Repo {\n    public int Size() { return 0; }\n    public int Unused() { return 1; }\n}\n",
                ),
                (
                    "app/Helper.cs",
                    "namespace app;\n\npublic class Helper {\n    public void Pong() {}\n    public void Quiet() {}\n}\n",
                ),
                (
                    "app/Base.cs",
                    "namespace app;\n\npublic class Base {\n    public void Parent() {}\n}\n",
                ),
            ],
        );
        let index = Index::build(&root);
        let service = index.by_rel["app/Service.cs"];

        let order = closure_units(
            &index,
            &[member_seed(service, "Service", "Used")],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        let members = member_names(&order);
        for expected in [("Helper", "Pong"), ("Repo", "Size")] {
            assert!(
                members
                    .iter()
                    .any(|(class, member)| class == expected.0 && member == expected.1),
                "missing {expected:?} in {members:?}"
            );
        }
        assert!(!members
            .iter()
            .any(|(class, member)| class == "Repo" && member == "Unused"));

        let order = closure_units(
            &index,
            &[Unit::File(service)],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        assert!(order.iter().any(|(unit, _, _)| {
            unit.symbol() == Some("Repo") && unit.member() == Some("Size")
        }));
        assert!(!order
            .iter()
            .any(|(unit, _, _)| unit.member() == Some("Unused")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rust_module_paths_and_method_call_graph() {
        let root = index_from_disk(
            "rust",
            &[
                (
                    "src/lib.rs",
                    "pub mod app;\npub use crate::app::Service;\n",
                ),
                (
                    "src/app.rs",
                    "pub mod service;\npub mod repo;\npub use service::Service;\n",
                ),
                (
                    "src/app/service.rs",
                    "mod sub;\n\nuse crate::app::repo::Repo;\nuse super::helper::Helper;\n\npub struct Service {\n    repo: Repo,\n}\n\nimpl Service {\n    pub fn used(&self, helper: Helper) -> i32 {\n        helper.pong();\n        self.repo.size();\n        internal();\n        free_helper();\n        sub_helper();\n        1\n    }\n\n    pub fn omitted(&self) -> i32 { 1 }\n\n    fn internal(&self) -> i32 { 2 }\n}\n",
                ),
                (
                    "src/app/service/sub.rs",
                    "pub fn sub_helper() {}\n",
                ),
                (
                    "src/app/repo.rs",
                    "use super::helper::Helper;\n\npub struct Repo;\n\nimpl Repo {\n    pub fn size(&self, helper: Helper) -> i32 { 0 }\n    pub fn unused(&self) -> i32 { 1 }\n}\n",
                ),
                (
                    "src/app/helper.rs",
                    "pub struct Helper;\n\nimpl Helper {\n    pub fn pong(&self) {}\n    pub fn quiet(&self) {}\n}\n\npub fn free_helper() {}\n",
                ),
            ],
        );
        let index = Index::build(&root);
        let service = index.by_rel["src/app/service.rs"];

        // `crate::`, `super::`, `self::` and `mod x;` all resolve by module
        // directory, including the app.rs (not app/mod.rs) parent module.
        assert_eq!(
            resolve_import(
                &index,
                &index.files[service],
                &import("crate.app.repo.Repo", ImportKind::Type)
            ),
            vec![index.by_rel["src/app/repo.rs"]]
        );
        // app.rs only re-exports Service, so follow the `pub use` to the file
        // that declares it.
        assert_eq!(
            resolve_import(
                &index,
                &index.files[service],
                &import("crate.app.Service", ImportKind::Type)
            ),
            vec![service]
        );
        assert_eq!(
            resolve_import(
                &index,
                &index.files[service],
                &import("super.helper.Helper", ImportKind::Type)
            ),
            vec![index.by_rel["src/app/helper.rs"]]
        );
        assert_eq!(
            resolve_import(
                &index,
                &index.files[service],
                &import("self.sub.sub_helper", ImportKind::Type)
            ),
            vec![index.by_rel["src/app/service/sub.rs"]]
        );
        // Unknown external crates never fall back to the crate root.
        assert!(resolve_import(
            &index,
            &index.files[service],
            &import("std.fmt.Debug", ImportKind::Type)
        )
        .is_empty());

        let order = closure_units(
            &index,
            &[member_seed(service, "Service", "used")],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        let members = member_names(&order);
        for expected in [
            ("Service", "internal"),
            ("Helper", "pong"),
            ("Repo", "size"),
        ] {
            assert!(
                members
                    .iter()
                    .any(|(class, member)| class == expected.0 && member == expected.1),
                "missing {expected:?} in {members:?}"
            );
        }
        for unexpected in [
            ("Service", "omitted"),
            ("Repo", "unused"),
            ("Helper", "quiet"),
        ] {
            assert!(
                !members
                    .iter()
                    .any(|(class, member)| class == unexpected.0 && member == unexpected.1),
                "unexpected {unexpected:?} in {members:?}"
            );
        }
        assert!(order
            .iter()
            .any(|(unit, _, _)| unit.symbol() == Some("free_helper")));
        assert!(order
            .iter()
            .any(|(unit, _, _)| unit.symbol() == Some("sub_helper")));

        // A whole-file seed follows the same calls and stubs unkept methods.
        let order = closure_units(
            &index,
            &[Unit::File(service)],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        assert!(order.iter().any(|(unit, _, _)| {
            unit.symbol() == Some("Repo") && unit.member() == Some("size")
        }));
        assert!(!order
            .iter()
            .any(|(unit, _, _)| unit.member() == Some("unused")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn groovy_method_and_file_seeds_use_the_call_graph() {
        let root = index_from_disk(
            "groovy",
            &[
                (
                    "app/Service.groovy",
                    "package app\n\nclass Service extends Base {\n    private Repo repo\n\n    int used(Helper helper) {\n        helper.pong()\n        return repo.size()\n    }\n\n    int omitted() { return 1 }\n}\n",
                ),
                (
                    "app/Repo.groovy",
                    "package app\n\nclass Repo {\n    int size() { return 0 }\n    int unused() { return 1 }\n}\n",
                ),
                (
                    "app/Helper.groovy",
                    "package app\n\nclass Helper {\n    void pong() {}\n    void quiet() {}\n}\n",
                ),
                (
                    "app/Base.groovy",
                    "package app\n\nclass Base {\n    void parent() {}\n}\n",
                ),
            ],
        );
        let index = Index::build(&root);
        let service = index.by_rel["app/Service.groovy"];

        let order = closure_units(
            &index,
            &[member_seed(service, "Service", "used")],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        let members = member_names(&order);
        for expected in [("Helper", "pong"), ("Repo", "size")] {
            assert!(
                members
                    .iter()
                    .any(|(class, member)| class == expected.0 && member == expected.1),
                "missing {expected:?} in {members:?}"
            );
        }
        assert!(!members
            .iter()
            .any(|(class, member)| class == "Repo" && member == "unused"));

        let order = closure_units(
            &index,
            &[Unit::File(service)],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        assert!(order.iter().any(|(unit, _, _)| {
            unit.symbol() == Some("Repo") && unit.member() == Some("size")
        }));
        assert!(!order
            .iter()
            .any(|(unit, _, _)| unit.member() == Some("unused")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn kotlin_method_and_file_seeds_use_the_call_graph() {
        let root = index_from_disk(
            "kotlin",
            &[
                (
                    "app/Service.kt",
                    "package app\n\nclass Service(private val repo: Repo) : Base() {\n    val helper: Helper = Helper()\n\n    fun used(): Int {\n        helper.pong()\n        return repo.size()\n    }\n\n    fun omitted(): Int = 1\n}\n",
                ),
                (
                    "app/Repo.kt",
                    "package app\n\nclass Repo {\n    fun size(): Int = 0\n    fun unused(): Int = 1\n}\n",
                ),
                (
                    "app/Helper.kt",
                    "package app\n\nclass Helper {\n    fun pong() {}\n    fun quiet() {}\n}\n",
                ),
                (
                    "app/Base.kt",
                    "package app\n\nopen class Base {\n    fun inherited() {}\n    fun baseOnly() {}\n}\n",
                ),
            ],
        );
        let index = Index::build(&root);
        let service = index.by_rel["app/Service.kt"];

        let order = closure_units(
            &index,
            &[member_seed(service, "Service", "used")],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        let members = member_names(&order);
        for expected in [("Helper", "pong"), ("Repo", "size")] {
            assert!(
                members
                    .iter()
                    .any(|(class, member)| class == expected.0 && member == expected.1),
                "missing {expected:?} in {members:?}"
            );
        }
        assert!(!members
            .iter()
            .any(|(class, member)| class == "Repo" && member == "unused"));

        // A file seed follows the same calls, but renders the file whole.
        let order = closure_units(
            &index,
            &[Unit::File(service)],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        assert!(order.iter().any(|(unit, _, _)| {
            unit.symbol() == Some("Repo") && unit.member() == Some("size")
        }));
        assert!(!order
            .iter()
            .any(|(unit, _, _)| unit.member() == Some("unused")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn java_member_closure_follows_calls_only() {
        let index = java_fixture();
        let order = closure_units(
            &index,
            &[member_seed(0, "Service", "used")],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: false,
            },
        );
        let members: Vec<(String, String)> = order
            .iter()
            .filter_map(|(unit, _, _)| {
                Some((unit.symbol()?.to_string(), unit.member()?.to_string()))
            })
            .collect();
        for expected in [
            ("Service", "used"),
            ("Service", "internal"),
            ("Helper", "pong"),
            ("Local", "ping"),
            ("Repo", "size"),
            ("Repo", "staticCall"),
            ("Base", "inherited"),
            ("Base", "parent"),
        ] {
            assert!(
                members
                    .iter()
                    .any(|(class, member)| class == expected.0 && member == expected.1),
                "missing {expected:?} in {members:?}"
            );
        }
        for unexpected in [
            ("Service", "omitted"),
            ("Repo", "unused"),
            ("Base", "baseOnly"),
        ] {
            assert!(
                !members
                    .iter()
                    .any(|(class, member)| class == unexpected.0 && member == unexpected.1),
                "unexpected {unexpected:?} in {members:?}"
            );
        }
        assert!(order
            .iter()
            .all(|(unit, _, _)| matches!(unit, Unit::Member { .. })));
        assert_eq!(order[0].0, member_seed(0, "Service", "used"));
    }

    #[test]
    fn java_member_closure_respects_depth() {
        let index = java_fixture();
        let order = closure_units(
            &index,
            &[member_seed(0, "Service", "used")],
            ResolveOptions {
                depth: 0,
                same_package: false,
                minimal: false,
            },
        );
        assert_eq!(order.len(), 1);
    }

    fn member_names(order: &[(Unit, u32, Reason)]) -> Vec<(String, String)> {
        order
            .iter()
            .filter_map(|(unit, _, _)| {
                Some((unit.symbol()?.to_string(), unit.member()?.to_string()))
            })
            .collect()
    }

    #[test]
    fn java_file_seed_uses_the_call_graph() {
        let index = java_fixture();
        let order = closure_units(
            &index,
            &[Unit::File(0)],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        let members = member_names(&order);
        for expected in [
            ("Helper", "pong"),
            ("Local", "ping"),
            ("Repo", "size"),
            ("Repo", "staticCall"),
            ("Base", "inherited"),
            ("Base", "parent"),
        ] {
            assert!(
                members
                    .iter()
                    .any(|(class, member)| class == expected.0 && member == expected.1),
                "missing {expected:?} in {members:?}"
            );
        }
        // Not the import/type closure: untouched files stay out entirely.
        assert!(!order
            .iter()
            .any(|(unit, _, _)| matches!(unit, Unit::File(_)) && unit.file() != 0));
        assert!(!members
            .iter()
            .any(|(class, member)| class == "Repo" && member == "unused"));
        assert_eq!(order[0], (Unit::File(0), 0, Reason::Seed));
    }

    #[test]
    fn java_class_seed_uses_the_call_graph() {
        let index = java_fixture();
        let order = closure_units(
            &index,
            &[Unit::Symbol {
                file: 0,
                symbol: "Service".to_string(),
            }],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        let members = member_names(&order);
        assert!(members
            .iter()
            .any(|(class, member)| class == "Repo" && member == "size"));
        assert!(!members
            .iter()
            .any(|(class, member)| class == "Repo" && member == "unused"));
        assert_eq!(
            order[0],
            (
                Unit::Symbol {
                    file: 0,
                    symbol: "Service".to_string(),
                },
                0,
                Reason::Seed,
            )
        );
    }

    #[test]
    fn unknown_member_falls_back_to_the_whole_class() {
        let mut index = java_fixture();
        index.files[0].symbols[0].members[0].calls = vec![crate::lang::Call {
            name: "notThere".to_string(),
            receiver: Receiver::Name("repo".to_string()),
            line: 1,
        }];
        let order = closure_units(
            &index,
            &[member_seed(0, "Service", "used")],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: false,
            },
        );
        assert!(order.iter().any(|(unit, _, _)| *unit
            == Unit::Symbol {
                file: 2,
                symbol: "Repo".to_string()
            }));
    }

    #[test]
    fn fe_symbols_resolve_calls_and_declarations_by_name() {
        let index = fe_fixture();
        let order = closure_units(
            &index,
            &[Unit::Symbol {
                file: 0,
                symbol: "main".to_string(),
            }],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: false,
            },
        );
        let units: Vec<(usize, &str, u32, Reason)> = order
            .iter()
            .map(|(unit, depth, reason)| {
                (unit.file(), unit.symbol().unwrap_or(""), *depth, *reason)
            })
            .collect();
        assert_eq!(
            units,
            vec![
                (0, "main", 0, Reason::Seed),
                (1, "helper", 1, Reason::Usage),
                (1, "limit", 1, Reason::Usage),
            ]
        );
        assert!(!units.iter().any(|(_, name, _, _)| *name == "unused"));
    }

    #[test]
    fn fe_symbol_closure_is_bounded_by_depth() {
        let index = fe_fixture();
        let order = closure_units(
            &index,
            &[Unit::Symbol {
                file: 0,
                symbol: "main".to_string(),
            }],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: false,
            },
        );
        assert!(!order
            .iter()
            .any(|(unit, _, _)| unit.symbol() == Some("fmt")));
    }

    #[test]
    fn a_whole_file_unit_supersedes_its_symbol_units() {
        let index = fe_fixture();
        let order = closure_units(
            &index,
            &[
                Unit::Symbol {
                    file: 0,
                    symbol: "main".to_string(),
                },
                Unit::File(0),
            ],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: false,
            },
        );
        assert_eq!(
            order.iter().filter(|(unit, _, _)| unit.file() == 0).count(),
            1
        );
        assert!(order.iter().any(|(unit, _, _)| *unit == Unit::File(0)));
    }

    fn fixture() -> Index {
        index_from(vec![
            entry(
                "src/main/java/app/Service.java",
                Some("app"),
                &["Service"],
                vec![
                    import("app.repo.UserRepository", ImportKind::Type),
                    import("app.model.User", ImportKind::Type),
                    import("java.util.List", ImportKind::Type),
                ],
            ),
            entry(
                "src/main/java/app/repo/UserRepository.java",
                Some("app.repo"),
                &["UserRepository"],
                vec![import("app.model.User", ImportKind::Type)],
            ),
            entry(
                "src/main/java/app/model/User.java",
                Some("app.model"),
                &["User"],
                vec![import("java.util.Objects", ImportKind::Type)],
            ),
            entry(
                "src/main/java/app/model/Unused.java",
                Some("app.model"),
                &["Unused"],
                vec![],
            ),
        ])
    }

    fn resolve(index: &Index, import: &Import) -> Vec<usize> {
        resolve_import(index, &index.files[0], import)
    }

    #[test]
    fn resolves_transitive_closure() {
        let index = fixture();
        let order = closure(
            &index,
            &[0],
            ResolveOptions {
                depth: 2,
                same_package: false,
                minimal: false,
            },
        );
        let paths: Vec<(&str, u32)> = order
            .iter()
            .map(|&(position, depth)| (index.files[position].rel.as_str(), depth))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("src/main/java/app/Service.java", 0),
                ("src/main/java/app/repo/UserRepository.java", 1),
                ("src/main/java/app/model/User.java", 1),
            ]
        );
    }

    #[test]
    fn resolves_same_package_references_without_imports() {
        let mut index = fixture();
        // RunService references Unused in its own package, with no import at all.
        index.files.push(FileEntry {
            rel: "src/main/java/app/model/RunService.java".to_string(),
            abs: PathBuf::from("/repo/src/main/java/app/model/RunService.java"),
            package: Some("app.model".to_string()),
            types: vec!["RunService".to_string()],
            imports: vec![],
            uses: vec!["Unused".to_string(), "String".to_string()],
            symbols: Vec::new(),
            preamble_end: 0,
        });
        let position = index.files.len() - 1;

        let resolved = resolve_uses(&index, &index.files[position]);
        assert_eq!(
            resolved,
            vec![(3, Reason::SamePackage)],
            "Unused is found by name, String is not in the repo"
        );
        assert_eq!(index.files[3].rel, "src/main/java/app/model/Unused.java");
    }

    #[test]
    fn explicit_imports_win_over_name_guessing() {
        let mut index = fixture();
        // `Service` refers to its own type and to two explicitly imported ones.
        index.files[0].uses = vec![
            "Service".to_string(),
            "User".to_string(),
            "UserRepository".to_string(),
        ];
        let resolved = resolve_uses(&index, &index.files[0]);
        // Imported names are left to resolve_import; only the self reference is
        // inferred here (and deduplicated by the closure).
        assert_eq!(resolved, vec![(0, Reason::SamePackage)]);
    }

    #[test]
    fn depth_zero_keeps_seeds_only() {
        let index = fixture();
        let order = closure(
            &index,
            &[0],
            ResolveOptions {
                depth: 0,
                same_package: false,
                minimal: false,
            },
        );
        assert_eq!(order.len(), 1);
    }

    #[test]
    fn static_and_wildcard_imports() {
        let index = fixture();
        assert_eq!(
            resolve(&index, &import("app.model.User.of", ImportKind::Static)),
            vec![2]
        );
        assert_eq!(
            resolve(&index, &import("app.model", ImportKind::Wildcard)).len(),
            2
        );
        assert!(resolve(&index, &import("java.util.List", ImportKind::Type)).is_empty());
    }

    #[test]
    fn member_of_a_package_object_resolves_to_its_file() {
        let index = index_from(vec![entry(
            "src/main/scala/app/util/package.scala",
            Some("app.util"),
            &[],
            vec![],
        )]);
        assert_eq!(
            resolve_import(
                &index,
                &index.files[0],
                &import("app.util.Version", ImportKind::Type)
            ),
            vec![0]
        );
    }

    #[test]
    fn wildcard_can_resolve_to_an_object_path() {
        let index = fixture();
        // Scala's `import app.Service._` names a type, not a package.
        assert_eq!(
            resolve(&index, &import("app.Service", ImportKind::Wildcard)),
            vec![0]
        );
        // A real package still expands to its files.
        assert_eq!(
            resolve(&index, &import("app.model", ImportKind::Wildcard)),
            vec![2, 3]
        );
    }

    #[test]
    fn same_package_option_includes_siblings() {
        let index = fixture();
        let order = closure(
            &index,
            &[2],
            ResolveOptions {
                depth: 1,
                same_package: true,
                minimal: false,
            },
        );
        let rels: Vec<&str> = order
            .iter()
            .map(|&(position, _)| index.files[position].rel.as_str())
            .collect();
        assert!(rels.contains(&"src/main/java/app/model/Unused.java"));
    }

    fn web_fixture() -> Index {
        index_from(vec![
            entry("src/app.ts", None, &[], vec![]),
            entry("src/lib/user.ts", None, &["User"], vec![]),
            entry("src/lib/index.ts", None, &[], vec![]),
            entry("src/components/App.vue", None, &[], vec![]),
            entry("src/components/View.tsx", None, &["View"], vec![]),
            entry("src/legacy.js", None, &[], vec![]),
            entry("src/tool.mjs", None, &[], vec![]),
            entry("src/components/App.ts", None, &[], vec![]),
        ])
    }

    fn resolve_web(index: &Index, from: usize, specifier: &str) -> Vec<usize> {
        resolve_import(
            index,
            &index.files[from],
            &import(specifier, ImportKind::Module),
        )
    }

    #[test]
    fn module_specifiers_probe_extensions_and_index_files() {
        let index = web_fixture();
        assert_eq!(resolve_web(&index, 0, "./lib/user"), vec![1]);
        assert_eq!(resolve_web(&index, 0, "./lib"), vec![2]);
        // Extensionless prefers TS; an explicit `.vue` keeps the component.
        assert_eq!(resolve_web(&index, 0, "./components/App"), vec![7]);
        assert_eq!(resolve_web(&index, 0, "./components/App.vue"), vec![3]);
        assert_eq!(resolve_web(&index, 0, "./components/View"), vec![4]);
        assert_eq!(resolve_web(&index, 0, "./legacy.js"), vec![5]);
        assert_eq!(resolve_web(&index, 0, "./tool"), vec![6]);
        // `./x.js` prefers a TS source when one exists.
        assert_eq!(resolve_web(&index, 0, "./lib/user.js"), vec![1]);
    }

    #[test]
    fn module_specifiers_stop_at_the_root_and_ignore_packages() {
        let index = web_fixture();
        assert!(resolve_web(&index, 0, "react").is_empty());
        assert!(resolve_web(&index, 0, "../outside").is_empty());
        assert!(resolve_web(&index, 0, "./missing").is_empty());
        assert!(resolve_web(&index, 1, "../../../outside").is_empty());
    }

    #[test]
    fn name_resolution_is_isolated_by_family() {
        let index = index_from(vec![
            entry("User.java", None, &["User"], vec![]),
            entry("src/user.ts", None, &["User"], vec![]),
            entry(
                "Service.java",
                None,
                &["Service"],
                vec![import("User", ImportKind::Type)],
            ),
        ]);
        // A default-package Java import must not pick the TypeScript `User`.
        assert_eq!(
            resolve_import(&index, &index.files[2], &index.files[2].imports[0]),
            vec![0]
        );
    }

    #[test]
    fn graph_edges_are_consistent_in_both_directions() {
        let index = java_fixture();
        let seed = member_seed(0, "Service", "used");
        let mut graph = build_graph(
            &index,
            std::slice::from_ref(&seed),
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        assert!(!graph.out.is_empty());
        let inc = graph.reverse().clone();
        for (from, edges) in &graph.out {
            for edge in edges {
                let back = inc
                    .get(&edge.to)
                    .unwrap_or_else(|| panic!("no inc edge for {edge:?}"));
                assert!(
                    back.iter().any(|reverse| reverse.to == *from
                        && reverse.reason == edge.reason
                        && reverse.line == edge.line),
                    "missing reverse edge {from:?} -> {:?}",
                    edge.to
                );
            }
        }
        assert!(!graph.out[&seed].is_empty());
        assert!(graph.out[&seed].iter().all(|edge| edge.line > 0));
    }

    #[test]
    fn graph_collects_diagnostics_without_changing_edges() {
        let mut index = java_fixture();
        index.files[0].symbols[0].members[0].calls = vec![
            crate::lang::Call {
                name: "missing".to_string(),
                receiver: Receiver::Unknown,
                line: 7,
            },
            crate::lang::Call {
                name: "notThere".to_string(),
                receiver: Receiver::Name("repo".to_string()),
                line: 8,
            },
        ];
        let graph = build_graph(
            &index,
            &[member_seed(0, "Service", "used")],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        assert!(graph.diagnostics.iter().any(|diagnostic| diagnostic.kind
            == DiagnosticKind::UnknownReceiver
            && diagnostic.line == 7));
        assert!(graph.diagnostics.iter().any(|diagnostic| diagnostic.kind
            == DiagnosticKind::MethodNotFound
            && diagnostic.line == 8));
        // The known-class/unknown-method fallback edge is still materialized.
        assert!(graph.out[&member_seed(0, "Service", "used")]
            .iter()
            .any(|edge| edge.to
                == Unit::Symbol {
                    file: 2,
                    symbol: "Repo".to_string(),
                }));
    }

    #[test]
    fn graph_reports_type_only_dependencies() {
        let mut index = java_fixture();
        let service_position = 0;
        let unused = java_class("Unused", Vec::new(), &[], &[]);
        index.files[service_position].symbols.push(unused);
        index
            .by_symbol
            .entry("Unused".to_string())
            .or_default()
            .push((service_position, 1));
        index.files[service_position].symbols[0].uses = vec!["Unused".to_string()];

        let graph = build_graph(
            &index,
            &[Unit::Symbol {
                file: service_position,
                symbol: "Service".to_string(),
            }],
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        assert!(
            graph.diagnostics.iter().any(|diagnostic| {
                diagnostic.kind == DiagnosticKind::TypeOnlySkipped && diagnostic.name == "Unused"
            }),
            "{:?}",
            graph.diagnostics
        );
    }

    #[test]
    fn unresolved_imports_are_diagnostics_even_at_depth_zero() {
        let index = index_from(vec![entry(
            "app/Pricing.java",
            Some("app"),
            &["Pricing"],
            vec![import("com.acme.Tax", ImportKind::Type)],
        )]);
        let graph = build_graph(
            &index,
            &[Unit::File(0)],
            ResolveOptions {
                depth: 0,
                same_package: false,
                minimal: false,
            },
        );
        let diagnostic = graph
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.kind == DiagnosticKind::UnresolvedImport)
            .unwrap_or_else(|| panic!("no unresolved import: {:?}", graph.diagnostics));
        assert_eq!(diagnostic.path, "app/Pricing.java");
        assert_eq!(diagnostic.name, "com.acme.Tax");
        assert_eq!(
            diagnostic.to_json(),
            serde_json::json!({
                "kind": "unresolved-import",
                "path": "app/Pricing.java",
                "import": "com.acme.Tax",
            })
        );
        assert_eq!(unresolved_import_count(&index), 1);
    }

    #[test]
    fn every_diagnostic_kind_has_one_documented_wire_field() {
        let cases = [
            (DiagnosticKind::UnresolvedImport, "import"),
            (DiagnosticKind::UnknownReceiver, "call"),
            (DiagnosticKind::UnresolvedCall, "name"),
            (DiagnosticKind::MethodNotFound, "name"),
            (DiagnosticKind::TypeOnlySkipped, "symbol"),
            (DiagnosticKind::Ambiguous, "name"),
        ];
        for (kind, field) in cases {
            let diagnostic = Diagnostic {
                kind,
                path: "app/Service.java".to_string(),
                line: 7,
                name: "thing".to_string(),
            };
            let json = diagnostic.to_json();
            assert_eq!(json["kind"], kind.as_str(), "{json}");
            assert_eq!(json["path"], "app/Service.java", "{json}");
            assert!(
                !json[field].is_null(),
                "{kind:?} must carry {field}: {json}"
            );
            assert!(!diagnostic.to_line().is_empty());
        }
    }

    #[test]
    fn unit_closure_keeps_excerpts_a_file_unit_would_supersede() {
        let index = java_fixture();
        let seed = member_seed(0, "Service", "used");
        let seeds = [Unit::File(0), seed.clone()];
        let graph = build_graph(
            &index,
            &seeds,
            ResolveOptions {
                depth: 1,
                same_package: false,
                minimal: true,
            },
        );
        assert!(graph
            .units(&seeds)
            .contains(&(seed.clone(), 0, Reason::Seed)));
        assert!(!graph.closure(&seeds).contains(&(seed, 0, Reason::Seed)));
    }

    fn callers_fixture(name: &str) -> (std::path::PathBuf, Index) {
        let root = index_from_disk(
            name,
            &[
                (
                    "app/Repo.java",
                    "package app;\npublic class Repo {\n    public void size() {}\n}\n",
                ),
                (
                    "app/Service.java",
                    "package app;\npublic class Service {\n    private Repo repo;\n    public void used() {\n        repo.size();\n    }\n}\n",
                ),
                (
                    "app/Controller.java",
                    "package app;\npublic class Controller {\n    private Service service;\n    public void handle() {\n        service.used();\n    }\n}\n",
                ),
            ],
        );
        let index = crate::index::Index::build(&root);
        (root, index)
    }

    fn member_pos(index: &Index, rel: &str, class: &str, member: &str) -> Unit {
        let position = *index.by_rel.get(rel).unwrap_or_else(|| panic!("{rel}"));
        member_seed(position, class, member)
    }

    #[test]
    fn callers_walk_reverse_call_edges_to_the_requested_depth() {
        let (root, index) = callers_fixture("callers-depth");
        let target = member_pos(&index, "app/Repo.java", "Repo", "size");

        let direct = callers(&index, &target, 1);
        assert_eq!(direct.len(), 1, "{direct:?}");
        assert_eq!(
            direct[0].unit,
            member_pos(&index, "app/Service.java", "Service", "used")
        );
        assert_eq!(direct[0].line, 5);
        assert_eq!(direct[0].depth, 1);

        let transitive = callers(&index, &target, 2);
        assert_eq!(transitive.len(), 2, "{transitive:?}");
        assert_eq!(transitive[0].depth, 1);
        assert_eq!(transitive[1].depth, 2);
        assert_eq!(
            transitive[1].unit,
            member_pos(&index, "app/Controller.java", "Controller", "handle")
        );
        assert_eq!(transitive[1].line, 5);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn why_returns_the_seed_chain_with_call_site_lines() {
        let (root, index) = callers_fixture("callers-why");
        let service = member_pos(&index, "app/Service.java", "Service", "used");
        let controller = member_pos(&index, "app/Controller.java", "Controller", "handle");
        let target = member_pos(&index, "app/Repo.java", "Repo", "size");

        let chain = why(&index, std::slice::from_ref(&controller), &target, 2).unwrap();
        assert_eq!(chain.len(), 3, "{chain:?}");
        assert_eq!(chain[0].unit, controller);
        assert_eq!(chain[0].reason, Reason::Seed);
        assert_eq!(chain[0].line, 0);
        assert_eq!(chain[1].unit, service);
        assert_eq!(chain[1].reason, Reason::Call);
        assert_eq!(chain[1].line, 5);
        assert_eq!(chain[2].unit, target);
        assert_eq!(chain[2].reason, Reason::Call);
        assert_eq!(chain[2].line, 5);

        // The seed itself needs no walk.
        let seed_chain = why(&index, std::slice::from_ref(&service), &service, 2).unwrap();
        assert_eq!(seed_chain.len(), 1);
        assert_eq!(seed_chain[0].reason, Reason::Seed);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refs_invert_recorded_uses_and_calls() {
        let (root, mut index) = callers_fixture("refs");

        let calls = references(&index, "size");
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].symbol, "Service");
        assert_eq!(calls[0].member.as_deref(), Some("used"));
        assert_eq!(calls[0].line, 5);
        assert_eq!(calls[0].kind, RefKind::Call);

        let indirect = references(&index, "used");
        assert!(indirect.iter().any(|reference| {
            reference.symbol == "Controller"
                && reference.member.as_deref() == Some("handle")
                && reference.line == 5
                && reference.kind == RefKind::Call
        }));

        // `uses` is recorded per symbol, so the reference line is the enclosing
        // declaration; this is the `refs TaxRate` contract for type uses.
        let service = *index.by_rel.get("app/Service.java").unwrap();
        index.files[service].symbols[0]
            .uses
            .push("TaxRate".to_string());
        let uses = references(&index, "TaxRate");
        assert_eq!(uses.len(), 1, "{uses:?}");
        assert_eq!(uses[0].symbol, "Service");
        assert_eq!(uses[0].member, None);
        assert_eq!(uses[0].line, index.files[service].symbols[0].line as u32);
        assert_eq!(uses[0].kind, RefKind::Use);

        std::fs::remove_dir_all(root).unwrap();
    }

    /// Ignored benchmark: `cargo test bench_reverse -- --ignored --nocapture`.
    /// Reported in #8: ~0.9 ms for 1800 edges, which is why `inc` is lazy.
    #[test]
    #[ignore]
    fn bench_reverse_cost() {
        let mut classes = String::from("package p;\n");
        for i in 0..300 {
            classes.push_str(&format!("class C{i} {{\n"));
            for m in 0..5 {
                let target = (i + 1) % 300;
                classes.push_str(&format!("  void m{m}() {{ new C{target}().m0(); }}\n"));
            }
            classes.push_str("}\n");
        }
        let root = index_from_disk("graph-bench", &[("C.java", &classes)]);
        let index = crate::index::Index::build(&root);
        let seed = Unit::File(*index.by_rel.get("C.java").expect("C.java"));
        let start = std::time::Instant::now();
        let mut graph = build_graph(
            &index,
            &[seed],
            ResolveOptions {
                depth: 2,
                same_package: false,
                minimal: true,
            },
        );
        let build = start.elapsed();
        let edges: usize = graph.out.values().map(Vec::len).sum();
        let start = std::time::Instant::now();
        let inc = graph.reverse().clone();
        let reverse = start.elapsed();
        println!(
            "build {build:?}, edges {edges}, reverse {reverse:?}, inc nodes {}",
            inc.len()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
