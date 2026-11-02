//! Language detection and the language-neutral parse result.
//!
//! Everything downstream of parsing (`index`, `resolve`, `bundle`) works on
//! [`Source`], so adding a language means adding a parser module and an arm in
//! [`Language::from_path`] — no changes to resolution or rendering.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::csharp::CSharpParser;
use crate::fe::FeParser;
use crate::groovy::GroovyParser;
use crate::java::JavaParser;
use crate::kotlin::KotlinParser;
use crate::scala::ScalaParser;
use crate::web::WebParser;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Java,
    Scala,
    Kotlin,
    Groovy,
    CSharp,
    TypeScript,
    JavaScript,
    Vue,
    /// Asset Control Formula Engine code (`.fe`).
    Fe,
}

/// How finely a language can be bundled and picked.
///
/// Every language has whole files (except Formula Engine, where one file can
/// hold thousands of functions). Only languages with a call graph expose
/// methods, and only Java does in this cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Granularity {
    /// Whole files only (Vue, files without symbols).
    File,
    /// `[file]` plus one row per top-level symbol.
    Symbol,
    /// One row per top-level function, no `[file]` row (Formula Engine).
    Function,
    /// `[file]`, `[class]` and `[method]` rows, with a call graph (Java).
    Member,
}

impl Granularity {
    /// Whether the picker offers a whole-file row for this language.
    pub fn file_row(self) -> bool {
        self != Granularity::Function
    }

    /// Whether top-level symbols are selectable excerpts.
    pub fn symbol_rows(self) -> bool {
        self != Granularity::File
    }
}

/// Languages that share an import model and may resolve each other's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    /// Package + `Type` imports (Java, Scala, Kotlin, Groovy, C#).
    Jvm,
    /// Module specifiers and relative paths (TypeScript, JavaScript, Vue).
    Web,
    /// One global namespace, no imports at all (Asset Control Formula Engine).
    Fe,
}

impl Language {
    /// Every language the index understands, for stats and docs.
    pub const ALL: [Language; 9] = [
        Language::Java,
        Language::Scala,
        Language::Kotlin,
        Language::Groovy,
        Language::CSharp,
        Language::TypeScript,
        Language::JavaScript,
        Language::Vue,
        Language::Fe,
    ];

    /// Language for a source file, or `None` when the extension is not indexed.
    ///
    /// `.sbt` (sbt build definitions) and `.sc` (Scala scripts) are Scala;
    /// `.gradle` files are Groovy; `.vue` files are Vue single-file components.
    pub fn from_path(path: &Path) -> Option<Language> {
        match path.extension()?.to_str()? {
            "java" => Some(Language::Java),
            "scala" | "sbt" | "sc" => Some(Language::Scala),
            "kt" | "kts" => Some(Language::Kotlin),
            "groovy" | "gradle" => Some(Language::Groovy),
            "cs" => Some(Language::CSharp),
            "ts" | "tsx" => Some(Language::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "vue" => Some(Language::Vue),
            "fe" => Some(Language::Fe),
            _ => None,
        }
    }

    /// Bundling granularity this language supports. See [`Granularity`].
    pub fn granularity(self) -> Granularity {
        match self {
            Language::Fe => Granularity::Function,
            Language::Java | Language::Kotlin => Granularity::Member,
            Language::Vue => Granularity::File,
            _ => Granularity::Symbol,
        }
    }

    /// Imports and name inference only cross files inside the same family.
    pub fn family(self) -> Family {
        match self {
            Language::TypeScript | Language::JavaScript | Language::Vue => Family::Web,
            Language::Fe => Family::Fe,
            _ => Family::Jvm,
        }
    }

    /// Fence info string used in the rendered markdown.
    pub fn fence(self) -> &'static str {
        match self {
            Language::Java => "java",
            Language::Scala => "scala",
            Language::Kotlin => "kotlin",
            Language::Groovy => "groovy",
            Language::CSharp => "csharp",
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Vue => "vue",
            Language::Fe => "fe",
        }
    }

