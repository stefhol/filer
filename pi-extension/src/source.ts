//! Which files filer parses, by extension.
//!
//! Shared by the steering (`src/guidance.ts`, which nudges `read` towards
//! `filer_bundle`) and by directory-seed expansion (`src/seeds.ts`), so both
//! agree on what counts as a source file.

/**
 * Extensions filer parses. Keep in sync with `Language::from_path` in
 * `src/lang.rs`; an unsupported file gets no nudge and no guideline.
 */
export const SOURCE_EXTENSIONS: readonly string[] = [
  "java",
  "scala",
  "sbt",
  "sc",
  "kt",
  "kts",
  "groovy",
  "gradle",
  "cs",
  "ts",
  "tsx",
  "js",
  "jsx",
  "mjs",
  "cjs",
  "vue",
  "fe",
  "rs",
  "py",
  "pyi",
];

/** The lowercase extension of a path, or null when it has none. */
export function extensionOf(path: string): string | null {
  const file = path.split(/[\\/]/).pop() ?? "";
  const dot = file.lastIndexOf(".");
  // `dot <= 0` keeps dotfiles such as `.bashrc` and bare names such as `Makefile`.
  if (dot <= 0) return null;
  return file.slice(dot + 1).toLowerCase();
}

/** The extension when filer parses this path, otherwise null. */
export function sourceExtensionOf(path: string): string | null {
  const extension = extensionOf(path);
  return extension !== null && SOURCE_EXTENSIONS.includes(extension) ? extension : null;
}
