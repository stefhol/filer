//! Markdown rendering of a code bundle that can be pasted into a chat.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::Index;
use crate::lang::{Language, MemberKind, Symbol};
use crate::resolve::{Diagnostic, Reason};
use crate::schema::SCHEMA_VERSION;
use crate::state::Selection;

/// Source bytes per token. `tokens` is `bytes / 4` for the unit span; the
/// budget is metered in the same unit, so there is deliberately no tokenizer
/// dependency and `--max-tokens N` is exactly `--max-bytes N * 4`.
pub const BYTES_PER_TOKEN: usize = 4;

/// Built-in per-file size limit, so one huge file cannot swallow the bundle.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 1024 * 1024;

pub struct BundleOptions {
    pub title: Option<String>,
    pub depth: u32,
    pub max_file_bytes: u64,
    /// Total source-byte budget for the whole bundle. `None` = unlimited.
    /// Truncation is a strict prefix of the canonical order (see [`describe`]).
    pub max_bytes: Option<u64>,
    /// Machine-readable TOML listing the bundled files, appended to the bundle.
    pub appendix: Option<String>,
}

impl Default for BundleOptions {
    fn default() -> Self {
        Self {
            title: None,
            depth: 2,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_bytes: None,
            appendix: None,
        }
    }
}

/// One unit of a bundle: a whole file, one symbol excerpt, or a class shell
/// with only some methods kept. `methods` is non-empty exactly when the scope
/// is [`Selection::Methods`].
pub struct BundleFile {
    pub rel: String,
    pub abs: PathBuf,
    pub scope: Selection,
    /// Top-level declaration for `symbol`/`methods` scopes.
    pub symbol: Option<String>,
    /// Kept methods for `methods` scope; the rest of the class is rendered as
    /// signature stubs.
    pub methods: BTreeSet<String>,
    pub depth: u32,
    /// Why the unit is in the bundle; shown in the file list.
    pub reason: Reason,
}

impl BundleFile {
    pub fn whole(rel: String, abs: PathBuf, depth: u32, reason: Reason) -> Self {
        Self {
            rel,
            abs,
            scope: Selection::File,
            symbol: None,
            methods: BTreeSet::new(),
            depth,
            reason,
        }
    }

    pub fn excerpt(rel: String, abs: PathBuf, depth: u32, reason: Reason, symbol: String) -> Self {
        Self {
            rel,
            abs,
            scope: Selection::Symbol,
            symbol: Some(symbol),
            methods: BTreeSet::new(),
            depth,
            reason,
        }
    }

    pub fn members(
        rel: String,
        abs: PathBuf,
        depth: u32,
        reason: Reason,
        symbol: String,
        methods: BTreeSet<String>,
    ) -> Self {
        Self {
            rel,
            abs,
            scope: Selection::Methods,
            symbol: Some(symbol),
            methods,
            depth,
            reason,
        }
    }

    pub fn label(&self) -> String {
        match self.scope {
            Selection::File => format!("`{}`", self.rel),
            Selection::Symbol => match &self.symbol {
                Some(symbol) => format!("`{}` :: `{symbol}`", self.rel),
                None => format!("`{}`", self.rel),
            },
            Selection::Methods => match &self.symbol {
                Some(symbol) => format!(
                    "`{}` :: `{symbol}` [{}]",
                    self.rel,
                    self.methods
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None => format!("`{}`", self.rel),
            },
        }
    }
}

/// Byte and line span of one rendered unit.
#[derive(Clone, Copy, Debug)]
struct Span {
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    end_line: usize,
}

/// The result of rendering one unit: the markdown section and, when the unit
/// could be read, its span. `note` is the machine-readable reason an entry was
/// left out (it is also written into the markdown as prose).
struct UnitRender {
    markdown: String,
    rendered: bool,
    note: Option<&'static str>,
    span: Option<Span>,
    /// BLAKE3 of the raw span, hex encoded (see [`span_hash`]).
    hash: Option<String>,
}

impl UnitRender {
    fn rendered(markdown: String, span: Span, hash: String) -> Self {
        Self {
            markdown,
            rendered: true,
            note: None,
            span: Some(span),
            hash: Some(hash),
        }
    }

    fn omitted(markdown: String, note: &'static str) -> Self {
        Self {
            markdown,
            rendered: false,
            note: Some(note),
            span: None,
            hash: None,
        }
    }
}

/// One unit in the JSON surface. The first thirteen fields are the wire
/// contract (`path` .. `rendered`); `label` and `markdown` carry the exact strings the
/// markdown renderer emits so consumers can rebuild it.
pub struct BundleEntry {
    pub path: String,
    pub scope: Selection,
    pub symbol: Option<String>,
    pub methods: Vec<String>,
    pub depth: u32,
    pub reason: Reason,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub start_byte: Option<usize>,
    pub end_byte: Option<usize>,
    pub tokens: usize,
    /// Hex BLAKE3 of the raw source span; `None` when the unit was omitted.
    pub hash: Option<String>,
    pub rendered: bool,
    /// The `## Files` line for this unit, without the leading `- `.
    pub label: String,
    /// The exact `## Contents` section for this unit.
    pub markdown: String,
    /// Why the unit was not rendered (`too_large`, `unreadable`, ...).
    pub note: Option<&'static str>,
}

/// How much of the index the JSON envelope stands on. Index-wide, so a
/// `bundle` and a `show` over the same root report the same numbers.
#[derive(Debug, Clone, Copy, Default)]
pub struct Coverage {
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub imports_unresolved: usize,
}

impl Coverage {
    /// The one shape every envelope uses (`bundle`, `show`, `index`), so the
    /// three cannot drift apart.
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "files_indexed": self.files_indexed,
            "files_skipped": self.files_skipped,
            "imports_unresolved": self.imports_unresolved,
        })
    }
}

