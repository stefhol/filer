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
//! Nothing is blocked: ranged reads, images and non-source files are untouched,
//! so quick peeks and `/skill` file loads keep working. The usability flag is
//! owned by the `session_start` probe in `src/tools.ts`; until it passes, this
//! module adds nothing to the prompt and leaves results alone.

import type { ExtensionAPI, ToolResultEvent } from "@earendil-works/pi-coding-agent";

/** One block of a tool result: text, or an image. */
type ContentPart = ToolResultEvent["content"][number];

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

/** The prompt section key; reusing it replaces our earlier text, never stacks. */
export const GUIDANCE_SECTION = "tool_guidance";

/** Marks a result this module already annotated, so a second pass is a no-op. */
export const READ_NUDGE_MARKER = "[filer]";

/** The cross-tool guidance added to the system prompt while filer is usable. */
export const TOOL_GUIDANCE = [
  "- To load a source file you need to understand, call `filer_bundle` with its path instead of `read`: it returns the file plus its import/call closure (depth 2 by default), which `read` cannot give you.",
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

/** The note appended to a full read of a source file. */
export function readNudge(path: string): string {
  return `${READ_NUDGE_MARKER} \`${path}\` is a source file filer parses. \`filer_bundle({ seeds: [${JSON.stringify(path)}] })\` returns it together with its import/call closure, so you also see what it depends on; \`filer_why\` explains what pulled it into a bundle.`;
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

  pi.on("before_agent_start", (event) => {
    if (!deps.availability.usable) return;
    const active = event.systemPromptOptions.selectedTools;
    if (!active.includes(bundleTool)) return;
    // Assign rather than append: the section is derived from current state, so
    // an earlier run's text is replaced instead of duplicated.
    event.systemPromptOptions.sections[GUIDANCE_SECTION] = TOOL_GUIDANCE;
  });

  pi.on("tool_result", (event) => {
    if (!deps.availability.usable) return;
    if (event.toolName !== "read" || event.isError) return;
    const nudge = nudgeForRead(event.input);
    if (nudge === null) return;
    const annotated = event.content.map((part): ContentPart => {
      if (part.type !== "text" || part.text.includes(READ_NUDGE_MARKER)) return part;
      return { ...part, text: `${part.text}\n\n${nudge}` };
    });
    if (annotated.every((part, index) => part === event.content[index])) return;
    return { content: annotated };
  });
}
