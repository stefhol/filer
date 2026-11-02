//! `filer` — bundle source files and their import closure for a chat LLM.

mod atomic;
mod bundle;
mod csharp;
mod error;
mod fe;
mod groovy;
mod index;
mod java;
mod kotlin;
mod lang;
mod resolve;
mod rust;
mod scala;
mod schema;
mod state;
mod web;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use bundle::{BundleFile, BundleOptions};
use error::{FilerError, EXIT_EMPTY, EXIT_INTERNAL};
use index::{FileEntry, Index};
use lang::{Granularity, Language, Member, MemberKind, Symbol, SymbolKind};
use resolve::{build_graph, drop_covered_symbols, Reason, ResolveOptions, Unit};
use schema::SCHEMA_VERSION;
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

    /// Print errors as a JSON envelope instead of prose, and let `search`
    /// explain an empty result with a machine-readable hint.
    #[arg(long, global = true)]
    json: bool,

    /// Suppress progress summaries and non-fatal warnings on stderr.
    #[arg(long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List source types as JSON lines (used by the Neovim picker).
    Search(SearchArgs),
    /// Resolve an exact fully-qualified name to its span and hash.
    Resolve(ResolveArgs),
    /// Map a 1-based line to the unit that contains it.
    Locate(LocateArgs),
    /// Emit a markdown bundle for the given files plus their import closure.
    Bundle(BundleArgs),
    /// Print the import closure of one file or unit, one row per file or per unit.
    Deps(DepsArgs),
    /// List the callers of one unit, nearest first.
    Callers(CallersArgs),
    /// Explain how a unit entered the saved bundle.
    Why(WhyArgs),
    /// Find indexed use and call sites of a name.
    Refs(RefsArgs),
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
    #[arg(long, conflicts_with = "json")]
    plain: bool,
    /// Mark files already in the bundle at this state file.
    #[arg(long, value_name = "FILE")]
    state: Option<PathBuf>,
    /// Branch whose bundle is marked. Defaults to the current branch.
    #[arg(long, value_name = "BRANCH")]
    branch: Option<String>,
}

#[derive(Args)]
struct ResolveArgs {
    /// Exact fully-qualified name, e.g. `app.Pricing.discount`.
    #[arg(value_name = "FQN")]
    fqn: String,
}

#[derive(Args)]
struct LocateArgs {
    /// File to inspect, relative to the root (or absolute).
    #[arg(value_name = "FILE")]
    file: String,
    /// 1-based line number.
    #[arg(long, value_name = "LINE")]
    at_line: usize,
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
    /// File or unit to inspect: `path`, `path::Symbol` or `path::Class.method`.
    #[arg(value_name = "UNIT")]
    file: String,
    /// Import/call hops to follow.
    #[arg(long, default_value_t = 2)]
    depth: u32,
    /// List one row per unit (`path::Symbol`, `path::Class.method`) instead of
    /// collapsing the same closure to files.
    #[arg(long)]
    unit: bool,
}

#[derive(Args)]
struct CallersArgs {
    /// Unit to inspect: `path`, `path::Symbol` or `path::Class.method`.
    #[arg(value_name = "UNIT")]
    unit: String,
    /// How many caller hops to follow. 1 = direct callers only.
    #[arg(long, default_value_t = 1)]
    depth: u32,
}

#[derive(Args)]
struct WhyArgs {
    /// Unit to explain: `path`, `path::Symbol` or `path::Class.method`.
    #[arg(value_name = "UNIT")]
    unit: String,
    /// State file. Defaults to <root>/.filer/bundle.toml.
    #[arg(long, value_name = "FILE")]
    state: Option<PathBuf>,
    /// Branch whose bundle is explained. Defaults to the current branch.
    #[arg(long, value_name = "BRANCH")]
    branch: Option<String>,
}

#[derive(Args)]
struct RefsArgs {
    /// Identifier to invert: a symbol/member name as the resolver records it.
    #[arg(value_name = "NAME")]
    name: String,
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
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) if error.use_stderr() => {
            // Usage errors are internal (exit 1), never 2 ("not found").
            let json = std::env::args().any(|argument| argument == "--json");
            if json {
                FilerError::internal(error.to_string().trim().to_string()).emit(true);
            } else {
                eprint!("{error}");
            }
            std::process::exit(EXIT_INTERNAL);
        }
        // --help / --version are successful output, not errors.
        Err(error) => error.exit(),
    };
    let context = Context {
        json: cli.json,
        quiet: cli.quiet,
    };
    match run(&context, cli.root, cli.command) {
        // A successful run that produced nothing: exit 4 without stderr noise.
        Ok(true) => std::process::exit(EXIT_EMPTY),
        Ok(false) => {}
        Err(error) => {
            error.emit(context.json);
            std::process::exit(error.code.exit_code());
        }
    }
}

