//! Markdown rendering of a code bundle that can be pasted into a chat.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::Index;
use crate::lang::{Language, MemberKind};
use crate::resolve::Reason;
use crate::state::Selection;

pub struct BundleOptions {
    pub title: Option<String>,
    pub depth: u32,
    pub max_file_bytes: u64,
    /// Machine-readable TOML listing the bundled files, appended to the bundle.
    pub appendix: Option<String>,
}

impl Default for BundleOptions {
    fn default() -> Self {
        Self {
            title: None,
            depth: 2,
            max_file_bytes: 1024 * 1024,
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

    fn methods_kept(&self) -> usize {
        match self {
            Content::Single(_) => 0,
            Content::Class { members, .. } => members.len(),
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

pub fn render(index: &Index, files: &[BundleFile], options: &BundleOptions) -> String {
    let name = index
        .root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    let mut out = String::new();
    let title = options.title.clone().unwrap_or_else(|| name.clone());
    out.push_str(&format!("# Code bundle: {title}\n\n"));
    out.push_str(&format!("- root: `{}`\n", index.root.to_string_lossy()));
    let contents = group(files);
    let distinct: HashSet<&str> = contents
        .iter()
        .map(|content| content.entry().rel.as_str())
        .collect();
    out.push_str(&format!("- files: {}\n", distinct.len()));
    let excerpts = contents
        .iter()
        .filter(|content| content.entry().scope != Selection::File)
        .count();
    if excerpts > 0 {
        out.push_str(&format!("- excerpts: {excerpts}\n"));
    }
    let methods_kept: usize = contents.iter().map(Content::methods_kept).sum();
    if methods_kept > 0 {
        out.push_str(&format!("- methods: {methods_kept}\n"));
    }
    out.push_str(&format!("- import depth: {}\n\n", options.depth));

    out.push_str("## Files\n\n");
    for content in &contents {
        let file = content.entry();
        let detail = match (file.depth, file.reason) {
            (0, Reason::Seed) => String::new(),
            (0, reason) => format!(" ({})", reason.as_str()),
            (depth, reason) => format!(" (depth {depth}, {})", reason.as_str()),
        };
        out.push_str(&format!("- {}{detail}\n", content.label()));
    }
    out.push_str("\n## Contents\n");

    for content in &contents {
        match content {
            Content::Single(file) => render_unit(&mut out, index, file, options),
            Content::Class { entry, members } => {
                render_member_group(&mut out, index, entry, members, options);
            }
        }
    }

    if let Some(appendix) = &options.appendix {
        out.push_str("\n## Bundle state\n\n");
        out.push_str("```toml\n");
        out.push_str(appendix.trim_end());
        out.push_str("\n```\n");
    }

    out
}

fn render_unit(out: &mut String, index: &Index, file: &BundleFile, options: &BundleOptions) {
    let language = language_for(&file.rel);
    let bytes = match read_bytes(file, options) {
        Ok(bytes) => bytes,
        Err(note) => {
            out.push_str(&format!("\n### {}\n\n{note}\n", file.label()));
            return;
        }
    };

    let Some(symbol) = file
        .symbol
        .as_ref()
        .filter(|_| file.scope != Selection::File)
    else {
        out.push_str(&format!("\n### {}\n\n", file.label()));
        out.push_str(&render_text(&String::from_utf8_lossy(&bytes), language));
        return;
    };

    let Some(span) = symbol_span(index, file, symbol) else {
        // The symbol moved or disappeared since the state was written.
        out.push_str(&format!(
            "\n### {}\n\n_(omitted: `{symbol}` is no longer part of the file)_\n",
            file.label()
        ));
        return;
    };

    if span.0 > span.1 || span.1 > bytes.len() {
        out.push_str(&format!(
            "\n### {}\n\n_(omitted: `{symbol}` moved while reading the file)_\n",
            file.label()
        ));
        return;
    }
    let line = line_number(&bytes, span.0);
    out.push_str(&format!("\n### {} (line {line})\n\n", file.label()));

    let preamble_end = index
        .by_rel
        .get(&file.rel)
        .map(|&position| index.files[position].preamble_end)
        .unwrap_or(0)
        .min(span.0);
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
    text.push_str(&String::from_utf8_lossy(&bytes[span.0..span.1]));
    for _ in 0..open_braces {
        text.push_str("\n}");
    }
    out.push_str(&render_text(&text, language));
}

fn read_bytes(file: &BundleFile, options: &BundleOptions) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(&file.abs).map_err(|error| format!("_(unreadable: {error})_"))?;
    if metadata.len() > options.max_file_bytes {
        return Err(format!(
            "_(omitted: {} bytes, larger than the {} byte limit)_",
            metadata.len(),
            options.max_file_bytes
        ));
    }
    fs::read(&file.abs).map_err(|error| format!("_(unreadable: {error})_"))
}

/// Renders a class shell with only `members` kept; every other method body is
/// replaced by its signature plus `// body omitted`.
fn render_member_group(
    out: &mut String,
    index: &Index,
    entry: &BundleFile,
    members: &BTreeSet<&str>,
    options: &BundleOptions,
) {
    let Some(class_name) = &entry.symbol else {
        render_unit(out, index, entry, options);
        return;
    };
    let language = language_for(&entry.rel);
    let bytes = match read_bytes(entry, options) {
        Ok(bytes) => bytes,
        Err(note) => {
            out.push_str(&format!("\n### {}\n\n{note}\n", entry.label()));
            return;
        }
    };
    let Some(position) = index.by_rel.get(&entry.rel).copied() else {
        out.push_str(&format!(
            "\n### {}\n\n_(omitted: not indexed)_\n",
            entry.label()
        ));
        return;
    };
    let Some(class) = index.files[position].symbol(class_name) else {
        out.push_str(&format!(
            "\n### {}\n\n_(omitted: `{class_name}` is no longer part of the file)_\n",
            entry.label()
        ));
        return;
    };
    if class.start > class.end || class.end > bytes.len() {
        out.push_str(&format!(
            "\n### {}\n\n_(omitted: `{class_name}` moved while reading the file)_\n",
            entry.label()
        ));
        return;
    }

    let line = line_number(&bytes, class.start);
    out.push_str(&format!(
        "\n### `{}` :: `{class_name}` (line {line})\n\n",
        entry.rel
    ));

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
            Some((
                member.start - class.start,
                member.end - class.start,
                format!("{signature};   // body omitted"),
            ))
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
    out.push_str(&render_text(&format!("{prefix}{text}"), language));
}

fn render_text(content: &str, language: &str) -> String {
    let content = content.trim_end();
    let fence = fence_for(content);
    format!("{fence}{language}\n{content}\n{fence}\n")
}

/// Byte span of `symbol` in the indexed file, if it still exists.
fn symbol_span(index: &Index, file: &BundleFile, symbol: &str) -> Option<(usize, usize)> {
    let position = *index.by_rel.get(&file.rel)?;
    let entry = index.files.get(position)?;
    entry
        .symbol(symbol)
        .map(|symbol| (symbol.start, symbol.end))
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
        let markdown = render(&index, &files, &BundleOptions::default());
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
        let markdown = render(&index, &files, &BundleOptions::default());
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
}
