//! Steering: teach the model when the filer tools beat `read` and grep (#61).
//!
//! Two registrations, both gated on the binary being usable:
//!
//! - `before_agent_start` adds a `tool_guidance` section so the model knows
//!   that `filer_bundle` returns a source file *plus* its import/call closure,
//!   and that symbol questions belong to `filer_deps`/`filer_callers`;
//! - `tool_result` appends one paragraph to a full `read` of a source file, so
//!   a model that already reached for `read` is corrected within the same turn.
//!
//! The paragraph carries numbers (#68): the read's own size is free (it is the
//! result this handler already holds), and the closure comes from one memoized
//! `filer deps --depth 2` probe per path. If the probe fails or runs long, the
//! closure sentence is dropped rather than guessed.
//!
//! Nothing is blocked: ranged reads, images and non-source files are untouched,
//! so quick peeks and `/skill` file loads keep working. The usability flag is
//! owned by the `session_start` probe in `src/tools.ts`; until it passes, this
//! module adds nothing to the prompt and leaves results alone.

import type { ExtensionAPI, ToolResultEvent } from "@earendil-works/pi-coding-agent";
import { diagnosticsSummary } from "./format";
import { deps, type DepsEnvelope } from "./operations";
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

/** Marks a result this module already annotated, so a second pass is a no-op. */
export const READ_NUDGE_MARKER = "[filer]";

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
  const why = "`filer_why` explains what pulled a unit into a bundle.";
  if (size === undefined) {
    return `${READ_NUDGE_MARKER} \`${path}\` is a source file filer parses. ${bundle} returns it together with its import/call closure, so you also see what it depends on; ${why}`;
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
  return `${READ_NUDGE_MARKER} \`${path}\` is a source file filer parses — ${lines}. ${closure}; ${bundle} returns it all, and ${why}`;
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

export interface FilerGuidanceDeps {
  availability: FilerAvailability;
  /** The tool the guidance points at; defaults to `filer_bundle`. */
  bundleTool?: string;
  /** Closure probe for the `read` nudge; absent means "say less". */
  probe?: ClosureProbe;
}

/**
 * Register the prompt section and the `read` nudge.
 *
 * Both handlers are no-ops until `availability.usable` is true, and the prompt
 * section also requires the bundle tool to be active, so a session that
 * removed the tools (see `registerFilerTools`) is never told to call them.
 */
export function registerFilerGuidance(
  pi: ExtensionAPI,
  deps: FilerGuidanceDeps,
): void {
  const bundleTool = deps.bundleTool ?? "filer_bundle";
  const probe = deps.probe === undefined ? undefined : memoizedProbe(deps.probe);

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
    if (event.toolName !== "read" || event.isError) return;
    if (nudgeForRead(event.input) === null) return;
    if (
      event.content.some(
        (part) => part.type === "text" && part.text.includes(READ_NUDGE_MARKER),
      )
    ) {
      return;
    }
    const path = event.input["path"] as string;
    // One memoized spawn per path; null means "the number is unknown".
    const facts = probe === undefined ? undefined : await probe(path, ctx);
    const nudge = readNudge(path, readSize(event.content), facts);
    const annotated = event.content.map((part): ContentPart => {
      if (part.type !== "text") return part;
      return { ...part, text: `${part.text}\n\n${nudge}` };
    });
    return { content: annotated };
  });
}