/// Global output switches shared by every command.
struct Context {
    /// `--json`: errors are envelopes, `search` hints at an empty result.
    json: bool,
    /// `--quiet`: no summaries and no soft warnings on stderr.
    quiet: bool,
}

impl Context {
    /// A non-fatal notice: dropped entries, skipped inputs, unresolved imports.
    /// With `--json` it is a JSON line on stderr so agents never parse prose.
    fn warn(&self, message: impl std::fmt::Display) {
        if self.quiet {
            return;
        }
        if self.json {
            eprintln!("{}", serde_json::json!({ "warning": message.to_string() }));
        } else {
            eprintln!("filer: {message}");
        }
    }

    /// The trailing "N units" line of a successful bundle or show.
    fn summary(&self, message: impl std::fmt::Display) {
        if !self.quiet {
            eprintln!("filer: {message}");
        }
    }
}

/// `Ok(true)` means "succeeded, but the result is empty" (exit code 4).
fn run(context: &Context, root: Option<PathBuf>, command: Command) -> Result<bool, FilerError> {
    let root =
        root.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match command {
        Command::Search(args) => search(&root, args, context),
        Command::Resolve(args) => resolve_cmd(&root, args, context),
        Command::Locate(args) => locate_cmd(&root, args, context),
        Command::Bundle(args) => bundle_cmd(&root, args, context).map(|()| false),
        Command::Deps(args) => deps(&root, args, context).map(|()| false),
        Command::Callers(args) => callers_cmd(&root, args, context),
        Command::Why(args) => why_cmd(&root, args, context),
        Command::Refs(args) => refs_cmd(&root, args, context),
        Command::Show(args) => show_cmd(&root, args, context).map(|()| false),
        Command::Index => {
            index_cmd(&root, context);
            Ok(false)
        }
        Command::State(args) => state_cmd(&root, args).map(|()| false),
    }
}

/// The hint an empty `search --json` carries: what this index does and does not
/// match, so an agent knows to fall back to text search for file contents.
const EMPTY_SEARCH_HINT: &str = "symbol/path index only; use text search for file contents";

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

fn search(root: &Path, args: SearchArgs, context: &Context) -> Result<bool, FilerError> {
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

    let empty = hits.is_empty();
    if empty && context.json {
        // One JSON line, so `search --json | jq .hint` explains the empty
        // result. It carries no `path`, so the picker ignores it either way.
        println!(
            "{}",
            serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "hint": EMPTY_SEARCH_HINT,
                "results": 0,
            })
        );
    }

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
                "start_line": 1,
                "end_line": file.end_line,
                "bytes": file.size,
                "est_tokens": file.size / 4,
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
                "start_line": symbol.line,
                "end_line": symbol.end_line,
                "bytes": symbol.end.saturating_sub(symbol.start),
                "est_tokens": symbol.end.saturating_sub(symbol.start) / 4,
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
            "start_line": member.line,
            "end_line": member.end_line,
            "bytes": member.end.saturating_sub(member.start),
            "est_tokens": member.end.saturating_sub(member.start) / 4,
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

    Ok(empty)
}

/// The wire shape shared by `resolve` and `locate`: one unit, its span, and a
/// TOCTOU hash. `scope` is the same string `bundle --json` uses.
struct UnitSpan<'a> {
    file: &'a FileEntry,
    scope: &'static str,
    symbol: Option<&'a str>,
    member: Option<&'a str>,
    start: usize,
    end: usize,
    start_line: usize,
    end_line: usize,
}

impl<'a> UnitSpan<'a> {
    fn symbol(file: &'a FileEntry, symbol: &'a Symbol) -> Self {
        Self {
            file,
            scope: Selection::Symbol.as_str(),
            symbol: Some(symbol.name.as_str()),
            member: None,
            start: symbol.start,
            end: symbol.end,
            start_line: symbol.line,
            end_line: symbol.end_line,
        }
    }

    fn member(file: &'a FileEntry, class: &'a Symbol, member: &'a Member) -> Self {
        Self {
            file,
            scope: Selection::Methods.as_str(),
            symbol: Some(class.name.as_str()),
            member: Some(member.name.as_str()),
            start: member.start,
            end: member.end,
            start_line: member.line,
            end_line: member.end_line,
        }
    }

