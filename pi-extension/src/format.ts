//! Envelope -> markdown, and the model-facing output cap.
//!
//! `renderBundleMarkdown` is a literal port of the `jq` recipe in the README:
//! header from the top-level fields, one `## Files` line per entry, the
//! `## Contents` sections in order, then the optional `## Bundle state`
//! appendix. Resolution and slicing stay in Rust; this module only
//! concatenates strings the CLI already produced.

/** Model-facing cap: a bundle past this is cut and the model is told why. */
export const MAX_TEXT_BYTES = 32 * 1024;
/** Second dimension of the same cap, for bundles full of short units. */
export const MAX_TEXT_LINES = 2000;

export interface BundleEntryMarkdown {
  label: string;
  markdown: string;
}

/** The subset of the envelope the markdown recipe reads. */
export interface BundleMarkdownInput {
  title: string;
  root: string;
  files: number;
  excerpts: number;
  methods: number;
  depth: number;
  entries: readonly BundleEntryMarkdown[];
  /** The saved bundle's TOML appendix, `null`/absent when there is none. */
  state_toml?: string | null;
}

export interface Truncation {
  truncated: boolean;
  reason?: "bytes" | "lines";
  /** Bundle units kept / total, when the capped text was a bundle. */
  shownUnits?: number;
  totalUnits?: number;
  omittedUnits?: number;
  /** Lines kept / total, for non-bundle text. */
  shownLines?: number;
  totalLines?: number;
  maxBytes: number;
  maxLines: number;
  note?: string;
}

export interface FormatOptions {
  maxBytes?: number;
  maxLines?: number;
  includeState?: boolean;
}

const encoder = new TextEncoder();

function byteLength(text: string): number {
  return encoder.encode(text).length;
}

function lineCount(text: string): number {
  if (text.length === 0) return 0;
  return text.split("\n").length;
}

function fits(text: string, maxBytes: number, maxLines: number): boolean {
  return lineCount(text) <= maxLines && byteLength(text) <= maxBytes;
}

function takeBytes(text: string, maxBytes: number): string {
  let used = 0;
  let head = "";
  for (const character of text) {
    const size = byteLength(character);
    if (used + size > maxBytes) break;
    head += character;
    used += size;
  }
  return head;
}

/**
 * The CLI-compatible markdown for a bundle envelope.
 *
 * Mirrors the README recipe byte for byte: the header, the `## Files` list,
 * every entry's `markdown` under `## Contents`, and the state appendix when
 * `state_toml` is a string.
 */
export function renderBundleMarkdown(
  bundle: BundleMarkdownInput,
  options: { includeState?: boolean; entries?: readonly BundleEntryMarkdown[] } = {},
): string {
  const includeState = options.includeState ?? true;
  const entries = options.entries ?? bundle.entries;
  let out = `# Code bundle: ${bundle.title}\n\n`;
  out += `- root: \`${bundle.root}\`\n`;
  out += `- files: ${bundle.files}\n`;
  if (bundle.excerpts > 0) out += `- excerpts: ${bundle.excerpts}\n`;
  if (bundle.methods > 0) out += `- methods: ${bundle.methods}\n`;
  out += `- import depth: ${bundle.depth}\n\n`;
  out += "## Files\n\n";
  for (const entry of entries) out += `- ${entry.label}\n`;
  out += "\n## Contents\n";
  for (const entry of entries) out += entry.markdown;
  const state = bundle.state_toml;
  if (includeState && typeof state === "string") {
    out += "\n## Bundle state\n\n```toml\n" + state.replace(/\n+$/, "") + "\n```\n";
  }
  return out;
}

/** The exact note the model sees when a bundle was cut for size. */
export function truncationNote(shownUnits: number, totalUnits: number): string {
  const omitted = Math.max(0, totalUnits - shownUnits);
  return `[truncated: showing ${shownUnits} of ${totalUnits} units; ${omitted} units omitted — raise filer_bundle seeds/depth or read the full bundle with /filer show]`;
}

function partialBundleText(
  bundle: BundleMarkdownInput,
  shown: number,
  total: number,
  withNote: boolean,
): string {
  const entries = bundle.entries.slice(0, shown);
  const text = renderBundleMarkdown(bundle, {
    entries,
    includeState: shown >= total,
  });
  if (!withNote) return text;
  return `${text}\n\n${truncationNote(shown, total)}`;
}

/**
 * Render a bundle and cap it at `maxBytes` / `maxLines`, keeping the head and
 * appending a note that names how many units are missing and how to get them.
 */
export function formatBundle(
  bundle: BundleMarkdownInput,
  options: FormatOptions = {},
): { text: string; truncation: Truncation } {
  const maxBytes = options.maxBytes ?? MAX_TEXT_BYTES;
  const maxLines = options.maxLines ?? MAX_TEXT_LINES;
  const includeState = options.includeState ?? true;
  const total = bundle.entries.length;
  const full = renderBundleMarkdown(bundle, { includeState });
  if (fits(full, maxBytes, maxLines)) {
    return {
      text: full,
      truncation: { truncated: false, totalUnits: total, maxBytes, maxLines },
    };
  }

  // The note is part of the budget, so fit entries with it included.
  let low = 0;
  let high = total;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    const candidate = partialBundleText(bundle, middle, total, true);
    if (fits(candidate, maxBytes, maxLines)) low = middle;
    else high = middle - 1;
  }
  const shown = low;
  const text = partialBundleText(bundle, shown, total, true);
  const oneMore = partialBundleText(bundle, Math.min(shown + 1, total), total, false);
  const reason: "bytes" | "lines" =
    byteLength(oneMore) > maxBytes ? "bytes" : "lines";
  return {
    text,
    truncation: {
      truncated: true,
      reason,
      shownUnits: shown,
      totalUnits: total,
      omittedUnits: total - shown,
      maxBytes,
      maxLines,
      note: truncationNote(shown, total),
    },
  };
}

/**
 * Cap arbitrary text (search rows, chains, state listings) at the same limits,
 * keeping the head and appending a note that says the output was cut.
 */
export function capText(
  text: string,
  options: { maxBytes?: number; maxLines?: number } = {},
): { text: string; truncation: Truncation } {
  const maxBytes = options.maxBytes ?? MAX_TEXT_BYTES;
  const maxLines = options.maxLines ?? MAX_TEXT_LINES;
  if (fits(text, maxBytes, maxLines)) {
    return {
      text,
      truncation: { truncated: false, maxBytes, maxLines },
    };
  }
  const lines = text.split("\n");
  let head = lines.length > maxLines ? lines.slice(0, maxLines).join("\n") : text;
  const byteCapped = byteLength(head) > maxBytes;
  if (byteCapped) head = takeBytes(head, maxBytes);
  const totalLines = lines.length;
  const shownLines = lineCount(head);
  const note = `[truncated: showing ${shownLines} of ${totalLines} lines; narrow the query or read the full output with /filer show]`;
  return {
    text: `${head}\n\n${note}`,
    truncation: {
      truncated: true,
      reason: byteCapped ? "bytes" : "lines",
      shownLines,
      totalLines,
      maxBytes,
      maxLines,
      note,
    },
  };
}
