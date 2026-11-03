//! Steering: teach the model when the filer tools beat `read` and grep (#61).
//!
//! Three registrations, all gated on the binary being usable:
//!
//! - `before_agent_start` adds a `tool_guidance` section so the model knows
//!   that `filer_bundle` returns a source file *plus* its import/call closure,
//!   and that symbol questions belong to `filer_search`/`filer_deps`/
//!   `filer_callers`;
//! - `tool_result` on a full `read` of a source file appends the closure facts
//!   for that path, so a model that reached for `read` is corrected in the turn;
//! - `tool_result` on a `bash` declaration search (`grep -n "fn to_toml"`)
//!   appends the units `filer_search` resolves for that name (#71). That is the
//!   event that actually happens: ranged peeks dominate `read` (981 of 1,134
//!   reads in the sessions measured for #71), while 432 searches were declared
//!   as declarations.
//!
//! Both paragraphs carry numbers (#68) and never guess: the read's own size is
//! free (it is the result the handler already holds), the closure comes from
//! one memoized `filer deps --depth 2` probe per path, and the symbol rows come
//! from one memoized `filer search` per name. A failed or slow probe drops the
//! claim instead of inventing one, and a search that finds no declaration stays
//! silent, so log and text greps are never annotated.
//!
//! Nothing is blocked: images, non-source files and non-declaration searches
//! are untouched, so quick peeks and `/skill` file loads keep working. The
//! usability flag is owned by the `session_start` probe in `src/tools.ts`;
//! until it passes, this module adds nothing to the prompt and leaves results
//! alone. `filer_show`, `filer_state` and `filer_why` are registered as
//! `deferred`, so no nudge points at them without `tool_search` first (#71).

import type { ExtensionAPI, ToolResultEvent } from "@earendil-works/pi-coding-agent";
import { diagnosticsSummary } from "./format";
import {
  deps,
  search,
  type DepsEnvelope,
  type SearchData,
  type SearchRow,
} from "./operations";
import {
  SOURCE_EXTENSIONS,
  extensionOf,
  sourceExtensionOf,
} from "./source";
import type { ClientResolver } from "./tools";

// Re-exported: the extension list is part of this module's public surface.
export { SOURCE_EXTENSIONS, extensionOf, sourceExtensionOf };

/** One block of a tool result: text, or an image. */
type ContentPart = ToolResultEvent["content"][number];

/** The prompt section key; reusing it replaces our earlier text, never stacks. */
export const GUIDANCE_SECTION = "tool_guidance";

/**
 * Marks a result this module already annotated, so a second pass is a no-op.
 * The read nudge introduced the prefix (#68); the symbol nudge reuses it, so a
 * result is annotated at most once whichever handler sees it first.
 */
export const NUDGE_MARKER = "[filer]";
/** The name the read nudge introduced (#68); kept for its tests and callers. */
export const READ_NUDGE_MARKER = NUDGE_MARKER;

/** The cross-tool guidance added to the system prompt while filer is usable. */
export const TOOL_GUIDANCE = [
  "- To load a source file you need to understand, call `filer_bundle` with its path instead of `read`: it returns the file plus its import/call closure (depth 2 by default), which `read` cannot give you.",
  "- For a workspace crate, seed its directory (`crates/hub`) or its `src/lib.rs`, not one file: the closure is what describes the build.",
  "- Use `read` for config, documentation, generated or unsupported files, and for a narrow `offset`/`limit` range you already know.",
  "- To learn what a function, method or class does, call `filer_deps` on its unit (`path::Symbol`), `filer_callers` for who reaches it, or `filer_bundle` on `path::Class.method`, instead of grepping its body or its call sites.",
  "- The `/filer` commands belong to the user; the model uses the `filer_*` tools.",
].join("\n");

/**
 * Set by the `session_start` probe in `src/tools.ts`. Steering stays off until
 * `filer --version` succeeded, so a machine without filer sees no change.
 */
export interface FilerAvailability {
  usable: boolean;
}

export function createAvailability(): FilerAvailability {
  return { usable: false };
}

// --- closure facts --------------------------------------------------------

/** Depth the nudge reports: `depth 0` would just be the file `read` returned. */
export const READ_PROBE_DEPTH = 2;