    fn whole_file(file: &'a FileEntry, end: usize, end_line: usize) -> Self {
        Self {
            file,
            scope: Selection::File.as_str(),
            symbol: None,
            member: None,
            start: 0,
            end,
            start_line: 1,
            end_line,
        }
    }

    fn to_json(&self, bytes: &[u8]) -> serde_json::Value {
        serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "path": self.file.rel,
            "scope": self.scope,
            "symbol": self.symbol,
            "member": self.member,
            "start_line": self.start_line,
            "end_line": self.end_line,
            "hash": bundle::span_hash(bytes, self.start, self.end),
            "granularity": granularity_of(self.file).as_str(),
        })
    }
}

fn read_source(file: &FileEntry) -> Result<Vec<u8>, FilerError> {
    std::fs::read(&file.abs)
        .map_err(|error| FilerError::internal(format!("cannot read {}: {error}", file.rel)))
}

/// `filer resolve app.Pricing.discount`: exact FQN lookup, never fuzzy.
fn resolve_cmd(root: &Path, args: ResolveArgs, _context: &Context) -> Result<bool, FilerError> {
    let index = Index::build(root);
    let fqn = args.fqn.trim();
    for file in &index.files {
        for symbol in &file.symbols {
            let class_fqn = class_fqn(file, symbol);
            if class_fqn == fqn {
                let bytes = read_source(file)?;
                let value = UnitSpan::symbol(file, symbol).to_json(&bytes);
                println!("{value}");
                return Ok(false);
            }
            for member in &symbol.members {
                if format!("{class_fqn}.{}", member.name) == fqn {
                    let bytes = read_source(file)?;
                    let value = UnitSpan::member(file, symbol, member).to_json(&bytes);
                    println!("{value}");
                    return Ok(false);
                }
            }
        }
    }
    Err(FilerError::unit_not_found(format!("no unit has FQN {fqn}"))
        .with_hint("run `filer search --query ...` to find the indexed name"))
}

/// `filer locate --at-line 61 src/Pricing.java`: line -> the unit that owns it.
fn locate_cmd(root: &Path, args: LocateArgs, _context: &Context) -> Result<bool, FilerError> {
    let index = Index::build(root);
    let root = index.root.clone();
    let Some(rel) = locate(&index, &root, &args.file) else {
        return Err(FilerError::file_not_found(format!(
            "{} is not inside {}",
            args.file,
            root.display()
        )));
    };
    let Some(&position) = index.by_rel.get(&rel) else {
        return Err(FilerError::not_indexed(format!(
            "{rel} is not indexed (binary, skipped directory or unsupported language?)"
        ))
        .with_hint("run `filer index` to see what the index contains"));
    };
    let file = &index.files[position];
    if args.at_line == 0 || args.at_line > file.end_line {
        return Err(FilerError::unit_not_found(format!(
            "{}:{} is not inside any unit",
            file.rel, args.at_line
        ))
        .with_hint("lines are 1-based and must exist in the file"));
    }
    let bytes = read_source(file)?;
    let source = String::from_utf8_lossy(&bytes);
    let offset = offset_of_line(&source, args.at_line);

    if let Some((class, member)) = file.member_at(offset) {
        if member.kind == MemberKind::Method {
            let value = UnitSpan::member(file, class, member).to_json(&bytes);
            println!("{value}");
            return Ok(false);
        }
    }
    if let Some(symbol) = file.symbol_at(offset) {
        let value = UnitSpan::symbol(file, symbol).to_json(&bytes);
        println!("{value}");
        return Ok(false);
    }
    if granularity_of(file) == Granularity::File {
        let value = UnitSpan::whole_file(file, bytes.len(), file.end_line).to_json(&bytes);
        println!("{value}");
        return Ok(false);
    }
    Err(FilerError::unit_not_found(format!(
        "{}:{} is not inside any unit",
        file.rel, args.at_line
    ))
    .with_hint("use `filer search` to find a symbol, or bundle the whole file"))
}

/// One picker row: JSON for Telescope, or `marker<TAB>[kind] label<TAB>target`.
fn print_row(
    plain: bool,
    included: bool,
    kind: &str,
    label: &str,
    target: &str,
    mut value: serde_json::Value,
) {
    if plain {
        let marker = if included { "*" } else { " " };
        println!("{marker}\t[{kind}] {label}\t{target}");
    } else {
        // Every search row carries the wire version, so JSON-lines readers do
        // not need a separate header row (the Neovim picker ignores extra keys).
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "schema_version".to_string(),
                serde_json::json!(SCHEMA_VERSION),
            );
        }
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