/// The bundle as data. `markdown` and [`RenderedBundle::to_json`] are both
/// derived from `entries`, so the two surfaces cannot describe different units.
pub struct RenderedBundle {
    pub title: String,
    pub root: String,
    pub depth: u32,
    pub max_file_bytes: u64,
    /// Total byte budget, `None` when the bundle is not budgeted.
    pub max_bytes: Option<u64>,
    /// [`Self::max_bytes`] expressed in tokens (`bytes / 4`).
    pub max_tokens: Option<usize>,
    pub files: usize,
    pub excerpts: usize,
    pub methods: usize,
    pub appendix: Option<String>,
    pub entries: Vec<BundleEntry>,
    /// Units the budget cut, in canonical order. They are not part of
    /// `entries` or the markdown, and `omitted` reports them as `budget`.
    pub dropped: Vec<BundleEntry>,
    pub markdown: String,
    /// What the traversal declined to include (empty for `show`, which does not
    /// resolve).
    pub diagnostics: Vec<Diagnostic>,
    /// Index-wide completeness counters for the same envelope.
    pub coverage: Coverage,
}

impl RenderedBundle {
    /// The versioned JSON envelope. `entries` always names the same units as
    /// the markdown; `omitted` is the subset that was not rendered (including
    /// everything the budget cut), `diagnostics` is what the traversal
    /// declined, and `coverage` says how complete the index behind it is.
    pub fn to_json(&self) -> serde_json::Value {
        let entries: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "path": entry.path,
                    "scope": entry.scope.as_str(),
                    "symbol": entry.symbol,
                    "methods": entry.methods,
                    "depth": entry.depth,
                    "reason": entry.reason.as_str(),
                    "start_line": entry.start_line,
                    "end_line": entry.end_line,
                    "start_byte": entry.start_byte,
                    "end_byte": entry.end_byte,
                    "tokens": entry.tokens,
                    "hash": entry.hash,
                    "rendered": entry.rendered,
                    "label": entry.label,
                    "markdown": entry.markdown,
                })
            })
            .collect();
        // `omitted` is one list with two sources: units that were selected but
        // could not be rendered, and units the budget cut from the tail of the
        // canonical order. A dropped unit keeps its position only in this list.
        let rendered_omitted = self
            .entries
            .iter()
            .filter(|entry| !entry.rendered)
            .map(|entry| {
                serde_json::json!({
                    "path": entry.path,
                    "scope": entry.scope.as_str(),
                    "symbol": entry.symbol,
                    "reason": entry.note.unwrap_or("unrendered"),
                })
            });
        let budget_omitted = self.dropped.iter().map(|entry| {
            serde_json::json!({
                "path": entry.path,
                "scope": entry.scope.as_str(),
                "symbol": entry.symbol,
                "reason": "budget",
            })
        });
        let omitted: Vec<serde_json::Value> = rendered_omitted.chain(budget_omitted).collect();
        let used_bytes: usize = self
            .entries
            .iter()
            .filter_map(|entry| match (entry.start_byte, entry.end_byte) {
                (Some(start), Some(end)) => Some(end.saturating_sub(start)),
                _ => None,
            })
            .sum();
        let used_tokens: usize = self.entries.iter().map(|entry| entry.tokens).sum();
        let truncated = !self.dropped.is_empty()
            || self
                .entries
                .iter()
                .any(|entry| entry.note == Some("too_large"));
        let diagnostics: Vec<serde_json::Value> =
            self.diagnostics.iter().map(Diagnostic::to_json).collect();
        serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "root": self.root,
            "title": self.title,
            "depth": self.depth,
            "files": self.files,
            "excerpts": self.excerpts,
            "methods": self.methods,
            "budget": {
                "max_file_bytes": self.max_file_bytes,
                "max_bytes": self.max_bytes,
                "max_tokens": self.max_tokens,
                "used_bytes": used_bytes,
                "used_tokens": used_tokens,
                "truncated": truncated,
            },
            "entries": entries,
            "omitted": omitted,
            "diagnostics": diagnostics,
            "coverage": self.coverage.to_json(),
            "state_toml": self.appendix,
        })
    }
}

enum Content<'a> {
    Single(&'a BundleFile),
    Class {
        entry: &'a BundleFile,
        members: BTreeSet<&'a str>,
    },
}

