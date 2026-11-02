//! `filer` — bundle source files and their import closure for a chat LLM.

mod bundle;
mod csharp;
mod fe;
mod groovy;
mod index;
mod java;
mod kotlin;
mod lang;
mod resolve;
mod rust;
mod scala;
mod state;
mod web;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use bundle::{BundleFile, BundleOptions};
use index::{FileEntry, Index};
use lang::{Granularity, Language, MemberKind, SymbolKind};
use resolve::{closure_units, drop_covered_symbols, Reason, ResolveOptions, Unit};
use state::{BranchBundle, Selection, StateDoc, StateFile};

#[derive(Parser)]
#[command(
    name = "filer",
    version,
    about = "Bundle sources and their import closure into chat-ready markdown"
)]
struct Cli {
    /// Project root. Defaults to the current directory.
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List source types as JSON lines (used by the Neovim picker).
    Search(SearchArgs),
    /// Emit a markdown bundle for the given files plus their import closure.
    Bundle(BundleArgs),
    /// Print the import closure of a single file.
    Deps(DepsArgs),
    /// Re-render the saved bundle from the state file.
    Show(ShowArgs),
    /// Build the index cache and print stats.
    Index,
    /// Show or clear the saved bundle state.
    State(StateArgs),
}

#[derive(Args)]
struct SearchArgs {
    /// Query matched against type names and paths (case-insensitive).
    #[arg(long, default_value = "", value_name = "TEXT")]
    query: String,
    /// Maximum number of results.
    #[arg(long, default_value_t = 200)]
    limit: usize,
    /// Human-readable output instead of JSON lines.
    #[arg(long)]
    plain: bool,
    /// Mark files already in the bundle at this state file.
    #[arg(long, value_name = "FILE")]
    state: Option<PathBuf>,
    /// Branch whose bundle is marked. Defaults to the current branch.
    #[arg(long, value_name = "BRANCH")]
    branch: Option<String>,
}

#[derive(Args)]
struct BundleArgs {
    /// Files to bundle, as paths relative to the root (or absolute paths).
    #[arg(required = true, value_name = "FILE")]
    files: Vec<String>,
    /// Import hops to follow. 0 bundles only the given files.
    #[arg(long, default_value_t = 2)]
    depth: u32,
    /// Also include files that share a package with a bundled file.
    #[arg(long)]
    with_same_package: bool,
    /// Also include build files (pom.xml, build.gradle, build.sbt, Cargo.toml, *.csproj, ...).
    #[arg(long)]
    with_config: bool,
    /// Bundle title shown in the header.
    #[arg(long)]
    title: Option<String>,
    /// Write the bundle to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Carry the previous bundle over from this state file and update it.
    #[arg(long, value_name = "FILE")]
    state: Option<PathBuf>,
    /// Start from an empty bundle instead of the saved state.
    #[arg(long)]
    reset: bool,
    /// Bundle the symbol under this 1-based line instead of the whole file
    /// (used by `:FilerHere` inside a `.fe` file).
    #[arg(long, value_name = "LINE")]
    at_line: Option<usize>,
}

#[derive(Args)]
struct DepsArgs {
    /// File to inspect, relative to the root (or absolute).
    #[arg(value_name = "FILE")]
    file: String,
    /// Import hops to follow.
    #[arg(long, default_value_t = 2)]
    depth: u32,
    /// JSON output.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ShowArgs {
    /// State file. Defaults to <root>/.filer/bundle.toml.
    #[arg(long, value_name = "FILE")]
    state: Option<PathBuf>,
    /// Branch to show. Defaults to the current branch.
    #[arg(long, value_name = "BRANCH")]
    branch: Option<String>,
    /// Bundle title shown in the header.
    #[arg(long)]
    title: Option<String>,
    /// Write the bundle to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct StateArgs {
    /// Delete the bundle for one branch (see --branch).
    #[arg(long)]
    clear: bool,
    /// Delete every branch's bundle.
    #[arg(long)]
    all: bool,
    /// Branch to clear or list. Defaults to the current branch.
    #[arg(long, value_name = "BRANCH")]
    branch: Option<String>,
    /// State file. Defaults to <root>/.filer/bundle.toml.
    #[arg(long, value_name = "FILE")]
    file: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();
    let root = cli
        .root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match cli.command {
        Command::Search(args) => search(&root, args),
        Command::Bundle(args) => bundle_cmd(&root, args),
        Command::Deps(args) => deps(&root, args),
        Command::Show(args) => show_cmd(&root, args),
        Command::Index => index_cmd(&root),
        Command::State(args) => state_cmd(&root, args),
    }
}

struct SearchHit {
    score: u8,
    file: usize,
    symbol: Option<usize>,
    member: Option<usize>,
}