fn bundle_cmd(root: &Path, args: BundleArgs, context: &Context) -> Result<(), FilerError> {
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
                            context.warn(format!(
                                "dropping {}::{symbol}: no longer in the file",
                                file.path
                            ));
                        }
                    }
                    Selection::Methods => {
                        let Some(symbol) = &file.symbol else {
                            continue;
                        };
                        let Some(class) = entry.symbol(symbol) else {
                            context.warn(format!(
                                "dropping {}::{symbol}: no longer in the file",
                                file.path
                            ));
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
                                context.warn(format!(
                                    "dropping {}::{symbol}.{member}: no longer in the file",
                                    file.path
                                ));
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
            context.warn(format!("skipping {path}: not inside {}", root.display()));
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
            Some(symbol) => seed_unit(&index, position, symbol)
                .ok_or_else(|| FilerError::symbol_not_found(&index.files[position].rel, symbol))?,
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

    let graph = build_graph(
        &index,
        &seeds,
        ResolveOptions {
            depth: args.depth,
            same_package: args.with_same_package,
            minimal: true,
        },
    );
    let resolved = graph.closure(&seeds);
    let diagnostics = graph.diagnostics;
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
        return Err(FilerError::empty_result("nothing to bundle")
            .with_hint("check the paths and symbol names; `filer search` lists what is indexed"));
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
    let mut rendered = bundle::describe(&index, &files, &bundle_options);
    rendered.diagnostics = diagnostics;
    rendered.coverage = coverage(&index);

    if let (Some(path), Some(bundle)) = (&state_path, branch_bundle) {
        let mut doc = doc.unwrap_or_else(|| StateDoc::new(&root));
        doc.upsert(bundle);
        if let Err(error) = state::save(path, &doc) {
            context.warn(format!("cannot write state {}: {error}", path.display()));
        }
    }

    let output = if context.json {
        let mut value = rendered.to_json();
        if let Some(object) = value.as_object_mut() {
            object.insert("branch".to_string(), serde_json::json!(branch));
        }
        value.to_string()
    } else {
        // Markdown cannot carry the list; at least say the slice is incomplete.
        if !rendered.diagnostics.is_empty() {
            context.warn(format!(
                "{} diagnostics: the bundle is an incomplete slice (see --json)",
                rendered.diagnostics.len()
            ));
        }
        rendered.markdown
    };

    match args.out {
        Some(path) => {
            if let Err(error) = std::fs::write(&path, &output) {
                return Err(FilerError::internal(format!(
                    "cannot write {}: {error}",
                    path.display()
                )));
            }
            context.summary(format!(
                "wrote {} ({} units, {} new)",
                path.display(),
                files.len(),
                fresh
            ));
        }
        None => {
            print!("{output}");
            context.summary(format!("{} units, {} new on this run", files.len(), fresh));
        }
    }

    Ok(())
}

/// Index-wide completeness counters for the `bundle --json`/`show --json`
/// envelope. `imports_unresolved` counts across the index, not only the units
/// in the bundle; the per-unit list lives in `diagnostics`.
fn coverage(index: &Index) -> bundle::Coverage {
    bundle::Coverage {
        files_indexed: index.files.len(),
        files_skipped: index.files_skipped,
        imports_unresolved: resolve::unresolved_import_count(index),
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

fn deps(root: &Path, args: DepsArgs, context: &Context) -> Result<(), FilerError> {
    let index = Index::build(root);
    let root = index.root.clone();
    let seed = resolve_unit(&index, &root, &args.file)?;
    let seeds = [seed.clone()];

    // File mode and `--unit` mode share one traversal; file mode only collapses
    // it to one row per file. A member seed therefore shows its files in file
    // mode and its exact method closure with `--unit`.
    let graph = build_graph(
        &index,
        &seeds,
        ResolveOptions {
            depth: args.depth,
            same_package: false,
            minimal: false,
        },
    );
    let units = graph.units(&seeds);
    let order = if args.unit {
        units
    } else {
        collapse_to_files(units)
    };

    if context.json {
        let rows: Vec<_> = order
            .iter()
            .map(|(unit, depth, reason)| {
                let file = &index.files[unit.file()];
                let mut row = serde_json::json!({
                    "path": file.rel,
                    "fqn": file.primary_fqn(),
                    "depth": depth,
                    "reason": reason.as_str(),
                });
                if let Some(object) = row.as_object_mut() {
                    object.insert(
                        "unit".to_string(),
                        serde_json::json!(unit_label(&index, unit)),
                    );
                    object.insert("scope".to_string(), serde_json::json!(unit_scope(unit)));
                    object.insert("symbol".to_string(), serde_json::json!(unit.symbol()));
                    object.insert("member".to_string(), serde_json::json!(unit.member()));
                }
                row
            })
            .collect();

        // Every materialized edge, so a consumer can redraw the graph instead
        // of inferring it from depths. Labels are canonical unit labels.
        let mut edges: Vec<(String, String, u32, &'static str)> = Vec::new();
        for (from, outgoing) in &graph.out {
            for edge in outgoing {
                edges.push((
                    unit_label(&index, from),
                    unit_label(&index, &edge.to),
                    edge.line,
                    edge.reason.as_str(),
                ));
            }
        }
        edges.sort();
        edges.dedup();
        let edges: Vec<_> = edges
            .iter()
            .map(|(from, to, line, reason)| {
                serde_json::json!({
                    "from": from,
                    "to": to,
                    "reason": reason,
                    "line": (*line != 0).then_some(*line),
                })
            })
            .collect();

        let diagnostics: Vec<_> = graph.diagnostics.iter().map(|d| d.to_json()).collect();
        println!(
            "{}",
            serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "seed": unit_label(&index, &seed),
                "granularity": if args.unit { "unit" } else { "file" },
                "deps": rows,
                "edges": edges,
                "diagnostics": diagnostics,
            })
        );
        return Ok(());
    }

    for (unit, depth, reason) in &order {
        let suffix = match (depth, reason) {
            (0, _) => String::new(),
            (_, reason) => format!(" ({})", reason.as_str()),
        };
        let label = if args.unit {
            unit_label(&index, unit)
        } else {
            index.files[unit.file()].rel.clone()
        };
        println!("{}{}{}", "  ".repeat(*depth as usize), label, suffix);
    }

    for diagnostic in &graph.diagnostics {
        context.warn(diagnostic.to_line());
    }

    Ok(())
}

/// Collapse a unit closure to one row per file, keeping discovery order and
/// the smallest depth seen. This is the default `deps` granularity.
fn collapse_to_files(units: Vec<(Unit, u32, Reason)>) -> Vec<(Unit, u32, Reason)> {
    let mut slots: HashMap<usize, usize> = HashMap::new();
    let mut collapsed: Vec<(Unit, u32, Reason)> = Vec::new();
    for (unit, depth, reason) in units {
        match slots.get(&unit.file()).copied() {
            Some(slot) => {
                if depth < collapsed[slot].1 {
                    collapsed[slot].1 = depth;
                }
            }
            None => {
                slots.insert(unit.file(), collapsed.len());
                collapsed.push((Unit::File(unit.file()), depth, reason));
            }
        }
    }
    collapsed
}

/// Resolve a CLI unit argument (`path`, `path::Symbol`, `path::Class.method`)
/// against the index, with the same seed rules `bundle` uses.
fn resolve_unit(index: &Index, root: &Path, input: &str) -> Result<Unit, FilerError> {
    let (path, symbol) = split_symbol(input);
    let Some(rel) = locate(index, root, path) else {
        return Err(FilerError::file_not_found(format!(
            "{path} is not inside {}",
            root.display()
        )));
    };
    let Some(&position) = index.by_rel.get(&rel) else {
        return Err(FilerError::not_indexed(format!(
            "{rel} is not indexed (binary, skipped directory or unsupported language?)"
        ))
        .with_hint("run `filer index` to see what the index contains"));
    };
    match symbol {
        Some(symbol) => seed_unit(index, position, symbol)
            .ok_or_else(|| FilerError::symbol_not_found(&index.files[position].rel, symbol)),
        None => Ok(Unit::File(position)),
    }
}

/// Canonical `path`, `path::Symbol` or `path::Class.method` of a unit.
fn unit_label(index: &Index, unit: &Unit) -> String {
    let rel = index
        .files
        .get(unit.file())
        .map(|file| file.rel.as_str())
        .unwrap_or("");
    match unit {
        Unit::File(_) => rel.to_string(),
        Unit::Symbol { symbol, .. } => format!("{rel}::{symbol}"),
        Unit::Member { class, member, .. } => format!("{rel}::{class}.{member}"),
    }
}

fn unit_scope(unit: &Unit) -> &'static str {
    match unit {
        Unit::File(_) => "file",
        Unit::Symbol { .. } => "symbol",
        Unit::Member { .. } => "methods",
    }
}

/// The units recorded in one branch bundle, validated against the live index.
fn bundle_units(index: &Index, bundle: &BranchBundle) -> Vec<(Unit, u32, Reason)> {
    let mut units = Vec::new();
    for file in &bundle.files {
        let Some(&position) = index.by_rel.get(&file.path) else {
            continue;
        };
        let depth = file.depth;
        match file.scope {
            Selection::File => {
                units.push((Unit::File(position), depth, Reason::from_str(&file.reason)))
            }
            Selection::Symbol => {
                if let Some(symbol) = &file.symbol {
                    if index.files[position].symbol(symbol).is_some() {
                        units.push((
                            Unit::Symbol {
                                file: position,
                                symbol: symbol.clone(),
                            },
                            depth,
                            Reason::from_str(&file.reason),
                        ));
                    }
                }
            }
            Selection::Methods => {
                let Some(class) = &file.symbol else {
                    continue;
                };
                for member in &file.methods {
                    units.push((
                        Unit::Member {
                            file: position,
                            class: class.clone(),
                            member: member.clone(),
                        },
                        depth,
                        Reason::from_str(&file.reason),
                    ));
                }
            }
        }
    }
    units
}

fn callers_cmd(root: &Path, args: CallersArgs, context: &Context) -> Result<bool, FilerError> {
    let index = Index::build(root);
    let root = index.root.clone();
    let target = resolve_unit(&index, &root, &args.unit)?;
    let found = resolve::callers(&index, &target, args.depth);

    if found.is_empty() {
        if context.json {
            println!("[]");
        }
        return Ok(true);
    }

    if context.json {
        let rows: Vec<serde_json::Value> = found
            .iter()
            .map(|caller| {
                serde_json::json!({
                    "caller": unit_label(&index, &caller.unit),
                    "line": caller.line,
                    "depth": caller.depth,
                })
            })
            .collect();
        println!("{}", serde_json::json!(rows));
        return Ok(false);
    }

    for caller in &found {
        println!(
            "{}{} (line {})",
            "  ".repeat(caller.depth.saturating_sub(1) as usize),
            unit_label(&index, &caller.unit),
            caller.line
        );
    }
    Ok(false)
}

fn why_cmd(root: &Path, args: WhyArgs, context: &Context) -> Result<bool, FilerError> {
    let index = Index::build(root);
    let root = index.root.clone();
    let target = resolve_unit(&index, &root, &args.unit)?;
    let path = args
        .state
        .clone()
        .unwrap_or_else(|| state::default_path(&root));
    let Some(doc) = state::load(&path) else {
        return Err(FilerError::new(
            error::Code::StateNotFound,
            format!("no bundle state at {}", path.display()),
        )
        .with_hint("bundle something first"));
    };
    let branch = args
        .branch
        .clone()
        .unwrap_or_else(|| state::current_branch(&root));
    let Some(bundle) = doc.bundle(&branch).cloned() else {
        return Err(FilerError::new(
            error::Code::BranchNotFound,
            format!(
                "no bundle for branch {} in {}",
                state::branch_label(&branch),
                path.display()
            ),
        )
        .with_hint("`filer state` lists every branch with a bundle"));
    };

    let entries = bundle_units(&index, &bundle);
    let target_entry = entries.iter().find(|(unit, _, _)| unit == &target).cloned();
    let mut seeds: Vec<Unit> = entries
        .iter()
        .filter(|(_, _, reason)| *reason == Reason::Seed)
        .map(|(unit, _, _)| unit.clone())
        .collect();
    if seeds.is_empty() {
        seeds = entries
            .iter()
            .filter(|(_, depth, _)| *depth == 0)
            .map(|(unit, _, _)| unit.clone())
            .collect();
    }
    let max_depth = bundle
        .depth
        .max(target_entry.as_ref().map_or(0, |(_, depth, _)| *depth))
        .max(1);

    let mut chain = resolve::why(&index, &seeds, &target, max_depth);
    if chain.is_none() {
        // A whole-file or whole-class seed covers every unit inside it; there is
        // no edge to walk when the target sits inside that seed.
        if let Some((cover, _, Reason::Seed)) = entries.iter().find(|(unit, _, reason)| {
            *reason == Reason::Seed
                && match unit {
                    Unit::File(file) => *file == target.file(),
                    Unit::Symbol { file, symbol } => {
                        *file == target.file() && target.symbol() == Some(symbol.as_str())
                    }
                    Unit::Member { .. } => false,
                }
        }) {
            let mut hops = vec![resolve::WhyHop {
                unit: cover.clone(),
                line: 0,
                reason: Reason::Seed,
            }];
            if cover != &target {
                hops.push(resolve::WhyHop {
                    unit: target.clone(),
                    line: 0,
                    reason: Reason::Seed,
                });
            }
            chain = Some(hops);
        }
    }

    let Some(chain) = chain else {
        return Err(FilerError::new(
            error::Code::SymbolNotFound,
            format!(
                "{} is not reachable from a seed in {}",
                unit_label(&index, &target),
                path.display()
            ),
        )
        .with_hint("bundle the unit first, or pass --state/--branch"));
    };

    let reason = target_entry
        .as_ref()
        .map(|(_, _, reason)| *reason)
        .or_else(|| chain.last().map(|hop| hop.reason))
        .unwrap_or(Reason::Usage);
    let depth = target_entry
        .as_ref()
        .map(|(_, depth, _)| *depth)
        .unwrap_or(chain.len().saturating_sub(1) as u32);

    if context.json {
        let hops: Vec<serde_json::Value> = chain
            .iter()
            .enumerate()
            .map(|(position, hop)| {
                let recorded = entries.iter().find(|(unit, _, _)| unit == &hop.unit);
                serde_json::json!({
                    "unit": unit_label(&index, &hop.unit),
                    "path": index.files.get(hop.unit.file()).map(|file| file.rel.clone()),
                    "scope": unit_scope(&hop.unit),
                    "symbol": hop.unit.symbol(),
                    "member": hop.unit.member(),
                    "line": if hop.line == 0 {
                        serde_json::Value::Null
                    } else {
                        serde_json::json!(hop.line)
                    },
                    "reason": hop.reason.as_str(),
                    "depth": recorded.map_or(position as u32, |(_, depth, _)| *depth),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "target": unit_label(&index, &target),
                "reason": reason.as_str(),
                "depth": depth,
                "chain": hops,
            })
        );
        return Ok(false);
    }

    println!(
        "{} ({} at depth {})",
        unit_label(&index, &target),
        reason.as_str(),
        depth
    );
    for (position, hop) in chain.iter().enumerate() {
        if position == 0 {
            println!("seed {}", unit_label(&index, &hop.unit));
        } else {
            println!(
                "  -> {} ({} line {})",
                unit_label(&index, &hop.unit),
                hop.reason.as_str(),
                hop.line
            );
        }
    }
    Ok(false)
}

fn refs_cmd(root: &Path, args: RefsArgs, context: &Context) -> Result<bool, FilerError> {
    let index = Index::build(root);
    let found = resolve::references(&index, &args.name);

    if found.is_empty() {
        if context.json {
            println!("[]");
        }
        return Ok(true);
    }

    if context.json {
        let rows: Vec<serde_json::Value> = found
            .iter()
            .map(|reference| {
                serde_json::json!({
                    "path": index.files[reference.file].rel,
                    "symbol": reference.symbol,
                    "member": reference.member,
                    "line": reference.line,
                    "kind": reference.kind.as_str(),
                })
            })
            .collect();
        println!("{}", serde_json::json!(rows));
        return Ok(false);
    }

    for reference in &found {
        let label = match &reference.member {
            Some(member) => format!("{}.{member}", reference.symbol),
            None => reference.symbol.clone(),
        };
        println!(
            "{}:{} {} {}",
            index.files[reference.file].rel,
            reference.line,
            reference.kind.as_str(),
            label
        );
    }
    Ok(false)
}

/// Re-renders one branch's bundle from the state file: same files, same depths
/// and reasons, current file contents.
fn show_cmd(root: &Path, args: ShowArgs, context: &Context) -> Result<(), FilerError> {
    let index = Index::build(root);
    let root = index.root.clone();
    let path = args
        .state
        .clone()
        .unwrap_or_else(|| state::default_path(&root));

    let Some(doc) = state::load(&path) else {
        return Err(FilerError::new(
            error::Code::StateNotFound,
            format!("no bundle state at {}", path.display()),
        )
        .with_hint("bundle something first"));
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
        return Err(FilerError::new(
            error::Code::BranchNotFound,
            format!(
                "no bundle for branch {} (have: {})",
                state::branch_label(&branch),
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                }
            ),
        )
        .with_hint("`filer state` lists every branch with a bundle"));
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
                            context.warn(format!(
                                "dropping {}::{symbol}: no longer in the file",
                                file.path
                            ));
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
                            context.warn(format!(
                                "dropping {}::{symbol}: no longer in the file",
                                file.path
                            ));
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
                                context.warn(format!(
                                    "dropping {}::{symbol}.{member}: no longer in the file",
                                    file.path
                                ));
                            }
                        }
                        if kept.is_empty() {
                            context
                                .warn(format!("dropping {}::{symbol}: no methods left", file.path));
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
                context.warn(format!("dropping {}: no longer in the project", file.path));
                continue;
            }
        }
    }

    if files.is_empty() {
        return Err(
            FilerError::empty_result(format!("bundle is empty ({})", path.display()))
                .with_hint("`:FilerReset` and bundle again"),
        );
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
    let mut rendered = bundle::describe(&index, &files, &bundle_options);
    // `show` re-renders stored units without resolving, so there is no
    // traversal to report; coverage still describes the index behind it.
    rendered.coverage = coverage(&index);

    let output = if context.json {
        let mut value = rendered.to_json();
        if let Some(object) = value.as_object_mut() {
            object.insert("branch".to_string(), serde_json::json!(branch));
        }
        value.to_string()
    } else {
        rendered.markdown
    };

    match args.out {
        Some(out) => {
            if let Err(error) = std::fs::write(&out, &output) {
                return Err(FilerError::internal(format!(
                    "cannot write {}: {error}",
                    out.display()
                )));
            }
            context.summary(format!(
                "wrote {} ({} units for branch {} from {})",
                out.display(),
                files.len(),
                state::branch_label(&branch),
                path.display()
            ));
        }
        None => {
            print!("{output}");
            context.summary(format!(
                "{} units for branch {} from {}",
                files.len(),
                state::branch_label(&branch),
                path.display()
            ));
        }
    }

    Ok(())
}