impl Content<'_> {
    fn entry(&self) -> &BundleFile {
        match self {
            Content::Single(file) => file,
            Content::Class { entry, .. } => entry,
        }
    }

    fn label(&self) -> String {
        match self {
            Content::Single(file) => file.label(),
            Content::Class { entry, members } => format!(
                "`{}` :: `{}` [{}]",
                entry.rel,
                entry.symbol.as_deref().unwrap_or(""),
                members.iter().copied().collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// `file > symbol > methods`: an entry whose file or class is already covered
/// adds nothing.
fn visible(files: &[BundleFile]) -> Vec<&BundleFile> {
    let whole: HashSet<&str> = files
        .iter()
        .filter(|file| file.scope == Selection::File)
        .map(|file| file.rel.as_str())
        .collect();
    let classes: HashSet<(&str, &str)> = files
        .iter()
        .filter(|file| file.scope == Selection::Symbol)
        .filter_map(|file| {
            file.symbol
                .as_deref()
                .map(|symbol| (file.rel.as_str(), symbol))
        })
        .collect();
    files
        .iter()
        .filter(|file| match file.scope {
            Selection::File => true,
            Selection::Symbol => !whole.contains(file.rel.as_str()),
            Selection::Methods => {
                !whole.contains(file.rel.as_str())
                    && file
                        .symbol
                        .as_deref()
                        .is_none_or(|symbol| !classes.contains(&(file.rel.as_str(), symbol)))
            }
        })
        .collect()
}

/// Groups method entries of the same class so the class shell is rendered once
/// with the union of the kept methods.
fn group<'a>(files: &'a [BundleFile]) -> Vec<Content<'a>> {
    let mut contents = Vec::new();
    let mut by_class: HashMap<(&str, &str), usize> = HashMap::new();
    for file in visible(files) {
        let Some(symbol) = file
            .symbol
            .as_deref()
            .filter(|_| file.scope == Selection::Methods)
        else {
            contents.push(Content::Single(file));
            continue;
        };
        let key = (file.rel.as_str(), symbol);
        match by_class.get(&key) {
            Some(&position) => {
                if let Some(Content::Class { members, .. }) = contents.get_mut(position) {
                    members.extend(file.methods.iter().map(String::as_str));
                }
            }
            None => {
                by_class.insert(key, contents.len());
                contents.push(Content::Class {
                    entry: file,
                    members: file.methods.iter().map(String::as_str).collect(),
                });
            }
        }
    }
    contents
}

/// One unit after rendering, ready to be ordered and budgeted. Rendering comes
/// first because the canonical order ends on the unit's span and the budget is
/// metered in `tokens` of the rendered span, so neither is known before.
struct Planned<'a> {
    content: Content<'a>,
    /// The `## Files` line, without the leading `- `.
    label: String,
    rendered: UnitRender,
    /// Position in the caller's unit list, so seeds keep their seed order.
    seed_order: usize,
}

impl Planned<'_> {
    fn entry(&self) -> &BundleFile {
        self.content.entry()
    }

    /// A seed is the only unit that keeps the caller's order. Everything else
    /// is sorted, which is what makes the bundle reproducible.
    fn is_seed(&self) -> bool {
        let entry = self.entry();
        entry.depth == 0 && entry.reason == Reason::Seed
    }

    fn bytes(&self) -> u64 {
        span_bytes(self.rendered.span)
    }

    /// The canonical order of the bundle, which is also the truncation
    /// priority: seeds in command-line order, then `file` > `symbol` >
    /// `methods`, then ascending depth, then `reason` (`seed` before `call`
    /// before the import reasons), then path, then span.
    fn key(&self) -> (u8, usize, u8, u32, u8, &str, &str, usize) {
        let entry = self.entry();
        let seed = self.is_seed();
        (
            u8::from(!seed),
            if seed { self.seed_order } else { 0 },
            scope_rank(entry.scope),
            entry.depth,
            reason_rank(entry.reason),
            entry.rel.as_str(),
            entry.symbol.as_deref().unwrap_or(""),
            self.rendered.span.map(|span| span.start_byte).unwrap_or(0),
        )
    }

    /// The wire entry for this unit. The markdown section travels with it, so
    /// `entries` and the rendered bundle cannot drift apart.
    fn into_entry(self) -> BundleEntry {
        let file = self.entry();
        let path = file.rel.clone();
        let scope = file.scope;
        let symbol = file.symbol.clone();
        let depth = file.depth;
        let reason = file.reason;
        let methods = match &self.content {
            Content::Class { members, .. } => {
                members.iter().map(|member| (*member).to_string()).collect()
            }
            Content::Single(_) => file.methods.iter().cloned().collect(),
        };
        let label = self.label;
        let unit = self.rendered;
        let (start_line, end_line, start_byte, end_byte) = span_fields(unit.span);
        BundleEntry {
            path,
            scope,
            symbol,
            methods,
            depth,
            reason,
            start_line,
            end_line,
            start_byte,
            end_byte,
            tokens: span_tokens(unit.span),
            hash: unit.hash,
            rendered: unit.rendered,
            label,
            markdown: unit.markdown,
            note: unit.note,
        }
    }
}

/// `file` > `symbol` > `methods`: the truncation priority of #12. Shared with
/// `bundle_cmd` so the state file is written in the same order.
pub fn scope_rank(scope: Selection) -> u8 {
    match scope {
        Selection::File => 0,
        Selection::Symbol => 1,
        Selection::Methods => 2,
    }
}

/// `seed` before `call` before the import and usage reasons, then the
/// `--with-*` extras. This is the documented truncation priority.
pub fn reason_rank(reason: Reason) -> u8 {
    match reason {
        Reason::Seed => 0,
        Reason::Call | Reason::StepDefinition => 1,
        Reason::Import
        | Reason::StaticImport
        | Reason::WildcardImport
        | Reason::ModuleImport
        | Reason::PathImport => 2,
        Reason::SamePackage | Reason::Usage => 3,
        Reason::Package => 4,
        Reason::Config => 5,
        Reason::External => 6,
    }
}

/// Source bytes of a rendered unit; 0 for one that could not be rendered.
fn span_bytes(span: Option<Span>) -> u64 {
    span.map(|span| span.end_byte.saturating_sub(span.start_byte) as u64)
        .unwrap_or(0)
}

/// `tokens` of a rendered unit: `bytes / 4`, rounded down.
fn span_tokens(span: Option<Span>) -> usize {
    span.map(|span| span.end_byte.saturating_sub(span.start_byte) / BYTES_PER_TOKEN)
        .unwrap_or(0)
}

fn span_fields(span: Option<Span>) -> (Option<usize>, Option<usize>, Option<usize>, Option<usize>) {
    match span {
        Some(span) => (
            Some(span.start_line),
            Some(span.end_line),
            Some(span.start_byte),
            Some(span.end_byte),
        ),
        None => (None, None, None, None),
    }
}