/// One file's entries in the saved bundle, for marking picker rows as included.
#[derive(Default)]
struct Included {
    file: Option<(u32, Reason)>,
    symbols: HashMap<String, (u32, Reason)>,
    methods: HashMap<(String, String), (u32, Reason)>,
}

impl Included {
    /// A file row is included only by a whole-file entry.
    fn whole(&self) -> Option<(u32, Reason)> {
        self.file
    }

    /// A class row is covered by a whole-file or a class entry.
    fn symbol(&self, symbol: &str) -> Option<(u32, Reason)> {
        self.file.or_else(|| self.symbols.get(symbol).copied())
    }

    /// A method row is covered by its own entry, its class, or the whole file.
    fn method(&self, symbol: &str, member: &str) -> Option<(u32, Reason)> {
        self.methods
            .get(&(symbol.to_string(), member.to_string()))
            .copied()
            .or_else(|| self.symbol(symbol))
    }
}

fn included_from(bundle: Option<BranchBundle>) -> HashMap<String, Included> {
    let mut included: HashMap<String, Included> = HashMap::new();
    let Some(bundle) = bundle else {
        return included;
    };
    for file in bundle.files {
        let entry = included.entry(file.path.clone()).or_default();
        let value = (file.depth, Reason::from_str(&file.reason));
        match file.scope {
            Selection::File => entry.file = Some(value),
            Selection::Symbol => {
                if let Some(symbol) = file.symbol {
                    entry.symbols.insert(symbol, value);
                }
            }
            Selection::Methods => {
                if let Some(symbol) = file.symbol {
                    for method in file.methods {
                        entry.methods.insert((symbol.clone(), method), value);
                    }
                }
            }
        }
    }
    included
}

/// Bundling granularity of an indexed file; unknown files are whole files.
fn granularity_of(file: &FileEntry) -> Granularity {
    file.language()
        .map(Language::granularity)
        .unwrap_or(Granularity::File)
}

fn search(root: &Path, args: SearchArgs) {
    let index = Index::build(root);
    let branch = args
        .branch
        .clone()
        .unwrap_or_else(|| state::current_branch(&index.root));

    let included = included_from(
        args.state
            .as_ref()
            .and_then(|path| state::load(path))
            .and_then(|doc| doc.bundle(&branch).cloned()),
    );

    let query = args.query.trim().to_lowercase();
    let tokens: Vec<&str> = query.split_whitespace().collect();

    let mut hits: Vec<SearchHit> = Vec::new();
    for (position, file) in index.files.iter().enumerate() {
        let granularity = granularity_of(file);
        if granularity.file_row() {
            if let Some(score) = score(&file.primary_fqn(), &file.rel, &file.types, &query, &tokens)
            {
                hits.push(SearchHit {
                    score,
                    file: position,
                    symbol: None,
                    member: None,
                });
            }
        }
        if !granularity.symbol_rows() {
            continue;
        }
        for (symbol_index, symbol) in file.symbols.iter().enumerate() {
            // Companion objects repeat the class name; one picker row is enough.
            if file.symbols[..symbol_index]
                .iter()
                .any(|earlier| earlier.name == symbol.name)
            {
                continue;
            }
            let class_fqn = class_fqn(file, symbol);
            if let Some(score) = score(
                &class_fqn,
                &file.rel,
                std::slice::from_ref(&symbol.name),
                &query,
                &tokens,
            ) {
                hits.push(SearchHit {
                    score,
                    file: position,
                    symbol: Some(symbol_index),
                    member: None,
                });
            }
            if granularity != Granularity::Member {
                continue;
            }
            for (member_index, member) in symbol.members.iter().enumerate() {
                if member.kind != MemberKind::Method {
                    continue;
                }
                let fqn = format!("{class_fqn}.{}", member.name);
                if let Some(score) = score(
                    &fqn,
                    &file.rel,
                    std::slice::from_ref(&member.name),
                    &query,
                    &tokens,
                ) {
                    hits.push(SearchHit {
                        score,
                        file: position,
                        symbol: Some(symbol_index),
                        member: Some(member_index),
                    });
                }
            }
        }
    }
    hits.sort_by(|a, b| {
        a.score
            .cmp(&b.score)
            .then_with(|| index.files[a.file].rel.cmp(&index.files[b.file].rel))
            .then_with(|| scope_rank(a).cmp(&scope_rank(b)))
            .then_with(|| hit_label(&index, a).cmp(&hit_label(&index, b)))
    });

    for hit in hits.into_iter().take(args.limit) {
        let file = &index.files[hit.file];
        let granularity = granularity_of(file);
        let entry = included.get(&file.rel);
        let Some(symbol_index) = hit.symbol else {
            let bundled = entry.and_then(Included::whole);
            let value = serde_json::json!({
                "path": file.rel,
                "scope": Selection::File.as_str(),
                "symbol": serde_json::Value::Null,
                "member": serde_json::Value::Null,
                "kind": "file",
                "fqn": file.rel,
                "line": serde_json::Value::Null,
                "package": file.package,
                "types": file.types,
                "included": bundled.is_some(),
                "depth": bundled.map(|(depth, _)| depth),
                "reason": bundled.map(|(_, reason)| reason.as_str()),
            });
            print_row(
                args.plain,
                bundled.is_some(),
                "file",
                &file.rel,
                &file.rel,
                value,
            );
            continue;
        };

        let symbol = &file.symbols[symbol_index];
        let class_fqn = class_fqn(file, symbol);
        let Some(member) = hit.member.map(|member| &symbol.members[member]) else {
            let kind = row_kind(granularity, symbol.kind);
            let bundled = entry.and_then(|entry| entry.symbol(&symbol.name));
            let value = serde_json::json!({
                "path": file.rel,
                "scope": Selection::Symbol.as_str(),
                "symbol": symbol.name,
                "member": serde_json::Value::Null,
                "kind": kind,
                "fqn": class_fqn,
                "line": symbol.line,
                "package": file.package,
                "types": file.types,
                "included": bundled.is_some(),
                "depth": bundled.map(|(depth, _)| depth),
                "reason": bundled.map(|(_, reason)| reason.as_str()),
            });
            let target = format!("{}::{}", file.rel, symbol.name);
            print_row(
                args.plain,
                bundled.is_some(),
                kind,
                &class_fqn,
                &target,
                value,
            );
            continue;
        };

        let bundled = entry.and_then(|entry| entry.method(&symbol.name, &member.name));
        let fqn = format!("{class_fqn}.{}", member.name);
        let value = serde_json::json!({
            "path": file.rel,
            "scope": Selection::Methods.as_str(),
            "symbol": symbol.name,
            "member": member.name,
            "kind": "method",
            "fqn": fqn,
            "line": member.line,
            "package": file.package,
            "types": file.types,
            "included": bundled.is_some(),
            "depth": bundled.map(|(depth, _)| depth),
            "reason": bundled.map(|(_, reason)| reason.as_str()),
        });
        let target = format!("{}::{}.{}", file.rel, symbol.name, member.name);
        print_row(
            args.plain,
            bundled.is_some(),
            "method",
            &fqn,
            &target,
            value,
        );
    }
}

