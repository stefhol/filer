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

    for name in symbol.calls.iter().chain(symbol.uses.iter()) {
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

/// Neighbours of one method excerpt: the class's own type dependencies and
/// imports, plus the methods this member calls.
fn member_neighbours(
    index: &Index,
    file_position: usize,
    class_name: &str,
    member_name: &str,
    options: ResolveOptions,
) -> Vec<(Unit, Reason)> {
    let Some(file) = index.files.get(file_position) else {
        return Vec::new();
    };
    let Some(class) = file.symbol(class_name) else {
        return Vec::new();
    };
    let Some(member) = class
        .members
        .iter()
        .find(|member| member.kind == MemberKind::Method && member.name == member_name)
    else {
        return Vec::new();
    };
    let family = family_of(file);
    let _ = options;

    // Member bundles are call-path driven: the class shell already carries the
    // package, imports, fields and signatures, so only methods that are
    // actually called pull in other files.
    let mut neighbours = Vec::new();
    for call in &member.calls {
        for unit in resolve_call(index, file_position, file, class, member, call, family) {
            neighbours.push((unit, Reason::Call));
        }
    }
    neighbours
}

fn resolve_call(
    index: &Index,
    file_position: usize,
    file: &FileEntry,
    class: &Symbol,
    member: &Member,
    call: &crate::lang::Call,
    family: Option<Family>,
) -> Vec<Unit> {
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
            found
        }
        Receiver::Super => super_methods(index, file_position, class, &call.name, family),
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
                return find_methods(
                    index,
                    file_position,
                    class,
                    &call.name,
                    family,
                    &mut visited,
                );
            }
            let Some((target_file, target_index)) = resolve_class(index, file, &type_name, family)
            else {
                return Vec::new();
            };
            let target = &index.files[target_file].symbols[target_index];
            let mut visited = HashSet::new();
            let methods =
                find_methods(index, target_file, target, &call.name, family, &mut visited);
            if methods.is_empty() {
                // We know the class but not the member: keep the whole class
                // rather than silently dropping a method.
                vec![Unit::Symbol {
                    file: target_file,
                    symbol: target.name.clone(),
                }]
            } else {
                methods
            }
        }
        Receiver::Unknown => Vec::new(),
    }
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

/// Neighbours of one unit, honouring [`ResolveOptions::minimal`]: Java file and
/// class seeds are call-path driven instead of import driven.
fn unit_neighbours(index: &Index, unit: &Unit, options: ResolveOptions) -> Vec<(Unit, Reason)> {
    match unit {
        Unit::File(position) if options.minimal => call_graph_seed(index, *position)
            .unwrap_or_else(|| file_neighbours(index, *position, options)),
        Unit::Symbol { file, symbol } if options.minimal => call_graph_class(index, *file, symbol)
            .unwrap_or_else(|| symbol_neighbours(index, *file, symbol, options)),
        Unit::File(position) => file_neighbours(index, *position, options),
        Unit::Symbol { file, symbol } => symbol_neighbours(index, *file, symbol, options),
        Unit::Member {
            file,
            class,
            member,
        } => member_neighbours(index, *file, class, member, options),
    }
}

/// Calls of every method in every class of a file, for a whole-file seed.
/// `None` keeps the old import/type closure for files that are not
/// member-capable or have no symbols.
fn call_graph_seed(index: &Index, position: usize) -> Option<Vec<(Unit, Reason)>> {
    let file = index.files.get(position)?;
    if file.language()?.granularity() != Granularity::Member || file.symbols.is_empty() {
        return None;
    }
    let family = family_of(file);
    let mut out = Vec::new();
    for class in &file.symbols {
        out.extend(class_calls(index, position, file, class, family));
    }
    Some(out)
}

/// Calls of every method of one class, for a class seed (`path::Class`).
fn call_graph_class(
    index: &Index,
    position: usize,
    class_name: &str,
) -> Option<Vec<(Unit, Reason)>> {
    let file = index.files.get(position)?;
    if file.language()?.granularity() != Granularity::Member {
        return None;
    }
    let class = file.symbol(class_name)?;
    Some(class_calls(index, position, file, class, family_of(file)))
}

fn class_calls(
    index: &Index,
    position: usize,
    file: &FileEntry,
    class: &Symbol,
    family: Option<Family>,
) -> Vec<(Unit, Reason)> {
    let mut out = Vec::new();
    for member in class
        .members
        .iter()
        .filter(|member| member.kind == MemberKind::Method)
    {
        for call in &member.calls {
            for unit in resolve_call(index, position, file, class, member, call, family) {
                out.push((unit, Reason::Call));
            }
        }
    }
    out
}

/// Breadth-first closure over units, keeping track of why each one was pulled
/// in. Returns units in discovery order, seeds first (depth 0), duplicates
/// removed. A whole-file unit supersedes symbol units of the same file.
pub fn closure_units(
    index: &Index,
    seeds: &[Unit],
    options: ResolveOptions,
) -> Vec<(Unit, u32, Reason)> {
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
        let current_depth = depth_of[&current];
        if current_depth >= options.depth {
            continue;
        }
        let neighbours = unit_neighbours(index, &current, options);

        for (neighbour, reason) in neighbours {
            if depth_of.contains_key(&neighbour) {
                continue;
            }
            let depth = current_depth + 1;
            depth_of.insert(neighbour.clone(), depth);
            order.push((neighbour.clone(), depth, reason));
            queue.push_back(neighbour);
        }
    }

    drop_covered_symbols(order)
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
            uses: uses.iter().map(|name| name.to_string()).collect(),
            calls: calls.iter().map(|name| name.to_string()).collect(),
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
}