    /// Fence for a specific extension (TSX/JSX differ from their base language).
    pub fn fence_for(path: &Path) -> &'static str {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("tsx") => "tsx",
            Some("jsx") => "jsx",
            Some("mjs" | "cjs") => "javascript",
            _ => Language::from_path(path).map(Language::fence).unwrap_or(""),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Language::Java => "java",
            Language::Scala => "scala",
            Language::Kotlin => "kotlin",
            Language::Groovy => "groovy",
            Language::CSharp => "csharp",
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Vue => "vue",
            Language::Fe => "fe",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    /// `import a.b.C;` / `import a.b.C` / `using static a.b.C;`
    Type,
    /// Java's `import static a.b.C.member;`
    Static,
    /// `import a.b.*;` / `import a.b._` / Scala selector wildcards /
    /// C#'s `using Some.Namespace;`
    Wildcard,
    /// A TypeScript/JavaScript module specifier (`./x`, `../y.js`, `react`).
    Module,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    /// Dotted name as written in the source, or the raw module specifier for
    /// [`ImportKind::Module`]. Selector imports are expanded to one per name.
    pub path: String,
    pub kind: ImportKind,
}

impl Import {
    pub fn is_wildcard(&self) -> bool {
        self.kind == ImportKind::Wildcard
    }
}

/// What kind of thing a [`Symbol`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// A Formula Engine function.
    Function,
    /// A class/interface/enum/record/object/trait/struct/type alias.
    Type,
    /// A top-level Formula Engine variable declaration (`global dict x = ...`).
    Declaration,
}

/// What kind of member a [`Member`] is. Only methods are prunable; constructors
/// and fields are always kept as class context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    Method,
    Constructor,
}

/// How a method invocation names its receiver.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Receiver {
    /// `helper()` — resolve in the enclosing class, supertypes, static imports.
    #[default]
    Bare,
    /// `this.helper()`.
    This,
    /// `super.helper()`.
    Super,
    /// `repo.find()`, `Helpers.create()` — a local, parameter, field or type.
    Name(String),
    /// `a.b().c()` and anything else we cannot type without real inference.
    Unknown,
}

/// One call site inside a member body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub name: String,
    #[serde(default)]
    pub receiver: Receiver,
}

/// A prunable member of a top-level type (currently Java methods/constructors).
/// Fields and other members stay implicit in the type's own byte span.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
    /// Byte offset of the first byte (doc comment and annotations included).
    pub start: usize,
    pub end: usize,
    pub line: usize,
    /// Offset of the `{` that opens the body; `None` for abstract/interface
    /// methods (there is no body to elide).
    #[serde(default)]
    pub body_start: Option<usize>,
    /// Parameter name -> simple type name.
    #[serde(default)]
    pub params: Vec<(String, String)>,
    /// Local variable name -> simple type name.
    #[serde(default)]
    pub locals: Vec<(String, String)>,
    #[serde(default)]
    pub calls: Vec<Call>,
}

/// One bundlable excerpt of a file: a top-level declaration with its byte span.
///
/// File-level bundling stays the default; a symbol is what `path::Name` and the
/// picker select when you only want one 20 000-line file's function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Byte offset of the first byte of the excerpt (leading doc comment included).
    pub start: usize,
    /// Byte offset one past the last byte.
    pub end: usize,
    /// 1-based line of `start`, cached so the picker needs no file reads.
    #[serde(default)]
    pub line: usize,
    /// Names referenced inside the symbol, deduplicated and sorted.
    #[serde(default)]
    pub uses: Vec<String>,
    /// Function names invoked from inside the symbol (Formula Engine).
    #[serde(default)]
    pub calls: Vec<String>,
    /// Prunable members (Java methods/constructors) for call-path pruning.
    #[serde(default)]
    pub members: Vec<Member>,
    /// Field name -> simple type name, for receiver typing.
    #[serde(default)]
    pub fields: Vec<(String, String)>,
    /// Simple names from `extends` / `implements`.
    #[serde(default)]
    pub supertypes: Vec<String>,
}

impl Symbol {
    pub fn contains(&self, offset: usize) -> bool {
        (self.start..self.end).contains(&offset)
    }
}

