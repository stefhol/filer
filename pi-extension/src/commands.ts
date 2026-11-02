//! The `/filer` command surface (#42).
//!
//! One command (`/filer`) with subcommands. Every subcommand runs the same
//! typed operation the tools use, maps failures through `errors.ts`, and
//! reports through pi's session/UI APIs:
//!
//! - bundle/show append a `filer-bundle` custom message without a turn;
//! - state/index/why append their own custom messages;
//! - bin/root set a session override and append `filer-overrides`;
//! - reset asks for confirmation and clears the saved state;
//! - the status line shows `<units> units · <root>` after a bundle/show.
//!
//! The parser and the diff/format helpers are pure; the command handler only
//! wires them to the adapter and pi.

import { stat } from "node:fs/promises";
import { resolve as resolvePath } from "node:path";
import type {
  ExtensionAPI,
  ExtensionCommandContext,
  ExtensionContext,
} from "@earendil-works/pi-coding-agent";
import type { FilerAdapter } from "../index";
import { FilerToolError, classifyRun } from "./errors";
import {
  bundle,
  index as indexOperation,
  show,
  state,
  why,
  type OperationContext,
  type StateEntry,
  type WhyData,
} from "./operations";
import { SUPPORTED_SCHEMA_VERSION } from "./schema";
import type { SessionOverrides } from "./session";

/** How long `/filer bin` may spend on `filer --version`. */
export const VERSION_TIMEOUT_MS = 5_000;

/** The status-line key the extension owns. */
export const STATUS_KEY = "filer";

// --- Command model --------------------------------------------------------

export type FilerCommand =
  | { kind: "help" }
  | { kind: "bundle"; seeds: string[]; depth?: number; reset: boolean }
  | { kind: "show"; branch?: string }
  | { kind: "state" }
  | { kind: "reset"; all: boolean }
  | { kind: "index" }
  | { kind: "why"; unit: string }
  | { kind: "bin"; path?: string }
  | { kind: "root"; path?: string }
  | { kind: "unknown"; subcommand: string }
  | { kind: "invalid"; message: string };

/** Split a command tail into tokens, honoring `"..."`, `'...'` and `\`. */
export function tokenize(input: string): string[] {
  const tokens: string[] = [];
  let current = "";
  let quote: '"' | "'" | undefined;
  let escaped = false;
  for (const char of input) {
    if (escaped) {
      current += char;
      escaped = false;
      continue;
    }
    if (char === "\\" && quote !== "'") {
      escaped = true;
      continue;
    }
    if (quote !== undefined) {
      if (char === quote) quote = undefined;
      else current += char;
      continue;
    }
    if (char === '"' || char === "'") {
      quote = char;
      continue;
    }
    if (/\s/.test(char)) {
      if (current.length > 0) {
        tokens.push(current);
        current = "";
      }
      continue;
    }
    current += char;
  }
  if (current.length > 0) tokens.push(current);
  return tokens;
}

function invalid(message: string): FilerCommand {
  return { kind: "invalid", message };
}

function parseDepth(value: string | undefined): number | FilerCommand {
  if (value === undefined || value.trim().length === 0) {
    return invalid("--depth needs a number");
  }
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < 0) {
    return invalid(`invalid --depth: ${value}`);
  }
  return parsed;
}

function parseBundle(tokens: string[]): FilerCommand {
  const seeds: string[] = [];
  let depth: number | undefined;
  let reset = false;
  let endOfFlags = false;
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index] ?? "";
    if (!endOfFlags && token === "--") {
      endOfFlags = true;
      continue;
    }
    if (!endOfFlags && token === "--reset") {
      reset = true;
      continue;
    }
    if (!endOfFlags && token === "--depth") {
      const parsed = parseDepth(tokens[index + 1]);
      if (typeof parsed !== "number") return parsed;
      depth = parsed;
      index += 1;
      continue;
    }
    if (!endOfFlags && token.startsWith("--depth=")) {
      const parsed = parseDepth(token.slice("--depth=".length));
      if (typeof parsed !== "number") return parsed;
      depth = parsed;
      continue;
    }
    if (!endOfFlags && token.startsWith("--")) {
      return invalid(`unknown option: ${token}`);
    }
    seeds.push(token);
  }
  if (seeds.length === 0) return invalid("bundle needs at least one seed");
  return {
    kind: "bundle",
    seeds,
    reset,
    ...(depth === undefined ? {} : { depth }),
  };
}