fn index_cmd(root: &Path, context: &Context) {
    let index = Index::build(root);
    let path = state::default_path(&index.root);
    let branch = state::current_branch(&index.root);
    let doc = state::load(&path);

    if context.json {
        let languages: Vec<serde_json::Value> = lang::Language::ALL
            .iter()
            .filter_map(|language| {
                let files = index
                    .files
                    .iter()
                    .filter(|file| file.language() == Some(*language))
                    .count();
                (files > 0).then(|| {
                    serde_json::json!({
                        "language": language.label(),
                        "files": files,
                    })
                })
            })
            .collect();
        let state = doc.map(|doc| {
            let current = doc.bundle(&branch).map(|bundle| bundle.files.len());
            let other_branches: Vec<serde_json::Value> = doc
                .bundles
                .iter()
                .filter(|bundle| bundle.name != branch)
                .map(|bundle| {
                    serde_json::json!({
                        "branch": bundle.name,
                        "label": state::branch_label(&bundle.name),
                        "entries": bundle.files.len(),
                    })
                })
                .collect();
            serde_json::json!({
                "branch": branch,
                "label": state::branch_label(&branch),
                "entries": current,
                "other_branches": other_branches,
            })
        });
        println!(
            "{}",
            serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "root": index.root.to_string_lossy(),
                "source_files": index.files.len(),
                "languages": languages,
                "packages": index.by_package.len(),
                "types": index.by_fqn.len(),
                "symbols": index.by_symbol.len(),
                "cache": {
                    "reused": index.reused_count,
                    "parsed": index.parsed_count,
                },
                "state": state,
            })
        );
        return;
    }

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

    match doc {
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

fn state_cmd(root: &Path, args: StateArgs) -> Result<(), FilerError> {
    let path = args.file.clone().unwrap_or_else(|| {
        state::default_path(&root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
    });
    let current = state::current_branch(root);

    if args.all {
        match state::clear(&path) {
            Ok(true) => println!("filer: cleared every branch ({})", path.display()),
            Ok(false) => println!("filer: no state at {}", path.display()),
            Err(error) => {
                return Err(FilerError::internal(format!(
                    "cannot remove {}: {error}",
                    path.display()
                )));
            }
        }
        return Ok(());
    }

    if args.clear {
        let target = args.branch.clone().unwrap_or_else(|| current.clone());
        let Some(mut doc) = state::load(&path) else {
            println!("filer: no state at {}", path.display());
            return Ok(());
        };
        if !doc.remove(&target) {
            println!(
                "filer: no bundle for {} in {}",
                state::branch_label(&target),
                path.display()
            );
            return Ok(());
        }
        if let Err(error) = state::save(&path, &doc) {
            return Err(FilerError::internal(format!(
                "cannot write {}: {error}",
                path.display()
            )));
        }
        println!(
            "filer: cleared bundle for {} ({})",
            state::branch_label(&target),
            path.display()
        );
        return Ok(());
    }

    let Some(doc) = state::load(&path) else {
        println!("filer: no state at {}", path.display());
        return Ok(());
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
        return Ok(());
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

    Ok(())
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
    fn empty_search_hint_names_the_index_limits() {
        // Locked down: the issue asks for exactly this explanation.
        assert_eq!(
            EMPTY_SEARCH_HINT,
            "symbol/path index only; use text search for file contents"
        );
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