/// Splits the canonical order into the prefix that fits the budget and the tail
/// that does not. The cut is strict: the first unit that would overflow takes
/// every later unit with it, so a smaller budget only ever removes entries from
/// the tail and never reorders the rest.
fn split_by_budget(
    planned: Vec<Planned<'_>>,
    max_bytes: Option<u64>,
) -> (Vec<Planned<'_>>, Vec<Planned<'_>>) {
    let Some(max_bytes) = max_bytes else {
        return (planned, Vec::new());
    };
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut used = 0u64;
    for planned in planned {
        let fits = dropped.is_empty() && used.saturating_add(planned.bytes()) <= max_bytes;
        if fits {
            used += planned.bytes();
            kept.push(planned);
        } else {
            dropped.push(planned);
        }
    }
    (kept, dropped)
}

/// The `## Files` line for one unit, with its depth and reason.
fn label_with_detail(content: &Content<'_>) -> String {
    let file = content.entry();
    let detail = match (file.depth, file.reason) {
        (0, Reason::Seed) => String::new(),
        (0, reason) => format!(" ({})", reason.as_str()),
        (depth, reason) => format!(" (depth {depth}, {})", reason.as_str()),
    };
    format!("{}{detail}", content.label())
}

/// Renders the bundle and keeps both surfaces: the markdown text and the units
/// it was built from. The JSON envelope is serialized from the same entries.
///
/// Units come out in canonical order, the budget then cuts a suffix of it, and
/// the markdown and the JSON envelope are both built from what is left.
pub fn describe(index: &Index, files: &[BundleFile], options: &BundleOptions) -> RenderedBundle {
    let name = index
        .root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    let title = options.title.clone().unwrap_or(name);
    let root = index.root.to_string_lossy().to_string();
    let max_tokens = options
        .max_bytes
        .map(|bytes| usize::try_from(bytes / BYTES_PER_TOKEN as u64).unwrap_or(usize::MAX));

    let mut planned: Vec<Planned> = group(files)
        .into_iter()
        .enumerate()
        .map(|(seed_order, content)| {
            let rendered = match &content {
                Content::Single(file) => render_unit(index, file, options),
                Content::Class { entry, members } => {
                    render_member_group(index, entry, members, options)
                }
            };
            Planned {
                label: label_with_detail(&content),
                content,
                rendered,
                seed_order,
            }
        })
        .collect();
    planned.sort_by(|left, right| left.key().cmp(&right.key()));
    let (kept, budget_dropped) = split_by_budget(planned, options.max_bytes);

    let entries: Vec<BundleEntry> = kept.into_iter().map(Planned::into_entry).collect();
    let dropped: Vec<BundleEntry> = budget_dropped
        .into_iter()
        .map(Planned::into_entry)
        .collect();

    let distinct: HashSet<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    let file_count = distinct.len();
    let excerpts = entries
        .iter()
        .filter(|entry| entry.scope != Selection::File)
        .count();
    let methods_kept: usize = entries.iter().map(|entry| entry.methods.len()).sum();

    let mut markdown = String::new();
    markdown.push_str(&format!("# Code bundle: {title}\n\n"));
    markdown.push_str(&format!("- root: `{root}`\n"));
    markdown.push_str(&format!("- files: {file_count}\n"));
    if excerpts > 0 {
        markdown.push_str(&format!("- excerpts: {excerpts}\n"));
    }
    if methods_kept > 0 {
        markdown.push_str(&format!("- methods: {methods_kept}\n"));
    }
    markdown.push_str(&format!("- import depth: {}\n\n", options.depth));

    markdown.push_str("## Files\n\n");
    for entry in &entries {
        markdown.push_str(&format!("- {}\n", entry.label));
    }
    markdown.push_str("\n## Contents\n");
    for entry in &entries {
        markdown.push_str(&entry.markdown);
    }

    if let Some(appendix) = &options.appendix {
        markdown.push_str("\n## Bundle state\n\n");
        markdown.push_str("```toml\n");
        markdown.push_str(appendix.trim_end());
        markdown.push_str("\n```\n");
    }

    RenderedBundle {
        title,
        root,
        depth: options.depth,
        max_file_bytes: options.max_file_bytes,
        max_bytes: options.max_bytes,
        max_tokens,
        files: file_count,
        excerpts,
        methods: methods_kept,
        appendix: options.appendix.clone(),
        entries,
        dropped,
        markdown,
        diagnostics: Vec::new(),
        coverage: Coverage::default(),
    }
}