/// One picker row: JSON for Telescope, or `marker<TAB>[kind] label<TAB>target`.
fn print_row(
    plain: bool,
    included: bool,
    kind: &str,
    label: &str,
    target: &str,
    value: serde_json::Value,
) {
    if plain {
        let marker = if included { "*" } else { " " };
        println!("{marker}\t[{kind}] {label}\t{target}");
    } else {
        println!("{value}");
    }
}

fn class_fqn(file: &FileEntry, symbol: &crate::lang::Symbol) -> String {
    match (&file.package, symbol.kind) {
        (Some(package), SymbolKind::Type) => format!("{package}.{}", symbol.name),
        _ => symbol.name.clone(),
    }
}

/// Picker row kind, which is also the `[kind]` label prefix.
fn row_kind(granularity: Granularity, kind: SymbolKind) -> &'static str {
    match (granularity, kind) {
        (Granularity::Member, _) => "class",
        (Granularity::Function, _) => "function",
        (_, SymbolKind::Type) => "type",
        (_, SymbolKind::Function) => "function",
        (_, SymbolKind::Declaration) => "declaration",
    }
}

/// `[file]` before `[class]` before `[method]` for the same file and score.
fn scope_rank(hit: &SearchHit) -> u8 {
    match (hit.symbol, hit.member) {
        (None, _) => 0,
        (Some(_), None) => 1,
        (Some(_), Some(_)) => 2,
    }
}

fn hit_label(index: &Index, hit: &SearchHit) -> String {
    let Some(symbol_index) = hit.symbol else {
        return index.files[hit.file].rel.clone();
    };
    let symbol = &index.files[hit.file].symbols[symbol_index];
    match hit.member {
        Some(member) => format!("{}.{}", symbol.name, symbol.members[member].name),
        None => symbol.name.clone(),
    }
}

fn score(fqn: &str, rel: &str, names: &[String], query: &str, tokens: &[&str]) -> Option<u8> {
    let fqn = fqn.to_lowercase();
    let rel = rel.to_lowercase();

    if !tokens.is_empty() {
        let haystack = format!("{fqn} {rel}");
        if !tokens.iter().all(|token| haystack.contains(token)) {
            return None;
        }
    }

    let names: Vec<String> = names.iter().map(|name| name.to_lowercase()).collect();
    Some(if query.is_empty() {
        4
    } else if names.iter().any(|name| name == query) {
        0
    } else if names.iter().any(|name| name.starts_with(query)) {
        1
    } else if fqn.contains(query) {
        2
    } else if rel.contains(query) {
        3
    } else {
        4
    })
}