function parseReset(tokens: string[]): FilerCommand {
  let all = false;
  for (const token of tokens) {
    if (token === "--all") {
      all = true;
      continue;
    }
    if (token.startsWith("--")) return invalid(`unknown option: ${token}`);
    return invalid(`unexpected argument: ${token}`);
  }
  return { kind: "reset", all };
}

/** Parse `/filer ...`; the empty tail is the help command. */
export function parseFilerCommand(input: string): FilerCommand {
  const tokens = tokenize(input);
  const subcommand = tokens[0];
  const rest = tokens.slice(1);
  if (subcommand === undefined) return { kind: "help" };
  switch (subcommand) {
    case "bundle":
      return parseBundle(rest);
    case "show": {
      if (rest.length === 0) return { kind: "show" };
      if (rest.length > 1) return invalid("show takes at most one branch");
      const branch = rest[0] ?? "";
      if (branch.startsWith("--")) return invalid(`unknown option: ${branch}`);
      return { kind: "show", branch };
    }
    case "state":
      return rest.length === 0 ? { kind: "state" } : invalid("state takes no arguments");
    case "reset":
      return parseReset(rest);
    case "index":
      return rest.length === 0 ? { kind: "index" } : invalid("index takes no arguments");
    case "why": {
      if (rest.length === 0) return invalid("why needs a unit");
      if ((rest[0] ?? "").startsWith("--")) {
        return invalid(`unknown option: ${rest[0] ?? ""}`);
      }
      return { kind: "why", unit: rest.join(" ") };
    }
    case "bin": {
      if (rest.length === 0) return { kind: "bin" };
      if ((rest[0] ?? "").startsWith("--")) {
        return invalid(`unknown option: ${rest[0] ?? ""}`);
      }
      return { kind: "bin", path: rest.join(" ") };
    }
    case "root": {
      if (rest.length === 0) return { kind: "root" };
      if ((rest[0] ?? "").startsWith("--")) {
        return invalid(`unknown option: ${rest[0] ?? ""}`);
      }
      return { kind: "root", path: rest.join(" ") };
    }
    default:
      return { kind: "unknown", subcommand };
  }
}

// --- Pure helpers ---------------------------------------------------------

/** The identity of a saved unit for the `M new` diff. */
export interface UnitKeyInput {
  path: string;
  scope: string;
  symbol: string | null;
  methods: readonly string[];
}

export function unitKey(unit: UnitKeyInput): string {
  const methods = [...unit.methods].sort().join(",");
  return `${unit.path}\u0000${unit.scope}\u0000${unit.symbol ?? ""}\u0000${methods}`;
}

/** How many of `next` were not already in `previous` (path/scope/symbol/methods). */
export function countNewUnits(
  previous: readonly UnitKeyInput[],
  next: readonly UnitKeyInput[],
): number {
  const seen = new Set(previous.map(unitKey));
  let added = 0;
  for (const unit of next) {
    if (!seen.has(unitKey(unit))) added += 1;
  }
  return added;
}

/** `seed -> hop (line N) -> target`, the one-line chain `/filer why` shows. */
export function whyChainText(data: WhyData): string {
  const seed = data.chain[0];
  if (seed === undefined) return `${data.target} (${data.reason})`;
  const parts: string[] = [seed.unit];
  let lastUnit = seed.unit;
  for (let index = 1; index < data.chain.length; index += 1) {
    const hop = data.chain[index];
    if (hop === undefined) continue;
    const line = hop.line === null ? "" : ` (line ${hop.line})`;
    parts.push(`${hop.unit}${line}`);
    lastUnit = hop.unit;
  }
  if (lastUnit !== data.target) parts.push(data.target);
  return parts.join(" -> ");
}

function flagValue(value: boolean | string | undefined): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length === 0 ? undefined : trimmed;
}

/** Resolve a typed bin path against cwd, but keep bare names for `PATH`. */
function normalizeBin(input: string, cwd: string): string {
  const trimmed = input.trim();
  if (trimmed.length === 0) return trimmed;
  if (trimmed.includes("/") || trimmed.includes("\\")) {
    return resolvePath(cwd, trimmed);
  }
  return trimmed;
}

