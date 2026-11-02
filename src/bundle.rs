//! Markdown rendering of a code bundle that can be pasted into a chat.

use std::fs;
use std::path::{Path, PathBuf};

use crate::index::Index;
use crate::lang::Language;
use crate::resolve::Reason;

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

pub struct BundleFile {
    pub rel: String,
    pub abs: PathBuf,
    pub depth: u32,
    /// Why the file is in the bundle; shown in the file list.
    pub reason: Reason,
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
    out.push_str(&format!("- files: {}\n", files.len()));
    out.push_str(&format!("- import depth: {}\n\n", options.depth));

    out.push_str("## Files\n\n");
    for file in files {
        let detail = match (file.depth, file.reason) {
            (0, Reason::Seed) => String::new(),
            (0, reason) => format!(" ({})", reason.as_str()),
            (depth, reason) => format!(" (depth {depth}, {})", reason.as_str()),
        };
        out.push_str(&format!("- `{}`{detail}\n", file.rel));
    }
    out.push_str("\n## Contents\n");

    for file in files {
        out.push_str(&format!("\n### `{}`\n\n", file.rel));
        out.push_str(&file_body(file, options));
    }

    if let Some(appendix) = &options.appendix {
        out.push_str("\n## Bundle state\n\n");
        out.push_str("```toml\n");
        out.push_str(appendix.trim_end());
        out.push_str("\n```\n");
    }

    out
}

fn file_body(file: &BundleFile, options: &BundleOptions) -> String {
    let language = language_for(&file.rel);
    let bytes = match fs::metadata(&file.abs) {
        Ok(metadata) if metadata.len() > options.max_file_bytes => {
            return format!(
                "_(omitted: {} bytes, larger than the {} byte limit)_\n",
                metadata.len(),
                options.max_file_bytes
            );
        }
        Ok(_) => fs::read(&file.abs),
        Err(error) => return format!("_(unreadable: {error})_\n"),
    };
    let Ok(bytes) = bytes else {
        return "_(unreadable)_\n".to_string();
    };
    let content = String::from_utf8_lossy(&bytes);
    let content = content.trim_end();
    let fence = fence_for(content);

    format!("{fence}{language}\n{content}\n{fence}\n")
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
    if let Some(language) = Language::from_path(Path::new(rel)) {
        return language.fence();
    }
    let extension = rel.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match extension {
        "kt" | "kts" => "kotlin",
        "groovy" | "gradle" => "groovy",
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
    fn fence_grows_for_embedded_backticks() {
        assert_eq!(fence_for("plain"), "```");
        assert_eq!(fence_for("a ``` b"), "````");
        assert_eq!(fence_for("a ```` b"), "`````");
    }
}