fn bundle_cmd(root: &Path, args: BundleArgs) {
    let index = Index::build(root);
    let root = index.root.clone();
    let state_path = args.state.clone();

    let branch = state::current_branch(&root);
    let doc = state_path.as_ref().and_then(|path| state::load(path));

    // Units carried over from this branch's previous bundle, validated against
    // the current index. One `methods` entry expands to one unit per method.
    let mut previous: HashMap<Unit, (u32, Reason)> = HashMap::new();
    if !args.reset {
        if let Some(bundle) = doc.as_ref().and_then(|doc| doc.bundle(&branch)) {
            for file in &bundle.files {
                let Some(&position) = index.by_rel.get(&file.path) else {
                    continue;
                };
                let entry = &index.files[position];
                let value = (file.depth, Reason::from_str(&file.reason));
                match file.scope {
                    Selection::File => {
                        previous.insert(Unit::File(position), value);
                    }
                    Selection::Symbol => {
                        let Some(symbol) = &file.symbol else {
                            continue;
                        };
                        if entry.symbol(symbol).is_some() {
                            previous.insert(
                                Unit::Symbol {
                                    file: position,
                                    symbol: symbol.clone(),
                                },
                                value,
                            );
                        } else {
                            eprintln!(
                                "filer: dropping {}::{symbol}: no longer in the file",
                                file.path
                            );
                        }
                    }
                    Selection::Methods => {
                        let Some(symbol) = &file.symbol else {
                            continue;
                        };
                        let Some(class) = entry.symbol(symbol) else {
                            eprintln!(
                                "filer: dropping {}::{symbol}: no longer in the file",
                                file.path
                            );
                            continue;
                        };
                        for member in &file.methods {
                            let exists = class.members.iter().any(|candidate| {
                                candidate.kind == MemberKind::Method && candidate.name == *member
                            });
                            if exists {
                                previous.insert(
                                    Unit::Member {
                                        file: position,
                                        class: symbol.clone(),
                                        member: member.clone(),
                                    },
                                    value,
                                );
                            } else {
                                eprintln!(
                                    "filer: dropping {}::{symbol}.{member}: no longer in the file",
                                    file.path
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    let mut seeds: Vec<Unit> = Vec::new();
    let mut extras: Vec<BundleFile> = Vec::new();
    let mut at_line_used = false;

    for input in &args.files {
        let (path, symbol) = split_symbol(input);
        let Some(rel) = locate(&index, &root, path) else {
            eprintln!("filer: skipping {path}: not inside {}", root.display());
            continue;
        };
        let Some(&position) = index.by_rel.get(&rel) else {
            extras.push(BundleFile::whole(
                rel.clone(),
                root.join(&rel),
                0,
                Reason::External,
            ));
            continue;
        };

        let unit = match symbol {
            Some(symbol) => seed_unit(&index, position, symbol).unwrap_or_else(|| {
                eprintln!(
                    "filer: {} has no symbol {symbol}; bundling the whole file",
                    index.files[position].rel
                );
                Unit::File(position)
            }),
            None if !at_line_used && args.at_line.is_some() => {
                at_line_used = true;
                let line = args.at_line.expect("checked");
                seed_at_line(&index, &root, position, line).unwrap_or(Unit::File(position))
            }
            None => Unit::File(position),
        };
        if !seeds.contains(&unit) {
            seeds.push(unit);
        }
    }

    let resolved = closure_units(
        &index,
        &seeds,
        ResolveOptions {
            depth: args.depth,
            same_package: args.with_same_package,
            minimal: true,
        },
    );
    let fresh = resolved.len();

    let mut merged = previous;
    for (unit, depth, reason) in resolved {
        match merged.get(&unit).copied() {
            Some((existing_depth, existing_reason)) => {
                // Minimum depth wins; on a tie the seed's reason wins.
                let replace = depth < existing_depth
                    || (depth == existing_depth
                        && reason == Reason::Seed
                        && existing_reason != Reason::Seed);
                if replace {
                    merged.insert(unit, (depth, reason));
                }
            }
            None => {
                merged.insert(unit, (depth, reason));
            }
        }
    }

    let mut units: Vec<(Unit, u32, Reason)> = merged
        .into_iter()
        .map(|(unit, (depth, reason))| (unit, depth, reason))
        .collect();
    units = drop_covered_symbols(units);
    units.sort_by(|(unit_a, depth_a, _), (unit_b, depth_b, _)| {
        depth_a
            .cmp(depth_b)
            .then_with(|| {
                index.files[unit_a.file()]
                    .rel
                    .cmp(&index.files[unit_b.file()].rel)
            })
            .then_with(|| {
                unit_a
                    .symbol()
                    .unwrap_or("")
                    .cmp(unit_b.symbol().unwrap_or(""))
            })
            .then_with(|| {
                unit_a
                    .member()
                    .unwrap_or("")
                    .cmp(unit_b.member().unwrap_or(""))
            })
    });

    // Group the method units of one class into a single `methods` entry.
    let mut files: Vec<BundleFile> = Vec::new();
    let mut class_slot: HashMap<(String, String), usize> = HashMap::new();
    for (unit, depth, reason) in units {
        let entry = &index.files[unit.file()];
        match unit {
            Unit::File(_) => files.push(BundleFile::whole(
                entry.rel.clone(),
                entry.abs.clone(),
                depth,
                reason,
            )),
            Unit::Symbol { symbol, .. } => files.push(BundleFile::excerpt(
                entry.rel.clone(),
                entry.abs.clone(),
                depth,
                reason,
                symbol,
            )),
            Unit::Member { class, member, .. } => {
                let key = (entry.rel.clone(), class.clone());
                match class_slot.get(&key).copied() {
                    Some(position) => {
                        let file = &mut files[position];
                        file.methods.insert(member);
                        if depth < file.depth {
                            file.depth = depth;
                            file.reason = reason;
                        } else if depth == file.depth && reason == Reason::Seed {
                            file.reason = reason;
                        }
                    }
                    None => {
                        class_slot.insert(key, files.len());
                        files.push(BundleFile::members(
                            entry.rel.clone(),
                            entry.abs.clone(),
                            depth,
                            reason,
                            class,
                            BTreeSet::from([member]),
                        ));
                    }
                }
            }
        }
    }

    if args.with_config {
        files.extend(config_files(&root));
    }
    files.extend(extras);

    if files.is_empty() {
        eprintln!("filer: nothing to bundle");
        std::process::exit(1);
    }

    let branch_bundle = if state_path.is_some() {
        Some(BranchBundle {
            name: branch.clone(),
            created: state::now(),
            depth: args.depth,
            files: files
                .iter()
                .map(|file| StateFile {
                    path: file.rel.clone(),
                    scope: file.scope,
                    symbol: file.symbol.clone(),
                    methods: file.methods.iter().cloned().collect(),
                    depth: file.depth,
                    reason: file.reason.as_str().to_string(),
                })
                .collect(),
        })
    } else {
        None
    };

    // The appendix of a bundle carries only the branch it was built for.
    let appendix = branch_bundle.as_ref().map(|bundle| {
        let mut shown = StateDoc::new(&root);
        shown.upsert(bundle.clone());
        state::to_toml_plain(&shown)
    });

    let bundle_options = BundleOptions {
        title: args.title,
        depth: args.depth,
        appendix,
        ..Default::default()
    };
    let markdown = bundle::render(&index, &files, &bundle_options);

    if let (Some(path), Some(bundle)) = (&state_path, branch_bundle) {
        let mut doc = doc.unwrap_or_else(|| StateDoc::new(&root));
        doc.upsert(bundle);
        if let Err(error) = state::save(path, &doc) {
            eprintln!("filer: cannot write state {}: {error}", path.display());
        }
    }

    match args.out {
        Some(path) => {
            if let Err(error) = std::fs::write(&path, &markdown) {
                eprintln!("filer: cannot write {}: {error}", path.display());
                std::process::exit(1);
            }
            eprintln!(
                "filer: wrote {} ({} units, {} new)",
                path.display(),
                files.len(),
                fresh
            );
        }
        None => {
            print!("{markdown}");
            eprintln!("filer: {} units, {} new on this run", files.len(), fresh);
        }
    }
}

/// `path::symbol` (Windows drive letters do not contain `::`).
fn split_symbol(input: &str) -> (&str, Option<&str>) {
    match input.rsplit_once("::") {
        Some((path, symbol)) if !path.is_empty() && !symbol.is_empty() => (path, Some(symbol)),
        _ => (input, None),
    }
}

/// `Class` -> whole class, `Class.method` -> pruned class, `function` -> FE.
/// Method scopes are only accepted from member-capable languages.
fn seed_unit(index: &Index, position: usize, symbol: &str) -> Option<Unit> {
    let file = index.files.get(position)?;
    if granularity_of(file) == Granularity::Member {
        if let Some((class, member)) = symbol.rsplit_once('.') {
            if let Some(class_symbol) = file.symbol(class) {
                if class_symbol.members.iter().any(|candidate| {
                    candidate.kind == MemberKind::Method && candidate.name == member
                }) {
                    return Some(Unit::Member {
                        file: position,
                        class: class.to_string(),
                        member: member.to_string(),
                    });
                }
            }
        }
    }
    file.symbol(symbol).map(|_| Unit::Symbol {
        file: position,
        symbol: symbol.to_string(),
    })
}

fn seed_at_line(index: &Index, root: &Path, position: usize, line: usize) -> Option<Unit> {
    let file = index.files.get(position)?;
    let source = std::fs::read_to_string(root.join(&file.rel)).ok()?;
    let offset = offset_of_line(&source, line);
    if let Some((class, member)) = file.member_at(offset) {
        if member.kind == MemberKind::Method {
            return Some(Unit::Member {
                file: position,
                class: class.name.clone(),
                member: member.name.clone(),
            });
        }
    }
    file.symbol_at(offset).map(|symbol| Unit::Symbol {
        file: position,
        symbol: symbol.name.clone(),
    })
}

fn offset_of_line(source: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    let mut current = 1;
    for (offset, byte) in source.bytes().enumerate() {
        if current == line {
            return offset;
        }
        if byte == b'\n' {
            current += 1;
        }
    }
    source.len()
}

fn deps(root: &Path, args: DepsArgs) {
    let index = Index::build(root);
    let root = index.root.clone();
    let Some(rel) = locate(&index, &root, &args.file) else {
        eprintln!("filer: {} is not inside {}", args.file, root.display());
        std::process::exit(1);
    };
    let Some(&position) = index.by_rel.get(&rel) else {
        eprintln!("filer: {rel} is not indexed (binary or skipped directory?)");
        std::process::exit(1);
    };

    let order = resolve::closure_detailed(
        &index,
        &[position],
        ResolveOptions {
            depth: args.depth,
            same_package: false,
            minimal: false,
        },
    );

    if args.json {
        let value: Vec<_> = order
            .iter()
            .map(|&(position, depth, reason)| {
                let file = &index.files[position];
                serde_json::json!({
                    "path": file.rel,
                    "fqn": file.primary_fqn(),
                    "depth": depth,
                    "reason": reason.as_str(),
                })
            })
            .collect();
        println!("{}", serde_json::json!(value));
        return;
    }

    for &(position, depth, reason) in &order {
        let suffix = match (depth, reason) {
            (0, _) => String::new(),
            (_, reason) => format!(" ({})", reason.as_str()),
        };
        println!(
            "{}{}{}",
            "  ".repeat(depth as usize),
            index.files[position].rel,
            suffix
        );
    }

    for import in resolve::unresolved_imports(&index, position) {
        eprintln!("unresolved (probably JDK/third-party): {import}");
    }
}

/// Re-renders one branch's bundle from the state file: same files, same depths
/// and reasons, current file contents.
fn show_cmd(root: &Path, args: ShowArgs) {
    let index = Index::build(root);
    let root = index.root.clone();
    let path = args
        .state
        .clone()
        .unwrap_or_else(|| state::default_path(&root));

    let Some(doc) = state::load(&path) else {
        eprintln!(
            "filer: no bundle state at {} — bundle something first",
            path.display()
        );
        std::process::exit(1);
    };

    let branch = args
        .branch
        .clone()
        .unwrap_or_else(|| state::current_branch(&root));

    let Some(bundle) = doc.bundle(&branch).cloned() else {
        let available: Vec<String> = doc
            .bundles
            .iter()
            .map(|bundle| {
                format!(
                    "{} ({})",
                    state::branch_label(&bundle.name),
                    bundle.files.len()
                )
            })
            .collect();
        eprintln!(
            "filer: no bundle for branch {} (have: {})",
            state::branch_label(&branch),
            if available.is_empty() {
                "none".to_string()
            } else {
                available.join(", ")
            }
        );
        std::process::exit(1);
    };

    let mut files: Vec<BundleFile> = Vec::new();
    let mut shown = bundle.clone();
    shown.files.clear();

    for file in &bundle.files {
        let abs = root.join(&file.path);
        let reason = Reason::from_str(&file.reason);
        match index.by_rel.get(&file.path) {
            Some(&position) => {
                let entry = &index.files[position];
                let mut shown_file = file.clone();
                let built = match file.scope {
                    Selection::File => Some(BundleFile::whole(
                        entry.rel.clone(),
                        entry.abs.clone(),
                        file.depth,
                        reason,
                    )),
                    Selection::Symbol => {
                        let Some(symbol) = &file.symbol else {
                            continue;
                        };
                        if entry.symbol(symbol).is_none() {
                            eprintln!(
                                "filer: dropping {}::{symbol}: no longer in the file",
                                file.path
                            );
                            continue;
                        }
                        Some(BundleFile::excerpt(
                            entry.rel.clone(),
                            entry.abs.clone(),
                            file.depth,
                            reason,
                            symbol.clone(),
                        ))
                    }
                    Selection::Methods => {
                        let Some(symbol) = &file.symbol else {
                            continue;
                        };
                        let Some(class) = entry.symbol(symbol) else {
                            eprintln!(
                                "filer: dropping {}::{symbol}: no longer in the file",
                                file.path
                            );
                            continue;
                        };
                        let mut kept = BTreeSet::new();
                        for member in &file.methods {
                            let exists = class.members.iter().any(|candidate| {
                                candidate.kind == MemberKind::Method && candidate.name == *member
                            });
                            if exists {
                                kept.insert(member.clone());
                            } else {
                                eprintln!(
                                    "filer: dropping {}::{symbol}.{member}: no longer in the file",
                                    file.path
                                );
                            }
                        }
                        if kept.is_empty() {
                            eprintln!("filer: dropping {}::{symbol}: no methods left", file.path);
                            continue;
                        }
                        shown_file.methods = kept.iter().cloned().collect();
                        Some(BundleFile::members(
                            entry.rel.clone(),
                            entry.abs.clone(),
                            file.depth,
                            reason,
                            symbol.clone(),
                            kept,
                        ))
                    }
                };
                if let Some(built) = built {
                    files.push(built);
                    shown.files.push(shown_file);
                }
            }
            None if abs.is_file() => {
                files.push(BundleFile::whole(
                    file.path.clone(),
                    abs,
                    file.depth,
                    reason,
                ));
                shown.files.push(file.clone());
            }
            None => {
                eprintln!("filer: dropping {}: no longer in the project", file.path);
                continue;
            }
        }
    }

    if files.is_empty() {
        eprintln!("filer: bundle is empty ({})", path.display());
        std::process::exit(1);
    }

    let appendix = {
        let mut single = StateDoc::new(&root);
        single.upsert(shown);
        state::to_toml_plain(&single)
    };

    let bundle_options = BundleOptions {
        title: args.title,
        depth: bundle.depth,
        appendix: Some(appendix),
        ..Default::default()
    };
    let markdown = bundle::render(&index, &files, &bundle_options);

    match args.out {
        Some(out) => {
            if let Err(error) = std::fs::write(&out, &markdown) {
                eprintln!("filer: cannot write {}: {error}", out.display());
                std::process::exit(1);
            }
            eprintln!(
                "filer: wrote {} ({} units for branch {} from {})",
                out.display(),
                files.len(),
                state::branch_label(&branch),
                path.display()
            );
        }
        None => {
            print!("{markdown}");
            eprintln!(
                "filer: {} units for branch {} from {}",
                files.len(),
                state::branch_label(&branch),
                path.display()
            );
        }
    }
}

fn index_cmd(root: &Path) {
    let index = Index::build(root);
    println!("root:       {}", index.root.display());
    println!("source files: {}", index.files.len());
    for language in lang::Language::ALL {
        let count = index
            .files
            .iter()
            .filter(|file| file.language() == Some(language))
            .count();
        if count > 0 {
            println!("  {}: {count}", language.label());
        }
    }
    println!("packages:   {}", index.by_package.len());
    println!("types:      {}", index.by_fqn.len());
    println!("symbols:    {}", index.by_symbol.len());
    println!(
        "cache:      {} reused, {} parsed",
        index.reused_count, index.parsed_count
    );

    let path = state::default_path(&index.root);
    let branch = state::current_branch(&index.root);
    match state::load(&path) {
        Some(doc) => {
            let current = doc.bundle(&branch).map_or(0, |bundle| bundle.files.len());
            println!(
                "bundle:     {} entries on {}",
                current,
                state::branch_label(&branch)
            );
            let others: Vec<String> = doc
                .bundles
                .iter()
                .filter(|bundle| bundle.name != branch)
                .map(|bundle| {
                    format!(
                        "{} ({})",
                        state::branch_label(&bundle.name),
                        bundle.files.len()
                    )
                })
                .collect();
            if !others.is_empty() {
                println!("other branches: {}", others.join(", "));
            }
        }
        None => println!("bundle:     none at {}", path.display()),
    }
}

fn state_cmd(root: &Path, args: StateArgs) {
    let path = args.file.clone().unwrap_or_else(|| {
        state::default_path(&root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
    });
    let current = state::current_branch(root);

    if args.all {
        match state::clear(&path) {
            Ok(true) => println!("filer: cleared every branch ({})", path.display()),
            Ok(false) => println!("filer: no state at {}", path.display()),
            Err(error) => {
                eprintln!("filer: cannot remove {}: {error}", path.display());
                std::process::exit(1);
            }
        }
        return;
    }

    if args.clear {
        let target = args.branch.clone().unwrap_or_else(|| current.clone());
        let Some(mut doc) = state::load(&path) else {
            println!("filer: no state at {}", path.display());
            return;
        };
        if !doc.remove(&target) {
            println!(
                "filer: no bundle for {} in {}",
                state::branch_label(&target),
                path.display()
            );
            return;
        }
        if let Err(error) = state::save(&path, &doc) {
            eprintln!("filer: cannot write {}: {error}", path.display());
            std::process::exit(1);
        }
        println!(
            "filer: cleared bundle for {} ({})",
            state::branch_label(&target),
            path.display()
        );
        return;
    }

    let Some(doc) = state::load(&path) else {
        println!("filer: no state at {}", path.display());
        return;
    };
    println!("state:    {}", path.display());
    println!("current:  {}", state::branch_label(&current));
    println!("branches: {}", doc.bundles.len());

    let target = args.branch.clone().unwrap_or_else(|| current.clone());
    let Some(bundle) = doc.bundle(&target) else {
        println!(
            "filer: no bundle for {} — stored: {}",
            state::branch_label(&target),
            doc.bundles
                .iter()
                .map(|bundle| format!(
                    "{} ({})",
                    state::branch_label(&bundle.name),
                    bundle.files.len()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
        return;
    };

    println!(
        "bundle:   {} entries, depth {}, created {}",
        bundle.files.len(),
        bundle.depth,
        bundle.created
    );
    for file in &bundle.files {
        println!("  {} (depth {}, {})", file.label(), file.depth, file.reason);
    }
}

fn locate(index: &Index, root: &Path, input: &str) -> Option<String> {
    let path = PathBuf::from(input);
    let absolute = if path.is_absolute() {
        path
    } else {
        let from_cwd = path
            .canonicalize()
            .ok()
            .filter(|candidate| candidate.starts_with(root));
        match from_cwd {
            Some(candidate) => candidate,
            None => root.join(&path),
        }
    };
    let absolute = absolute.canonicalize().ok()?;
    if !absolute.starts_with(&index.root) {
        return None;
    }
    Some(index::relative(&index.root, &absolute))
}

fn config_files(root: &Path) -> Vec<BundleFile> {
    const CANDIDATES: &[&str] = &[
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "gradle.properties",
        "build.sbt",
        "project/build.properties",
        "project/plugins.sbt",
        "tsconfig.json",
        "jsconfig.json",
        "package.json",
        "Directory.Build.props",
        "Cargo.toml",
    ];
    let mut files: Vec<BundleFile> = CANDIDATES
        .iter()
        .filter_map(|name| config_file(root, name))
        .collect();

    // C# project and solution files carry the project name, so scan the root.
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let extension = path.extension().and_then(|extension| extension.to_str());
            if matches!(extension, Some("csproj" | "sln" | "fsproj")) && path.is_file() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    if let Some(file) = config_file(root, name) {
                        files.push(file);
                    }
                }
            }
        }
    }
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    files.dedup_by(|a, b| a.rel == b.rel);
    files
}

fn config_file(root: &Path, name: &str) -> Option<BundleFile> {
    let path = root.join(name);
    path.is_file()
        .then(|| BundleFile::whole(name.to_string(), path, 0, Reason::Config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_file(
        path: &str,
        scope: Selection,
        symbol: Option<&str>,
        methods: &[&str],
        depth: u32,
        reason: &str,
    ) -> StateFile {
        StateFile {
            path: path.to_string(),
            scope,
            symbol: symbol.map(str::to_string),
            methods: methods.iter().map(|method| method.to_string()).collect(),
            depth,
            reason: reason.to_string(),
        }
    }

    #[test]
    fn picker_marking_respects_file_then_class_then_method() {
        let included = included_from(Some(BranchBundle {
            name: "main".to_string(),
            created: 1,
            depth: 2,
            files: vec![
                state_file("Whole.java", Selection::File, None, &[], 0, "seed"),
                state_file(
                    "Partial.java",
                    Selection::Methods,
                    Some("Partial"),
                    &["used"],
                    1,
                    "call",
                ),
                state_file(
                    "Class.java",
                    Selection::Symbol,
                    Some("Class"),
                    &[],
                    2,
                    "import",
                ),
            ],
        }));

        let whole = &included["Whole.java"];
        assert_eq!(whole.whole().map(|(depth, _)| depth), Some(0));
        assert!(whole.symbol("Whole").is_some());
        assert!(whole.method("Whole", "anything").is_some());

        let partial = &included["Partial.java"];
        assert!(partial.whole().is_none());
        assert!(partial.symbol("Partial").is_none());
        assert!(partial.method("Partial", "used").is_some());
        assert!(partial.method("Partial", "other").is_none());

        let class = &included["Class.java"];
        assert!(class.whole().is_none());
        assert!(class.symbol("Class").is_some());
        // A class entry covers all of its methods.
        assert!(class.method("Class", "anything").is_some());
    }

    #[test]
    fn splits_path_and_symbol() {
        assert_eq!(split_symbol("a.fe::main"), ("a.fe", Some("main")));
        assert_eq!(split_symbol("src/a.fe"), ("src/a.fe", None));
        assert_eq!(split_symbol("a::"), ("a::", None));
        assert_eq!(split_symbol("::main"), ("::main", None));
    }

    #[test]
    fn line_offsets_are_one_based() {
        let source = "a\nb\nc\n";
        assert_eq!(offset_of_line(source, 1), 0);
        assert_eq!(offset_of_line(source, 2), 2);
        assert_eq!(offset_of_line(source, 3), 4);
        assert_eq!(offset_of_line(source, 99), source.len());
    }
}