function setStatus(ctx: ExtensionContext, text: string | undefined): void {
  if (!ctx.hasUI) return;
  ctx.ui.setStatus(STATUS_KEY, text);
}

function notifyError(ctx: ExtensionCommandContext, error: unknown): void {
  const message =
    error instanceof FilerToolError
      ? error.toToolMessage()
      : `filer command failed: ${error instanceof Error ? error.message : String(error)}`;
  ctx.ui.notify(message, "error");
}

// --- Dependencies ---------------------------------------------------------

export interface FilerCommandsDeps {
  adapter: FilerAdapter;
  session: SessionOverrides;
  /** `--filer-bin` flag name; defaults to `filer-bin`. */
  binFlag?: string;
  /** `--filer-root` flag name; defaults to `filer-root`. */
  rootFlag?: string;
}

export interface FilerCommandRuntime {
  flagBin?: string;
  flagRoot?: string;
  resolved: Awaited<ReturnType<FilerAdapter["resolve"]>>;
  context: OperationContext;
}

async function resolveRuntime(
  pi: ExtensionAPI,
  deps: FilerCommandsDeps,
  ctx: ExtensionCommandContext,
): Promise<FilerCommandRuntime> {
  const flagBin = flagValue(pi.getFlag(deps.binFlag ?? "filer-bin"));
  const flagRoot = flagValue(pi.getFlag(deps.rootFlag ?? "filer-root"));
  const resolved = await deps.adapter.resolve({
    cwd: ctx.cwd,
    ...(flagBin === undefined ? {} : { flagBin }),
    ...(flagRoot === undefined ? {} : { flagRoot }),
  });
  return {
    ...(flagBin === undefined ? {} : { flagBin }),
    ...(flagRoot === undefined ? {} : { flagRoot }),
    resolved,
    context: { client: deps.adapter.client(resolved) },
  };
}

// --- Execution ------------------------------------------------------------

function notify(ctx: ExtensionCommandContext, message: string): void {
  ctx.ui.notify(message, "info");
}

function bundleDetails(
  data: {
    entries: readonly unknown[];
    depth: number;
    branch?: string;
  },
  warnings: readonly string[],
  truncation: unknown,
  newUnits: number | undefined,
): Record<string, unknown> {
  return {
    units: data.entries.length,
    depth: data.depth,
    ...(data.branch === undefined ? {} : { branch: data.branch }),
    ...(newUnits === undefined ? {} : { new: newUnits }),
    ...(truncation === undefined ? {} : { truncation }),
    warnings: [...warnings],
  };
}

async function runBundle(
  pi: ExtensionAPI,
  runtime: FilerCommandRuntime,
  ctx: ExtensionCommandContext,
  command: Extract<FilerCommand, { kind: "bundle" }>,
): Promise<void> {
  // The pre-read is only for `M new`; a failure drops that count, not the
  // command. `state --json` returns an empty envelope (exit 0) when no state
  // file exists, so `current: null` is a real "no previous units".
  let previous: StateEntry[] | undefined;
  try {
    const before = await state(runtime.context);
    previous = before.data.current?.entries ?? [];
  } catch {
    previous = undefined;
  }

  const result = await bundle(runtime.context, {
    seeds: command.seeds,
    ...(command.depth === undefined ? {} : { depth: command.depth }),
    ...(command.reset ? { reset: true } : {}),
    persist: true,
  });
  const units = result.data.entries.length;
  const newUnits =
    previous === undefined ? undefined : countNewUnits(previous, result.data.entries);
  pi.sendMessage(
    {
      customType: "filer-bundle",
      content: result.text,
      display: true,
      details: bundleDetails(result.data, result.warnings, result.truncation, newUnits),
    },
    { triggerTurn: false },
  );
  const summary =
    newUnits === undefined
      ? `filer: ${units} units`
      : `filer: ${units} units, ${newUnits} new`;
  notify(ctx, summary);
  setStatus(ctx, `${units} units · ${result.data.root}`);
}