/** How long the nudge may wait for the probe before it says less. */
export const READ_PROBE_TIMEOUT_MS = 2_000;

/** What one `filer deps` probe learned about a file's closure. */
export interface ClosureFacts {
  /** Files in the closure at `READ_PROBE_DEPTH`, the seed included. */
  files: number;
  /** Unresolved imports the traversal reported for the closure. */
  unresolved: number;
  /** Deepest hop the probe actually reached. */
  depth: number;
}

/** One probe: the closure of a path, or null when it could not be known. */
export type ClosureProbe = (
  path: string,
  ctx: { cwd: string },
) => Promise<ClosureFacts | null>;

/**
 * The facts a `deps --json` envelope already carries. The row set includes the
 * seed at depth 0, so the distinct paths are exactly the files a bundle of the
 * same seed would render.
 */
export function closureFacts(data: DepsEnvelope): ClosureFacts {
  const files = new Set(data.deps.map((row) => row.path));
  const depth = data.deps.reduce((deepest, row) => Math.max(deepest, row.depth), 0);
  return {
    files: files.size,
    unresolved: diagnosticsSummary(data.diagnostics).total,
    depth,
  };
}

/**
 * The real probe, over the same client resolver the tools use. A failure (no
 * binary, unindexed path, bad JSON) is "unknown", never a zero.
 */
export function createClosureProbe(resolve: ClientResolver): ClosureProbe {
  return async (path, ctx) => {
    try {
      const context = await resolve(ctx, undefined);
      const result = await deps(context, { seed: path, depth: READ_PROBE_DEPTH });
      return closureFacts(result.data);
    } catch {
      return null;
    }
  };
}

/**
 * Resolve `work` within `ms`, or null. Both outcomes are cached by the caller,
 * so a slow or broken probe costs one spawn per path per session, not one per
 * read.
 */
function timed<T>(work: Promise<T | null>, ms: number): Promise<T | null> {
  return new Promise((resolvePromise) => {
    const timer = setTimeout(() => resolvePromise(null), ms);
    work.then(
      (value) => {
        clearTimeout(timer);
        resolvePromise(value);
      },
      () => {
        clearTimeout(timer);
        resolvePromise(null);
      },
    );
  });
}

/** Memoize a probe per path, failures included. */
export function memoizedProbe(probe: ClosureProbe): ClosureProbe {
  const cache = new Map<string, Promise<ClosureFacts | null>>();
  return (path, ctx) => {
    const cached = cache.get(path);
    if (cached !== undefined) return cached;
    const pending = timed(probe(path, ctx), READ_PROBE_TIMEOUT_MS);
    cache.set(path, pending);
    return pending;
  };
}

// --- the nudge text -------------------------------------------------------

/** The size of a `read` result, summed over its text blocks. */
export interface ReadSize {
  lines: number;
  bytes: number;
}

const encoder = new TextEncoder();

/** Count what the model was just handed: lines and UTF-8 bytes. */
export function readSize(parts: readonly ContentPart[]): ReadSize {
  let lines = 0;
  let bytes = 0;
  for (const part of parts) {
    if (part.type !== "text" || typeof part.text !== "string") continue;
    const text = part.text;
    bytes += encoder.encode(text).length;
    if (text.length > 0) lines += text.replace(/\n$/, "").split("\n").length;
  }
  return { lines, bytes };
}

/** `24 KB`, `4.2 KB`: one significant digit below 10. */
export function formatSize(bytes: number): string {
  const kilobytes = bytes / 1024;
  return kilobytes >= 10 ? `${Math.round(kilobytes)} KB` : `${kilobytes.toFixed(1)} KB`;
}

/** The note appended to a full read of a source file. */
export function readNudge(
  path: string,
  size?: ReadSize,
  facts?: ClosureFacts | null,
): string {
  const seed = JSON.stringify(path);
  const bundle = `\`filer_bundle({ seeds: [${seed}] })\``;
  if (size === undefined) {
    return `${READ_NUDGE_MARKER} \`${path}\` is a source file filer parses. ${bundle} returns it together with its import/call closure, so you also see what it depends on.`;
  }
  const lines = `${size.lines} line${size.lines === 1 ? "" : "s"} / ${formatSize(size.bytes)}`;
  const closure =
    facts === null || facts === undefined
      ? "`filer_bundle` returns it together with its import/call closure, so you also see what it depends on"
      : `Its closure at depth ${READ_PROBE_DEPTH} is ${facts.files} file${
          facts.files === 1 ? "" : "s"
        }${
          facts.unresolved === 0
            ? ""
            : ` (+${facts.unresolved} unresolved import${
                facts.unresolved === 1 ? "" : "s"
              })`
        }`;
  return `${READ_NUDGE_MARKER} \`${path}\` is a source file filer parses — ${lines}. ${closure}; ${bundle} returns it all.`;
}