fn render_unit(index: &Index, file: &BundleFile, options: &BundleOptions) -> UnitRender {
    let language = language_for(&file.rel);
    let bytes = match read_bytes(file, options) {
        Ok(bytes) => bytes,
        Err((note, code)) => {
            return UnitRender::omitted(format!("\n### {}\n\n{note}\n", file.label()), code);
        }
    };

    let Some(symbol) = file
        .symbol
        .as_ref()
        .filter(|_| file.scope != Selection::File)
    else {
        let markdown = format!(
            "\n### {}\n\n{}",
            file.label(),
            render_text(&String::from_utf8_lossy(&bytes), language)
        );
        let span = Span {
            start_byte: 0,
            end_byte: bytes.len(),
            start_line: 1,
            end_line: end_line_number(&bytes, bytes.len()),
        };
        let hash = span_hash(&bytes, span.start_byte, span.end_byte);
        return UnitRender::rendered(markdown, span, hash);
    };

    let Some(symbol_entry) = symbol_span(index, file, symbol) else {
        // The symbol moved or disappeared since the state was written.
        return UnitRender::omitted(
            format!(
                "\n### {}\n\n_(omitted: `{symbol}` is no longer part of the file)_\n",
                file.label()
            ),
            "symbol_missing",
        );
    };

    let (start, end) = (symbol_entry.start, symbol_entry.end);
    if start > end || end > bytes.len() {
        return UnitRender::omitted(
            format!(
                "\n### {}\n\n_(omitted: `{symbol}` moved while reading the file)_\n",
                file.label()
            ),
            "stale_span",
        );
    }
    let line = line_number(&bytes, start);
    let mut markdown = format!("\n### {} (line {line})\n\n", file.label());

    let preamble_end = index
        .by_rel
        .get(&file.rel)
        .map(|&position| index.files[position].preamble_end)
        .unwrap_or(0)
        .min(start);
    let mut text = String::new();
    let mut open_braces = 0;
    if preamble_end > 0 {
        let (preamble, open) = preamble_text(&bytes, preamble_end);
        open_braces = open;
        text.push_str(&preamble);
        if !text.is_empty() {
            text.push_str("\n\n");
        }
    }
    text.push_str(&String::from_utf8_lossy(&bytes[start..end]));
    for _ in 0..open_braces {
        text.push_str("\n}");
    }
    markdown.push_str(&render_text(&text, language));

    let span = Span {
        start_byte: start,
        end_byte: end,
        start_line: line,
        end_line: symbol_entry.end_line.max(line),
    };
    let hash = span_hash(&bytes, start, end);
    UnitRender::rendered(markdown, span, hash)
}

/// Reads the file unless it exceeds `max_file_bytes`. The second value is the
/// machine-readable omission code used by the JSON surface.
fn read_bytes(
    file: &BundleFile,
    options: &BundleOptions,
) -> Result<Vec<u8>, (String, &'static str)> {
    let metadata = fs::metadata(&file.abs)
        .map_err(|error| (format!("_(unreadable: {error})_"), "unreadable"))?;
    if metadata.len() > options.max_file_bytes {
        return Err((
            format!(
                "_(omitted: {} bytes, larger than the {} byte limit)_",
                metadata.len(),
                options.max_file_bytes
            ),
            "too_large",
        ));
    }
    fs::read(&file.abs).map_err(|error| (format!("_(unreadable: {error})_"), "unreadable"))
}

/// Renders a class shell with only `members` kept; every other method body is
/// replaced by its signature plus `// body omitted`.
fn render_member_group(
    index: &Index,
    entry: &BundleFile,
    members: &BTreeSet<&str>,
    options: &BundleOptions,
) -> UnitRender {
    let Some(class_name) = &entry.symbol else {
        return render_unit(index, entry, options);
    };
    let language = language_for(&entry.rel);
    let bytes = match read_bytes(entry, options) {
        Ok(bytes) => bytes,
        Err((note, code)) => {
            return UnitRender::omitted(format!("\n### {}\n\n{note}\n", entry.label()), code);
        }
    };
    let Some(position) = index.by_rel.get(&entry.rel).copied() else {
        return UnitRender::omitted(
            format!("\n### {}\n\n_(omitted: not indexed)_\n", entry.label()),
            "not_indexed",
        );
    };
    let Some(class) = index.files[position].symbol(class_name) else {
        return UnitRender::omitted(
            format!(
                "\n### {}\n\n_(omitted: `{class_name}` is no longer part of the file)_\n",
                entry.label()
            ),
            "symbol_missing",
        );
    };
    if class.start > class.end || class.end > bytes.len() {
        return UnitRender::omitted(
            format!(
                "\n### {}\n\n_(omitted: `{class_name}` moved while reading the file)_\n",
                entry.label()
            ),
            "stale_span",
        );
    }

    let line = line_number(&bytes, class.start);
    let mut markdown = format!("\n### `{}` :: `{class_name}` (line {line})\n\n", entry.rel);

    let mut text = String::from_utf8_lossy(&bytes[class.start..class.end]).to_string();
    let mut splices: Vec<(usize, usize, String)> = class
        .members
        .iter()
        .filter(|member| member.kind == MemberKind::Method)
        .filter(|member| !members.contains(member.name.as_str()))
        .filter_map(|member| {
            let body_start = member.body_start?;
            if body_start < class.start || member.end > class.end || body_start > member.end {
                return None;
            }
            let signature = String::from_utf8_lossy(&bytes[member.start..body_start])
                .trim_end()
                .to_string();
            let stub = if language == "python" {
                // `def x(self) -> int: ...` stays valid Python.
                format!("{signature} ...  # body omitted")
            } else {
                format!("{signature};   // body omitted")
            };
            Some((member.start - class.start, member.end - class.start, stub))
        })
        .collect();
    splices.sort_by_key(|(start, _, _)| *start);
    for (start, end, stub) in splices.into_iter().rev() {
        text.replace_range(start..end, &stub);
    }

    let preamble_end = index.files[position].preamble_end.min(class.start);
    let mut prefix = String::new();
    let mut open_braces = 0;
    if preamble_end > 0 {
        let (preamble, open) = preamble_text(&bytes, preamble_end);
        open_braces = open;
        prefix.push_str(&preamble);
        if !prefix.is_empty() {
            prefix.push_str("\n\n");
        }
    }
    for _ in 0..open_braces {
        text.push_str("\n}");
    }
    markdown.push_str(&render_text(&format!("{prefix}{text}"), language));

    let span = Span {
        start_byte: class.start,
        end_byte: class.end,
        start_line: line,
        end_line: class.end_line.max(line),
    };
    let hash = span_hash(&bytes, class.start, class.end);
    UnitRender::rendered(markdown, span, hash)
}