async function runShow(
  pi: ExtensionAPI,
  runtime: FilerCommandRuntime,
  ctx: ExtensionCommandContext,
  command: Extract<FilerCommand, { kind: "show" }>,
): Promise<void> {
  const result = await show(
    runtime.context,
    command.branch === undefined ? {} : { branch: command.branch },
  );
  const units = result.data.entries.length;
  pi.sendMessage(
    {
      customType: "filer-bundle",
      content: result.text,
      display: true,
      details: bundleDetails(result.data, result.warnings, result.truncation, undefined),
    },
    { triggerTurn: false },
  );
  notify(ctx, `filer: ${units} units`);
  setStatus(ctx, `${units} units · ${result.data.root}`);
}

async function runState(
  pi: ExtensionAPI,
  runtime: FilerCommandRuntime,
): Promise<void> {
  const result = await state(runtime.context);
  pi.sendMessage(
    {
      customType: "filer-state",
      content: result.text,
      display: true,
      details: {
        branch: result.data.branch,
        label: result.data.label,
        exists: result.data.exists,
        entries: result.data.current?.entries.length ?? 0,
        warnings: result.warnings,
      },
    },
    { triggerTurn: false },
  );
}

async function runIndex(
  pi: ExtensionAPI,
  runtime: FilerCommandRuntime,
): Promise<void> {
  const result = await indexOperation(runtime.context);
  pi.sendMessage(
    {
      customType: "filer-index",
      content: result.text,
      display: true,
      details: {
        source_files: result.data.source_files,
        coverage: result.data.coverage ?? null,
        warnings: result.warnings,
      },
    },
    { triggerTurn: false },
  );
}

async function runWhy(
  pi: ExtensionAPI,
  runtime: FilerCommandRuntime,
  command: Extract<FilerCommand, { kind: "why" }>,
): Promise<void> {
  const result = await why(runtime.context, { unit: command.unit });
  pi.sendMessage(
    {
      customType: "filer-why",
      content: whyChainText(result.data),
      display: true,
      details: {
        target: result.data.target,
        reason: result.data.reason,
        depth: result.data.depth,
        warnings: result.warnings,
      },
    },
    { triggerTurn: false },
  );
}

async function runReset(
  runtime: FilerCommandRuntime,
  ctx: ExtensionCommandContext,
  command: Extract<FilerCommand, { kind: "reset" }>,
): Promise<void> {
  if (ctx.hasUI) {
    const message = command.all
      ? "Clear every saved bundle?"
      : "Clear the saved bundle for this branch?";
    const confirmed = await ctx.ui.confirm("filer reset", message);
    if (!confirmed) return;
  }
  const result = await state(runtime.context, { clear: true, all: command.all });
  const cleared = result.data.cleared;
  notify(
    ctx,
    typeof cleared === "string" ? `filer: cleared ${cleared}` : "filer: nothing to clear",
  );
  setStatus(ctx, undefined);
}

async function runBin(
  deps: FilerCommandsDeps,
  runtime: FilerCommandRuntime,
  ctx: ExtensionCommandContext,
  command: Extract<FilerCommand, { kind: "bin" }>,
): Promise<void> {
  const overrides = deps.session.get();
  if (command.path === undefined) {
    const suffix =
      overrides.bin === undefined ? "" : ` (session override: ${overrides.bin})`;
    notify(ctx, `filer binary: ${runtime.resolved.bin}${suffix}`);
    return;
  }
  const bin = normalizeBin(command.path, ctx.cwd);
  const check = await deps.adapter
    .client({ ...runtime.resolved, bin, timeoutMs: VERSION_TIMEOUT_MS })
    .run(["--version"]);
  const outcome = classifyRun(check);
  if (!outcome.ok) throw outcome.error;
  if (outcome.empty || !/\bfiler\b/i.test(check.stdout)) {
    throw new FilerToolError(
      "not_filer",
      `${bin} --version did not print a filer version`,
      { hint: "check the path; it must be the filer binary" },
    );
  }
  deps.session.setBin(bin);
  notify(ctx, `filer binary set to ${bin} (session)`);
}