/// What the index needs from a parsed file, independent of its language.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Source {
    pub package: Option<String>,
    /// Top-level type declarations (classes, interfaces, enums, records,
    /// objects, traits, structs, type aliases).
    pub types: Vec<String>,
    pub imports: Vec<Import>,
    /// Type names referenced anywhere in the file (`DemoDocument.class`,
    /// generic arguments, field types, `Object.member` receivers, ...).
    /// Catches same-package references, which need no import. Module languages
    /// leave this empty and rely on explicit imports.
    pub uses: Vec<String>,
    /// Top-level declarations in source order, with byte spans. Empty for
    /// languages whose files are the natural bundling unit.
    #[serde(default)]
    pub symbols: Vec<Symbol>,
    /// End of the file header (package/imports/license) — the first symbol's
    /// start. Repeated in front of each symbol excerpt so it stands alone.
    #[serde(default)]
    pub preamble_end: usize,
}

/// Assigns each `(name, byte offset)` use to the symbol that contains it.
/// Uses outside every symbol (imports, package clause) are dropped.
pub fn attribute_uses(uses: &[(String, usize)], symbols: &[Symbol]) -> Vec<Vec<String>> {
    let mut buckets = vec![Vec::new(); symbols.len()];
    for (name, offset) in uses {
        if let Some(index) = symbols.iter().position(|symbol| symbol.contains(*offset)) {
            buckets[index].push(name.clone());
        }
    }
    for bucket in &mut buckets {
        bucket.sort();
        bucket.dedup();
    }
    buckets
}

/// Builds the [`Symbol`] for one top-level declaration node.
pub fn symbol_from_node(
    node: tree_sitter::Node<'_>,
    name: &str,
    kind: SymbolKind,
    comment_kinds: &[&str],
    src: &str,
) -> Symbol {
    let start = leading_comment_start(node, comment_kinds);
    Symbol {
        name: name.to_string(),
        kind,
        start,
        end: node.end_byte(),
        line: line_of(src, start),
        uses: Vec::new(),
        calls: Vec::new(),
        members: Vec::new(),
        fields: Vec::new(),
        supertypes: Vec::new(),
    }
}