/// BLAKE3 fingerprint of a byte span, hex encoded. This is a TOCTOU check
/// between an agent reading a span and patching it, not a security boundary.
pub fn span_hash(bytes: &[u8], start: usize, end: usize) -> String {
    let end = end.min(bytes.len());
    let start = start.min(end);
    blake3::hash(&bytes[start..end]).to_hex().to_string()
}

fn render_text(content: &str, language: &str) -> String {
    let content = content.trim_end();
    let fence = fence_for(content);
    format!("{fence}{language}\n{content}\n{fence}\n")
}

/// The indexed symbol with this name, if it still exists. Its `start`/`end`
/// and `line`/`end_line` are the span the JSON surface reports.
fn symbol_span<'a>(index: &'a Index, file: &BundleFile, symbol: &str) -> Option<&'a Symbol> {
    let position = *index.by_rel.get(&file.rel)?;
    let entry = index.files.get(position)?;
    entry.symbol(symbol)
}

/// The file header (license, package, imports) that goes in front of an
/// excerpt, plus the number of braces it leaves open so the caller can close
/// them after the excerpt (block namespaces, braced packages).
fn preamble_text(bytes: &[u8], end: usize) -> (String, usize) {
    let text = String::from_utf8_lossy(&bytes[..end]);
    let text = text.trim_end();
    (text.to_string(), brace_balance(text))
}

/// Net `{` count, ignoring comments and string/char literals.
fn brace_balance(text: &str) -> usize {
    let mut depth = 0i32;
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '/' if chars.peek() == Some(&'/') => {
                for character in chars.by_ref() {
                    if character == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for character in chars.by_ref() {
                    if previous == '*' && character == '/' {
                        break;
                    }
                    previous = character;
                }
            }
            '"' | '\'' => {
                let quote = character;
                let mut escaped = false;
                for character in chars.by_ref() {
                    if escaped {
                        escaped = false;
                    } else if character == '\\' {
                        escaped = true;
                    } else if character == quote {
                        break;
                    }
                }
            }
            '{' => depth += 1,
            '}' => depth = (depth - 1).max(0),
            _ => {}
        }
    }
    depth as usize
}