/**
 * The nudge for a `read` call, or null when none applies. A ranged read
 * (`offset`/`limit`) is a deliberate peek at a known location, not the
 * "load this file" case this guidance is about.
 */
export function nudgeForRead(input: Record<string, unknown>): string | null {
  const path = input["path"];
  if (typeof path !== "string") return null;
  if (input["offset"] !== undefined || input["limit"] !== undefined) return null;
  if (sourceExtensionOf(path) === null) return null;
  return readNudge(path);
}

// --- symbol nudge ---------------------------------------------------------

/** Commands that search file contents; the first operand is the pattern. */
export const SEARCH_COMMANDS: readonly string[] = ["rg", "grep", "ag", "ack"];

/** Keywords whose operand names a declaration, not arbitrary text. */
export const DECLARATION_KEYWORDS: readonly string[] = [
  "fn",
  "struct",
  "enum",
  "trait",
  "impl",
  "class",
  "interface",
  "def",
  "func",
  "fun",
  "type",
];

const DECLARATION = new RegExp(
  `\\b(?:${DECLARATION_KEYWORDS.join("|")})\\s+([A-Za-z_][A-Za-z0-9_]*)`,
  "g",
);

/** Options whose next word is a value, not the search pattern. */
const OPTIONS_WITH_VALUE = new Set([
  "-e",
  "-f",
  "--regexp",
  "--file",
  "-g",
  "--glob",
  "--iglob",
  "--include",
  "--exclude",
  "--type",
  "-t",
  "-T",
  "--type-not",
  "-A",
  "-B",
  "-C",
  "-m",
  "--max-count",
  "--after-context",
  "--before-context",
  "--context",
  "--ignore-file",
]);

/** Words that may sit between a segment start and the search command. */
const COMMAND_PREFIXES = new Set([
  "cd",
  "sudo",
  "env",
  "time",
  "command",
  "unbuffer",
]);

/** Names one command may contribute; three is already generous. */
export const MAX_DECLARATION_NAMES = 3;

/**
 * Split one shell segment into words, dropping quotes. This is deliberately not
 * a shell parser: it only needs to find `rg`/`grep` and read its pattern.
 */
export function shellWords(segment: string): string[] {
  const words: string[] = [];
  const word = /"((?:[^"\\]|\\.)*)"|'([^']*)'|(\S+)/g;
  for (const match of segment.matchAll(word)) {
    if (match[1] !== undefined) words.push(match[1].replace(/\\(.)/g, "$1"));
    else if (match[2] !== undefined) words.push(match[2]);
    else if (match[3] !== undefined) words.push(match[3]);
  }
  return words;
}

/**
 * Split a command line into segments at `;`, `&&`, `||`, `|` and newlines,
 * ignoring separators inside quotes so a pattern like `'fn a|fn b'` stays one
 * word.
 */
export function shellSegments(command: string): string[] {
  const segments: string[] = [];
  let current = "";
  let quote: string | null = null;
  for (let index = 0; index < command.length; index += 1) {
    const char = command[index]!;
    if (quote !== null) {
      current += char;
      if (char === "\\" && quote === '"') {
        current += command[index + 1] ?? "";
        index += 1;
      } else if (char === quote) {
        quote = null;
      }
      continue;
    }
    if (char === '"' || char === "'") {
      quote = char;
      current += char;
      continue;
    }
    if (char === "\\") {
      current += char + (command[index + 1] ?? "");
      index += 1;
      continue;
    }
    const next = command[index + 1];
    if ((char === "|" && next === "|") || (char === "&" && next === "&")) {
      segments.push(current);
      current = "";
      index += 1;
      continue;
    }
    if (char === "|" || char === ";" || char === "\n") {
      segments.push(current);
      current = "";
      continue;
    }
    current += char;
  }
  segments.push(current);
  return segments;
}

