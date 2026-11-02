//! Markdown rendering of a code bundle that can be pasted into a chat.

use std::fs;
use std::path::PathBuf;

use crate::index::Index;

pub struct BundleOptions {
    pub title: Option<String>,
    pub depth: u32,
    pub max_file_bytes: u64,
}

impl Default for BundleOptions {
    fn default() -> Self {
        Self {
            title: None,
            depth: 2,
            max_file_bytes: 1024 * 1024,
        }
    }
}

pub struct BundleFile {
    pub rel: String,
    pub abs: PathBuf,
    pub depth: u32,
    /// Extra qualifier shown in the file list, e.g. `build config`.
    pub note: Option<String>,
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
        match file.depth {
            0 => out.push_str(&format!("- `{}`\n", file.rel)),
            depth => out.push_str(&format!("- `{}` (import depth {depth})\n", file.rel)),
        }
    }
    out.push_str("\n## Contents\n");

    for file in files {
        out.push_str(&format!("\n### `{}`\n\n", file.rel));
        if let Some(note) = &file.note {
            out.push_str(&format!("_{note}_\n\n"));
        }
        out.push_str(&file_body(file, options));
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
    let extension = rel.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match extension {
        "java" => "java",
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