/// 1-based line of a byte offset.
pub fn line_of(src: &str, offset: usize) -> usize {
    src.as_bytes()[..offset.min(src.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

/// Assigns `(name, offset)` uses to the symbols that contain them and fills in
/// the file-level fields derived from them (`uses`, `preamble_end`).
pub fn finish_symbols(out: &mut Source, uses: &[(String, usize)]) {
    let mut names: Vec<String> = uses.iter().map(|(name, _)| name.clone()).collect();
    names.sort();
    names.dedup();
    let buckets = attribute_uses(uses, &out.symbols);
    for (symbol, bucket) in out.symbols.iter_mut().zip(buckets) {
        symbol.uses = bucket;
    }
    out.uses = names;
    out.preamble_end = out
        .symbols
        .iter()
        .map(|symbol| symbol.start)
        .min()
        .unwrap_or(0);
}

/// Start of a declaration, walking backwards over contiguous comment siblings
/// so an excerpt carries its doc comment.
pub fn leading_comment_start(node: tree_sitter::Node<'_>, comment_kinds: &[&str]) -> usize {
    let mut start = node.start_byte();
    let mut current = node;
    while let Some(previous) = current.prev_named_sibling() {
        if !comment_kinds.contains(&previous.kind()) {
            break;
        }
        start = previous.start_byte();
        current = previous;
    }
    start
}

/// Parses source files of any supported language. Holds one tree-sitter parser
/// per grammar; callers create one per thread and reuse it for many files.
pub struct SourceParser {
    java: JavaParser,
    scala: ScalaParser,
    kotlin: KotlinParser,
    groovy: GroovyParser,
    csharp: CSharpParser,
    web: WebParser,
    fe: FeParser,
}

impl SourceParser {
    pub fn new() -> Self {
        Self {
            java: JavaParser::new(),
            scala: ScalaParser::new(),
            kotlin: KotlinParser::new(),
            groovy: GroovyParser::new(),
            csharp: CSharpParser::new(),
            web: WebParser::new(),
            fe: FeParser::new(),
        }
    }

    /// Returns `None` only when the parser bails out (invalid encoding or
    /// cancelled parse). Files with syntax errors still yield whatever
    /// declarations tree-sitter could recover.
    pub fn parse(&mut self, language: Language, src: &str) -> Option<Source> {
        match language {
            Language::Java => self.java.parse(src),
            Language::Scala => self.scala.parse(src),
            Language::Kotlin => self.kotlin.parse(src),
            Language::Groovy => self.groovy.parse(src),
            Language::CSharp => self.csharp.parse(src),
            Language::Fe => self.fe.parse(src),
            Language::TypeScript | Language::JavaScript | Language::Vue => {
                self.web.parse(language, src)
            }
        }
    }
}

impl Default for SourceParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_languages_by_extension() {
        let cases = [
            ("App.java", Some(Language::Java)),
            ("App.scala", Some(Language::Scala)),
            ("build.sbt", Some(Language::Scala)),
            ("script.sc", Some(Language::Scala)),
            ("App.kt", Some(Language::Kotlin)),
            ("build.gradle.kts", Some(Language::Kotlin)),
            ("build.gradle", Some(Language::Groovy)),
            ("App.cs", Some(Language::CSharp)),
            ("App.ts", Some(Language::TypeScript)),
            ("App.tsx", Some(Language::TypeScript)),
            ("App.js", Some(Language::JavaScript)),
            ("App.jsx", Some(Language::JavaScript)),
            ("App.vue", Some(Language::Vue)),
            ("Risk.fe", Some(Language::Fe)),
            ("README.md", None),
            ("Makefile", None),
        ];
        for (name, expected) in cases {
            assert_eq!(
                Language::from_path(&PathBuf::from(name)),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn granularity_per_language() {
        assert_eq!(Language::Java.granularity(), Granularity::Member);
        assert_eq!(Language::Fe.granularity(), Granularity::Function);
        assert_eq!(Language::Vue.granularity(), Granularity::File);
        assert_eq!(Language::Kotlin.granularity(), Granularity::Member);
        assert_eq!(Language::Scala.granularity(), Granularity::Symbol);
        assert_eq!(Language::TypeScript.granularity(), Granularity::Symbol);
        assert!(!Language::Fe.granularity().file_row());
        assert!(Language::Fe.granularity().symbol_rows());
        assert!(Language::Java.granularity().symbol_rows());
        assert!(!Language::Vue.granularity().symbol_rows());
    }

    #[test]
    fn families_are_isolated() {
        assert_eq!(Language::Kotlin.family(), Family::Jvm);
        assert_eq!(Language::CSharp.family(), Family::Jvm);
        assert_eq!(Language::TypeScript.family(), Family::Web);
        assert_eq!(Language::Vue.family(), Family::Web);
        assert_eq!(Language::Fe.family(), Family::Fe);
    }

    #[test]
    fn extension_specific_fences() {
        assert_eq!(Language::fence_for(Path::new("A.ts")), "typescript");
        assert_eq!(Language::fence_for(Path::new("A.tsx")), "tsx");
        assert_eq!(Language::fence_for(Path::new("A.jsx")), "jsx");
        assert_eq!(Language::fence_for(Path::new("A.cjs")), "javascript");
        assert_eq!(Language::fence_for(Path::new("A.cs")), "csharp");
    }

    #[test]
    fn parses_every_language_through_one_parser() {
        let mut parser = SourceParser::new();
        let cases = [
            (Language::Java, "package a; class A {}"),
            (Language::Scala, "package b\nclass B"),
            (Language::Kotlin, "package c\nclass C"),
            (Language::Groovy, "package d\nclass D {}"),
            (Language::CSharp, "namespace e; class E {}"),
            (Language::TypeScript, "export class F {}"),
            (Language::JavaScript, "export class G {}"),
            (Language::Fe, "function g() { return 1; }"),
        ];
        for (language, source) in cases {
            let parsed = parser.parse(language, source).expect("parsed");
            assert!(!parsed.types.is_empty(), "{language:?}: {parsed:?}");
        }
    }
}