/**
 * The names a command greps for as declarations, in order of appearance.
 *
 * Only declaration-shaped patterns count: `grep -n "fn to_toml"` yields
 * `to_toml`, while `grep -iE "error"` and `rg '\.walk\('` yield nothing, so
 * ordinary text and log searches are never annotated.
 */
export function declarationNames(command: string): string[] {
  const names: string[] = [];
  for (const segment of shellSegments(command)) {
    const words = shellWords(segment);
    const at = words.findIndex((word) => SEARCH_COMMANDS.includes(word));
    if (at < 0) continue;
    if (!words.slice(0, at).every((word) => COMMAND_PREFIXES.has(word))) continue;
    const pattern = searchPattern(words.slice(at + 1));
    if (pattern === null) continue;
    for (const match of pattern.matchAll(DECLARATION)) {
      const name = match[1];
      if (name !== undefined && !names.includes(name)) names.push(name);
    }
  }
  return names.slice(0, MAX_DECLARATION_NAMES);
}

/** The first operand that is not an option or an option's value. */
function searchPattern(args: readonly string[]): string | null {
  for (let index = 0; index < args.length; index += 1) {
    const word = args[index]!;
    // `-e`/`--regexp` *are* the pattern; `-f`/`--file` keep it in a file.
    if (word === "-e" || word === "--regexp") return args[index + 1] ?? null;
    if (word === "-f" || word === "--file") return null;
    if (word.startsWith("-")) {
      // Skip the option's value too: `--glob '*.rs'` is not the pattern.
      if (OPTIONS_WITH_VALUE.has(word)) index += 1;
      continue;
    }
    return word;
  }
  return null;
}

/** One unit `filer_search` resolved for a name. */
export interface SymbolFacts {
  /** The seed the tools accept: `path::Symbol` or `path::Class.method`. */
  unit: string;
  kind: string;
  path: string;
  line: number | null;
  tokens: number;
}

/** Rows asked of the probe: enough for three declarations plus file rows. */
export const SYMBOL_PROBE_LIMIT = 8;
/** How long the symbol nudge may wait before it stays silent. */
export const SYMBOL_PROBE_TIMEOUT_MS = 2000;

/** The declaration rows of a search envelope, deduplicated and capped. */
export function symbolFacts(data: SearchData): SymbolFacts[] {
  const facts: SymbolFacts[] = [];
  for (const row of data.rows) {
    const fact = symbolFact(row);
    if (fact === null) continue;
    if (facts.some((seen) => seen.unit === fact.unit)) continue;
    facts.push(fact);
    if (facts.length === MAX_DECLARATION_NAMES) break;
  }
  return facts;
}

function symbolFact(row: SearchRow): SymbolFacts | null {
  // A file row is a path hit, not a declaration: `grep` already found the file.
  if (row.symbol === null || row.scope === "file") return null;
  // `fqn` is language-dependent (bare in Rust, package-qualified in Java); the
  // tools take the `path::Symbol` form, so build that from the row.
  const unit =
    row.member === null
      ? `${row.path}::${row.symbol}`
      : `${row.path}::${row.symbol}.${row.member}`;
  return {
    unit,
    kind: row.kind,
    path: row.path,
    line: row.line,
    tokens: row.est_tokens,
  };
}

/** One lookup per name: the units, or null when it could not be known. */
export type SymbolProbe = (
  name: string,
  ctx: { cwd: string },
) => Promise<SymbolFacts[] | null>;

/**
 * The real probe, over the same client resolver the tools use. A failure (no
 * binary, unindexed root, bad JSON) is "unknown", never "no declarations".
 */
export function createSymbolProbe(resolve: ClientResolver): SymbolProbe {
  return async (name, ctx) => {
    try {
      const context = await resolve(ctx, undefined);
      const result = await search(context, {
        query: name,
        limit: SYMBOL_PROBE_LIMIT,
      });
      return symbolFacts(result.data);
    } catch {
      return null;
    }
  };
}