async function runRoot(
  deps: FilerCommandsDeps,
  runtime: FilerCommandRuntime,
  ctx: ExtensionCommandContext,
  command: Extract<FilerCommand, { kind: "root" }>,
): Promise<void> {
  const overrides = deps.session.get();
  if (command.path === undefined) {
    const suffix =
      overrides.root === undefined ? "" : ` (session override: ${overrides.root})`;
    notify(ctx, `filer root: ${runtime.resolved.root}${suffix}`);
    return;
  }
  const root = resolvePath(ctx.cwd, command.path);
  const info = await stat(root).catch(() => undefined);
  if (info === undefined || !info.isDirectory()) {
    throw new FilerToolError("root_not_directory", `not a directory: ${root}`, {
      hint: "pass a directory that exists",
    });
  }
  deps.session.setRoot(root);
  notify(ctx, `filer root set to ${root} (session)`);
  // The previous status line described the old root; drop it.
  setStatus(ctx, undefined);
}

async function runHelp(
  pi: ExtensionAPI,
  deps: FilerCommandsDeps,
  runtime: FilerCommandRuntime,
): Promise<void> {
  let branch = "unknown";
  try {
    branch = (await state(runtime.context)).data.branch;
  } catch {
    // A missing binary still gets the rest of the help.
  }
  const overrides = deps.session.get();
  const binSuffix =
    overrides.bin === undefined ? "" : ` (session override: ${overrides.bin})`;
  const rootSuffix =
    overrides.root === undefined ? "" : ` (session override: ${overrides.root})`;
  const text = [
    "filer commands:",
    "  /filer bundle <seed...> [--depth N] [--reset]",
    "  /filer show [branch]",
    "  /filer state",
    "  /filer reset [--all]",
    "  /filer index",
    "  /filer why <unit>",
    "  /filer bin [path]",
    "  /filer root [path]",
    "",
    `binary: ${runtime.resolved.bin}${binSuffix}`,
    `root: ${runtime.resolved.root}${rootSuffix}`,
    `branch: ${branch}`,
    `schema: ${SUPPORTED_SCHEMA_VERSION}`,
  ].join("\n");
  pi.sendMessage(
    { customType: "filer-help", content: text, display: true },
    { triggerTurn: false },
  );
}

/** Execute one parsed `/filer` invocation; failures surface as notifications. */
export async function runFilerCommand(
  pi: ExtensionAPI,
  deps: FilerCommandsDeps,
  command: FilerCommand,
  ctx: ExtensionCommandContext,
): Promise<void> {
  if (command.kind === "invalid") {
    notifyError(ctx, new FilerToolError("invalid_arguments", command.message));
    return;
  }
  if (command.kind === "unknown") {
    notifyError(
      ctx,
      new FilerToolError(
        "unknown_command",
        `unknown subcommand \`${command.subcommand}\``,
        { hint: "run /filer for the list" },
      ),
    );
    return;
  }

  const runtime = await resolveRuntime(pi, deps, ctx);
  switch (command.kind) {
    case "help":
      await runHelp(pi, deps, runtime);
      return;
    case "bundle":
      await runBundle(pi, runtime, ctx, command);
      return;
    case "show":
      await runShow(pi, runtime, ctx, command);
      return;
    case "state":
      await runState(pi, runtime);
      return;
    case "index":
      await runIndex(pi, runtime);
      return;
    case "why":
      await runWhy(pi, runtime, command);
      return;
    case "reset":
      await runReset(runtime, ctx, command);
      return;
    case "bin":
      await runBin(deps, runtime, ctx, command);
      return;
    case "root":
      await runRoot(deps, runtime, ctx, command);
      return;
  }
}

/** Register `/filer` and the session handlers that persist overrides. */
export function registerFilerCommands(
  pi: ExtensionAPI,
  deps: FilerCommandsDeps,
): void {
  pi.registerCommand("filer", {
    description:
      "Bundle source files and their import/call closure with the filer CLI",
    handler: async (args, ctx) => {
      const command = parseFilerCommand(args);
      try {
        await runFilerCommand(pi, deps, command, ctx);
      } catch (error) {
        if (error instanceof FilerToolError && error.code === "filer_not_found") {
          setStatus(ctx, undefined);
        }
        notifyError(ctx, error);
      }
    },
  });

  // Restore before the tools' version check: `index.ts` registers the
  // commands first so a session `/filer bin` is honored on reload.
  pi.on("session_start", async (_event, ctx) => {
    deps.session.restore(ctx.sessionManager.getBranch());
    setStatus(ctx, undefined);
  });
  pi.on("session_tree", async (_event, ctx) => {
    deps.session.restore(ctx.sessionManager.getBranch());
  });
}
