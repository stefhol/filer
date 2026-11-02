//! Language detection and the language-neutral parse result.
//!
//! Everything downstream of parsing (`index`, `resolve`, `bundle`) works on
//! [`Source`], so adding a language means adding a parser module and an arm in
//! [`Language::from_path`] — no changes to resolution or rendering.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::java::JavaParser;
use crate::scala::ScalaParser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Java,
    Scala,
}

impl Language {
    /// Language for a source file, or `None` when the extension is not indexed.
    ///
    /// `.sbt` (build definitions) and `.sc` (Scala scripts, Scala CLI) are
    /// parsed as Scala: they are ordinary Scala syntax, just without a package.
    pub fn from_path(path: &Path) -> Option<Language> {
        match path.extension()?.to_str()? {
            "java" => Some(Language::Java),
            "scala" | "sbt" | "sc" => Some(Language::Scala),
            _ => None,
        }
    }

    /// Fence info string used in the rendered markdown.
    pub fn fence(self) -> &'static str {
        match self {
            Language::Java => "java",
            Language::Scala => "scala",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Language::Java => "java",
            Language::Scala => "scala",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    /// `import a.b.C;` / `import a.b.C`
    Type,
    /// Java's `import static a.b.C.member;`
    Static,
    /// `import a.b.*;` / `import a.b._` / Scala selector wildcards
    Wildcard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    /// Dotted name as written in the source, without `static`, `_`, `*` or
    /// selector braces. Scala selectors are expanded to one import per name.
    pub path: String,
    pub kind: ImportKind,
}

impl Import {
    pub fn is_wildcard(&self) -> bool {
        self.kind == ImportKind::Wildcard
    }
}

/// What the index needs from a parsed file, independent of its language.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Source {
    pub package: Option<String>,
    /// Top-level type declarations (classes, interfaces, enums, records,
    /// objects, traits, type aliases).
    pub types: Vec<String>,
    pub imports: Vec<Import>,
    /// Type names referenced anywhere in the file (`DemoDocument.class`,
    /// generic arguments, field types, `Object.member` receivers, ...).
    /// Catches same-package references, which need no import.
    pub uses: Vec<String>,
}

/// Parses source files of any supported language. Holds one tree-sitter parser
/// per grammar; callers create one per thread and reuse it for many files.
pub struct SourceParser {
    java: JavaParser,
    scala: ScalaParser,
}

impl SourceParser {
    pub fn new() -> Self {
        Self {
            java: JavaParser::new(),
            scala: ScalaParser::new(),
        }
    }

    /// Returns `None` only when the parser bails out (invalid encoding or
    /// cancelled parse). Files with syntax errors still yield whatever
    /// declarations tree-sitter could recover.
    pub fn parse(&mut self, language: Language, src: &str) -> Option<Source> {
        match language {
            Language::Java => self.java.parse(src),
            Language::Scala => self.scala.parse(src),
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
    fn parses_both_languages_through_one_parser() {
        let mut parser = SourceParser::new();
        let java = parser
            .parse(Language::Java, "package a; class A {}")
            .unwrap();
        assert_eq!(java.package.as_deref(), Some("a"));
        assert_eq!(java.types, vec!["A"]);

        let scala = parser.parse(Language::Scala, "package b\nclass B").unwrap();
        assert_eq!(scala.package.as_deref(), Some("b"));
        assert_eq!(scala.types, vec!["B"]);

        // Switching back works too.
        let java = parser.parse(Language::Java, "class C {}").unwrap();
        assert_eq!(java.types, vec!["C"]);
    }
}