/** Memoize a probe per project and name, failures included. */
export function memoizedSymbolProbe(probe: SymbolProbe): SymbolProbe {
  const cache = new Map<string, Promise<SymbolFacts[] | null>>();
  return (name, ctx) => {
    const key = `${ctx.cwd}\u0000${name}`;
    const cached = cache.get(key);
    if (cached !== undefined) return cached;
    const pending = timed(probe(name, ctx), SYMBOL_PROBE_TIMEOUT_MS);
    cache.set(key, pending);
    return pending;
  };
}

/** The note appended to a `bash` declaration search. */
export function symbolNudge(name: string, facts: readonly SymbolFacts[]): string {
  const rows = facts
    .slice(0, MAX_DECLARATION_NAMES)
    .map((fact) => {
      const where = fact.line === null ? fact.path : `${fact.path}:${fact.line}`;
      return `${fact.unit} (${fact.kind}, ${where}, ${fact.tokens} tokens)`;
    })
    .join("; ");
  return `${NUDGE_MARKER} \`${name}\` is a declaration: ${rows}. \`filer_search({ query: ${JSON.stringify(
    name,
  )} })\` returns each unit's path, kind, span and size; \`filer_callers({ unit })\` takes that seed as-is, and \`filer_refs({ name })\` lists every recorded use.`;
}

export interface FilerGuidanceDeps {
  availability: FilerAvailability;
  /** The tool the guidance points at; defaults to `filer_bundle`. */
  bundleTool?: string;
  /** Closure probe for the `read` nudge; absent means "say less". */
  probe?: ClosureProbe;
  /** Symbol probe for the `bash` nudge; absent means "say nothing". */
  symbolProbe?: SymbolProbe;
}

/**
 * Register the prompt section and the two result nudges.
 *
 * Every handler is a no-op until `availability.usable` is true, and the prompt
 * section also requires the bundle tool to be active, so a session that
 * removed the tools (see `registerFilerTools`) is never told to call them.
 */
export function registerFilerGuidance(
  pi: ExtensionAPI,
  deps: FilerGuidanceDeps,
): void {
  const bundleTool = deps.bundleTool ?? "filer_bundle";
  const probe = deps.probe === undefined ? undefined : memoizedProbe(deps.probe);
  const symbolProbe =
    deps.symbolProbe === undefined ? undefined : memoizedSymbolProbe(deps.symbolProbe);

  pi.on("before_agent_start", (event) => {
    if (!deps.availability.usable) return;
    const active = event.systemPromptOptions.selectedTools;
    if (!active.includes(bundleTool)) return;
    // Assign rather than append: the section is derived from current state, so
    // an earlier run's text is replaced instead of duplicated.
    event.systemPromptOptions.sections[GUIDANCE_SECTION] = TOOL_GUIDANCE;
  });

  pi.on("tool_result", async (event, ctx) => {
    if (!deps.availability.usable) return;
    if (event.isError) return;
    if (alreadyAnnotated(event.content)) return;

    // #71: the event that actually happens is a declaration grep, not a read.
    if (event.toolName === "bash") {
      const command = event.input["command"];
      if (typeof command !== "string" || symbolProbe === undefined) return;
      const name = declarationNames(command)[0];
      if (name === undefined) return;
      // One memoized spawn per project and name; null means "unknown".
      const facts = await symbolProbe(name, ctx);
      if (facts === null || facts.length === 0) return;
      return annotate(event.content, symbolNudge(name, facts));
    }

    if (event.toolName !== "read") return;
    if (nudgeForRead(event.input) === null) return;
    const path = event.input["path"] as string;
    // One memoized spawn per path; null means "the number is unknown".
    const facts = probe === undefined ? undefined : await probe(path, ctx);
    const nudge = readNudge(path, readSize(event.content), facts);
    return annotate(event.content, nudge);
  });
}

/** A result this module already annotated is left exactly as it is. */
function alreadyAnnotated(content: readonly ContentPart[]): boolean {
  return content.some(
    (part) => part.type === "text" && part.text.includes(NUDGE_MARKER),
  );
}

/** The tool-result shape a nudge returns: replaced content, nothing else. */
interface NudgeResult {
  content: ContentPart[];
}

/** Append one paragraph to every text block, leaving images untouched. */
function annotate(
  content: readonly ContentPart[],
  nudge: string,
): NudgeResult {
  return {
    content: content.map((part) =>
      part.type === "text" ? { ...part, text: `${part.text}\n\n${nudge}` } : part,
    ),
  };
}