fn line_number(bytes: &[u8], offset: usize) -> usize {
    bytes[..offset.min(bytes.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

/// 1-based line of the last byte in `0..end`; a trailing newline does not add
/// an extra line to a whole-file entry.
fn end_line_number(bytes: &[u8], end: usize) -> usize {
    line_number(bytes, end.saturating_sub(1))
}

/// Uses a longer fence when the file itself contains triple backticks.
fn fence_for(content: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for character in content.chars() {
        if character == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

fn language_for(rel: &str) -> &'static str {
    let fence = Language::fence_for(Path::new(rel));
    if !fence.is_empty() {
        return fence;
    }
    let extension = rel.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match extension {
        "xml" | "pom" => "xml",
        "yml" | "yaml" => "yaml",
        "properties" => "properties",
        "sql" => "sql",
        "json" => "json",
        "md" => "markdown",
        "sh" => "bash",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brace_balance_ignores_comments_and_strings() {
        assert_eq!(brace_balance("namespace A\n{\n"), 1);
        assert_eq!(brace_balance("// { comment\nclass A {}"), 0);
        assert_eq!(brace_balance("/* { */ class A {}"), 0);
        assert_eq!(brace_balance("string s = \"{\";"), 0);
        assert_eq!(brace_balance("one { two { three } }"), 0);
    }

    #[test]
    fn preamble_is_trimmed_and_reports_open_braces() {
        assert_eq!(
            preamble_text(b"package a;\n\n", 12),
            ("package a;".to_string(), 0)
        );
        assert_eq!(
            preamble_text(b"namespace A\n{\n   ", 17),
            ("namespace A\n{".to_string(), 1)
        );
    }

    /// Rebuilds the markdown from the JSON envelope the way a consumer would:
    /// header from top-level fields, one `## Files` line and one `## Contents`
    /// section per `entries` item.
    fn markdown_from_json(value: &serde_json::Value) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# Code bundle: {}\n\n",
            value["title"].as_str().expect("title")
        ));
        out.push_str(&format!(
            "- root: `{}`\n",
            value["root"].as_str().expect("root")
        ));
        out.push_str(&format!("- files: {}\n", value["files"]));
        let excerpts = value["excerpts"].as_u64().expect("excerpts");
        if excerpts > 0 {
            out.push_str(&format!("- excerpts: {excerpts}\n"));
        }
        let methods = value["methods"].as_u64().expect("methods");
        if methods > 0 {
            out.push_str(&format!("- methods: {methods}\n"));
        }
        out.push_str(&format!("- import depth: {}\n\n", value["depth"]));
        out.push_str("## Files\n\n");
        let entries = value["entries"].as_array().expect("entries");
        for entry in entries {
            out.push_str(&format!("- {}\n", entry["label"].as_str().expect("label")));
        }
        out.push_str("\n## Contents\n");
        for entry in entries {
            out.push_str(entry["markdown"].as_str().expect("markdown"));
        }
        if let Some(appendix) = value["state_toml"].as_str() {
            out.push_str("\n## Bundle state\n\n```toml\n");
            out.push_str(appendix.trim_end());
            out.push_str("\n```\n");
        }
        out
    }

    #[test]
    fn json_entries_reconstruct_the_markdown() {
        let root = std::env::temp_dir().join(format!("filer-bundle-json-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("Service.java"),
            "package app;\n\npublic class Service {\n    private Repo repo;\n\n    int used() { return repo.size(); }\n\n    int unused() { return 1; }\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("Repo.java"),
            "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n    public int other() { return 1; }\n}\n",
        )
        .unwrap();

        let index = Index::build(&root);
        let files = vec![
            BundleFile::members(
                "Service.java".to_string(),
                root.join("Service.java"),
                0,
                Reason::Seed,
                "Service".to_string(),
                BTreeSet::from(["used".to_string()]),
            ),
            BundleFile::excerpt(
                "Repo.java".to_string(),
                root.join("Repo.java"),
                1,
                Reason::Call,
                "Repo".to_string(),
            ),
        ];
        let options = BundleOptions {
            appendix: Some("version = 3\n".to_string()),
            ..Default::default()
        };
        let described = describe(&index, &files, &options);
        let json = described.to_json();

        assert_eq!(json["schema_version"], serde_json::json!(SCHEMA_VERSION));
        assert_eq!(
            json["entries"].as_array().unwrap().len(),
            described.entries.len()
        );
        assert_eq!(json["omitted"].as_array().unwrap().len(), 0);
        // The JSON surface and the markdown describe the same units, and the
        // markdown can be rebuilt from `entries` plus the envelope fields.
        assert_eq!(markdown_from_json(&json), described.markdown);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn line_number_is_one_based() {
        let bytes = b"a\nb\nc";
        assert_eq!(line_number(bytes, 0), 1);
        assert_eq!(line_number(bytes, 2), 2);
        assert_eq!(line_number(bytes, 4), 3);
    }

    #[test]
    fn member_group_keeps_the_shell_and_stubs_uncalled_methods() {
        let root =
            std::env::temp_dir().join(format!("filer-bundle-members-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("Service.java"),
            "package app;\n\nimport app.Repo;\n\npublic class Service {\n    private Repo repo;\n\n    public Service(Repo repo) { this.repo = repo; }\n\n    int used() { return repo.size(); }\n\n    int unused() { return 1; }\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("Repo.java"),
            "package app;\n\npublic class Repo {\n    public int size() { return 0; }\n    public int other() { return 1; }\n}\n",
        )
        .unwrap();

        let index = Index::build(&root);
        let files = vec![
            BundleFile::members(
                "Service.java".to_string(),
                root.join("Service.java"),
                0,
                Reason::Seed,
                "Service".to_string(),
                BTreeSet::from(["used".to_string()]),
            ),
            BundleFile::members(
                "Repo.java".to_string(),
                root.join("Repo.java"),
                1,
                Reason::Call,
                "Repo".to_string(),
                BTreeSet::from(["size".to_string()]),
            ),
        ];
        let markdown = describe(&index, &files, &BundleOptions::default()).markdown;
        assert!(
            markdown.contains("### `Service.java` :: `Service` (line 5)"),
            "{markdown}"
        );
        assert!(
            markdown.contains("int used() { return repo.size(); }"),
            "{markdown}"
        );
        assert!(
            markdown.contains("int unused();   // body omitted"),
            "{markdown}"
        );
        assert!(
            markdown.contains("public Service(Repo repo) { this.repo = repo; }"),
            "{markdown}"
        );
        assert!(
            markdown.contains("int other();   // body omitted"),
            "{markdown}"
        );
        assert!(
            !markdown.contains("int unused() { return 1; }"),
            "{markdown}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_whole_file_supersedes_its_excerpts() {
        let root = std::env::temp_dir().join(format!("filer-bundle-cover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("Service.java"),
            "package app;\n\npublic class Service {\n    int used() { return 1; }\n    int other() { return 2; }\n}\n",
        )
        .unwrap();

        let index = Index::build(&root);
        let files = vec![
            BundleFile::whole(
                "Service.java".to_string(),
                root.join("Service.java"),
                0,
                Reason::Seed,
            ),
            BundleFile::excerpt(
                "Service.java".to_string(),
                root.join("Service.java"),
                1,
                Reason::Call,
                "Service".to_string(),
            ),
            BundleFile::members(
                "Service.java".to_string(),
                root.join("Service.java"),
                1,
                Reason::Call,
                "Service".to_string(),
                BTreeSet::from(["used".to_string()]),
            ),
        ];
        let markdown = describe(&index, &files, &BundleOptions::default()).markdown;
        // One file line, one content block, and the whole class text.
        assert_eq!(
            markdown.matches("- `Service.java`").count(),
            1,
            "{markdown}"
        );
        assert!(markdown.contains("- files: 1"), "{markdown}");
        assert!(!markdown.contains("excerpts:"), "{markdown}");
        assert!(markdown.contains("int other() { return 2; }"), "{markdown}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fence_grows_for_embedded_backticks() {
        assert_eq!(fence_for("plain"), "```");
        assert_eq!(fence_for("a ``` b"), "````");
        assert_eq!(fence_for("a ```` b"), "`````");
    }

    /// One class per file, so a test can point a [`BundleFile`] at any of them
    /// and `describe` has real spans to order on.
    fn ordering_project(name: &str) -> (std::path::PathBuf, Index) {
        let root =
            std::env::temp_dir().join(format!("filer-bundle-order-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for (file, class) in [
            ("Service.java", "Service"),
            ("Repo.java", "Repo"),
            ("Alpha.java", "Alpha"),
            ("Beta.java", "Beta"),
            ("Gamma.java", "Gamma"),
        ] {
            fs::write(
                root.join(file),
                format!(
                    "package app;\n\npublic class {class} {{\n    int used() {{ return 0; }}\n    int other() {{ return 1; }}\n}}\n"
                ),
            )
            .unwrap();
        }
        let index = Index::build(&root);
        (root, index)
    }

    /// The rendered source span of every entry, in the envelope's order.
    fn spans(rendered: &RenderedBundle) -> Vec<u64> {
        rendered
            .entries
            .iter()
            .map(|entry| match (entry.start_byte, entry.end_byte) {
                (Some(start), Some(end)) => (end.saturating_sub(start)) as u64,
                _ => 0,
            })
            .collect()
    }

    fn paths(rendered: &RenderedBundle) -> Vec<String> {
        rendered
            .entries
            .iter()
            .map(|entry| format!("{} {}", entry.scope.as_str(), entry.path))
            .collect()
    }

    #[test]
    fn the_canonical_order_is_scope_then_depth_then_reason() {
        let (root, index) = ordering_project("canonical");
        let files = vec![
            BundleFile::members(
                "Service.java".to_string(),
                root.join("Service.java"),
                0,
                Reason::Seed,
                "Service".to_string(),
                BTreeSet::from(["used".to_string()]),
            ),
            BundleFile::excerpt(
                "Beta.java".to_string(),
                root.join("Beta.java"),
                1,
                Reason::Call,
                "Beta".to_string(),
            ),
            BundleFile::whole(
                "Alpha.java".to_string(),
                root.join("Alpha.java"),
                2,
                Reason::Import,
            ),
            BundleFile::whole(
                "Repo.java".to_string(),
                root.join("Repo.java"),
                1,
                Reason::Call,
            ),
            BundleFile::members(
                "Gamma.java".to_string(),
                root.join("Gamma.java"),
                1,
                Reason::Call,
                "Gamma".to_string(),
                BTreeSet::from(["used".to_string()]),
            ),
        ];
        let rendered = describe(&index, &files, &BundleOptions::default());
        assert_eq!(
            paths(&rendered),
            [
                "methods Service.java",
                "file Repo.java",
                "file Alpha.java",
                "symbol Beta.java",
                "methods Gamma.java",
            ]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn seeds_keep_the_order_they_were_given() {
        let (root, index) = ordering_project("seeds");
        let files = vec![
            BundleFile::whole(
                "Repo.java".to_string(),
                root.join("Repo.java"),
                0,
                Reason::Seed,
            ),
            BundleFile::whole(
                "Alpha.java".to_string(),
                root.join("Alpha.java"),
                0,
                Reason::Seed,
            ),
        ];
        let rendered = describe(&index, &files, &BundleOptions::default());
        assert_eq!(paths(&rendered), ["file Repo.java", "file Alpha.java"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_budget_cuts_the_tail_and_lists_what_it_dropped() {
        let (root, index) = ordering_project("budget");
        let files = vec![
            BundleFile::whole(
                "Repo.java".to_string(),
                root.join("Repo.java"),
                0,
                Reason::Seed,
            ),
            BundleFile::excerpt(
                "Alpha.java".to_string(),
                root.join("Alpha.java"),
                1,
                Reason::Call,
                "Alpha".to_string(),
            ),
            BundleFile::excerpt(
                "Beta.java".to_string(),
                root.join("Beta.java"),
                1,
                Reason::Call,
                "Beta".to_string(),
            ),
            BundleFile::excerpt(
                "Gamma.java".to_string(),
                root.join("Gamma.java"),
                1,
                Reason::Call,
                "Gamma".to_string(),
            ),
        ];
        let full = describe(&index, &files, &BundleOptions::default());
        assert_eq!(full.entries.len(), 4);
        assert!(full.dropped.is_empty());

        // Room for the seed and the first excerpt, but not for the second.
        let budget = spans(&full)[0] + spans(&full)[1];
        let options = BundleOptions {
            max_bytes: Some(budget),
            ..Default::default()
        };
        let cut = describe(&index, &files, &options);
        assert_eq!(paths(&cut), ["file Repo.java", "symbol Alpha.java"]);
        assert_eq!(cut.dropped.len(), 2);
        assert!(!cut.markdown.contains("Beta.java"), "{}", cut.markdown);

        let json = cut.to_json();
        assert_eq!(
            json["budget"]["max_tokens"],
            serde_json::json!(budget as usize / BYTES_PER_TOKEN)
        );
        assert_eq!(json["budget"]["truncated"], serde_json::json!(true));
        let used: usize = cut.entries.iter().map(|entry| entry.tokens).sum();
        assert_eq!(json["budget"]["used_tokens"], serde_json::json!(used));
        let omitted = json["omitted"].as_array().expect("omitted");
        assert_eq!(omitted.len(), 2);
        assert_eq!(omitted[0]["path"], serde_json::json!("Beta.java"));
        assert_eq!(omitted[1]["path"], serde_json::json!("Gamma.java"));
        assert!(omitted.iter().all(|row| row["reason"] == "budget"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn shrinking_the_budget_only_removes_entries_from_the_tail() {
        let (root, index) = ordering_project("shrink");
        let files = vec![
            BundleFile::whole(
                "Repo.java".to_string(),
                root.join("Repo.java"),
                0,
                Reason::Seed,
            ),
            BundleFile::excerpt(
                "Alpha.java".to_string(),
                root.join("Alpha.java"),
                1,
                Reason::Call,
                "Alpha".to_string(),
            ),
            BundleFile::excerpt(
                "Beta.java".to_string(),
                root.join("Beta.java"),
                1,
                Reason::Call,
                "Beta".to_string(),
            ),
        ];
        let full = describe(&index, &files, &BundleOptions::default());
        let baseline = paths(&full);
        let total: u64 = spans(&full).iter().sum();
        for budget in 0..=total {
            let options = BundleOptions {
                max_bytes: Some(budget),
                ..Default::default()
            };
            let cut = describe(&index, &files, &options);
            let kept = paths(&cut);
            assert_eq!(kept, baseline[..kept.len()], "budget of {budget} bytes");
            assert_eq!(kept.len() + cut.dropped.len(), baseline.len());
            assert_eq!(
                cut.to_json()["budget"]["truncated"],
                !cut.dropped.is_empty()
            );
        }
        let _ = fs::remove_dir_all(&root);
    }
}
