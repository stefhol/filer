//! The eight model-callable filer tools (#41).
//!
//! Thin wrappers over `src/operations.ts`: argument mapping, spawning and the
//! JSON guards live there. This module maps typed parameters onto one operation
//! each and the result onto pi's tool shape:
//!
//! - `content` is the model-facing text (rebuilt markdown, or rows);
//! - `details` is the render/state payload (counts, warnings, truncation);
//! - `structuredContent` is the parsed envelope for the data tools.
//!
//! Every call passes `--json --quiet` (see `src/args.ts`), resolves the binary
//! and root through the adapter, and runs with `cwd = ctx.cwd`. Failures become
//! clean `isError` results built from `code` + `hint`; raw stderr is never
//! forwarded.

import type {
  AgentToolResult,
  ExtensionAPI,
  ExtensionToolContext,
  ToolAnnotations,
  ToolDefinition,
  ToolNamespace,
} from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";
import { Type, type TSchema } from "typebox";
import type { FilerAdapter } from "../index";
import { FilerToolError } from "./errors";
import { capText, type Truncation } from "./format";
import {
  bundle,
  callers,
  deps as depsOperation,
  refs,
  search,
  show,
  state,
  why,
  type BundleEnvelope,
  type OperationContext,
  type OperationResult,
  type SearchRow,
} from "./operations";

/** Prefix every tool name; the session-start guard removes all of them. */
export const FILER_TOOL_PREFIX = "filer_";
/** How long `filer --version` may take at session start. */
export const VERSION_TIMEOUT_MS = 5_000;
/** Shown when the binary is missing or unusable. */
export const FILER_INSTALL_HINT =
  "filer not found — install with cargo install --path . or set FILER_BIN";

const FILER_NAMESPACE: ToolNamespace = {
  name: "filer",
  description:
    "Bundle source files and their import/call closure with the filer CLI",
};

const READ_ONLY: ToolAnnotations = {
  readOnlyHint: true,
  destructiveHint: false,
  openWorldHint: false,
};

// A persisting bundle writes `<root>/.filer/bundle.toml`; it only adds or
// updates the current branch's entry, so it is not destructive.
const BUNDLE_ANNOTATIONS: ToolAnnotations = {
  readOnlyHint: false,
  destructiveHint: false,
  openWorldHint: false,
};

export interface FilerToolsDeps {
  adapter: FilerAdapter;
  /** `--filer-bin` flag name; defaults to the value registered in `index.ts`. */
  binFlag?: string;
  /** `--filer-root` flag name. */
  rootFlag?: string;
}

/** The details every tool returns, plus what the renderer prints. */
export interface FilerToolDetails {
  /** One-line summary for `renderResult`. */
  summary: string;
  /** Parsed `{"warning":"..."}` stderr lines. */
  warnings: string[];
  /** Present only when the model-facing text was cut for size. */
  truncation?: Truncation;
  error?: {
    code: string;
    message: string;
    hint?: string;
    details?: Record<string, unknown>;
  };
}

/** `filer_bundle` / `filer_show` details carry the envelope's metadata. */
export interface BundleToolDetails extends FilerToolDetails {
  units: number;
  depth: number;
  branch?: string;
  budget: BundleEnvelope["budget"];
  omitted: BundleEnvelope["omitted"];
  diagnostics: BundleEnvelope["diagnostics"];
  coverage: BundleEnvelope["coverage"];
}

type StructuredContent = NonNullable<AgentToolResult<unknown>["structuredContent"]>;

function asStructured(value: unknown): StructuredContent {
  return value as StructuredContent;
}

function textContent(text: string): AgentToolResult<unknown>["content"] {
  return [{ type: "text", text }];
}

function searchRowLine(row: SearchRow): string {
  const target =
    row.symbol === null
      ? row.path
      : row.member === null
        ? `${row.path}::${row.symbol}`
        : `${row.path}::${row.symbol}.${row.member}`;
  const where =
    row.line === null ? `lines ${row.start_line}-${row.end_line}` : `line ${row.line}`;
  return `${target} — ${row.fqn} (${row.kind}, ${where}, ${
    row.included ? "in bundle" : "not in bundle"
  })`;
}

function truncationDetails(
  truncation: Truncation | undefined,
): { truncation?: Truncation } {
  return truncation?.truncated === true ? { truncation } : {};
}

function errorDetails(error: unknown): NonNullable<FilerToolDetails["error"]> {
  if (error instanceof FilerToolError) {
    return {
      code: error.code,
      message: error.message,
      ...(error.hint === undefined ? {} : { hint: error.hint }),
      ...(error.details === undefined ? {} : { details: error.details }),
    };
  }
  return {
    code: "filer_extension_error",
    message: error instanceof Error ? error.message : String(error),
  };
}

/**
 * A failed tool result. `isError` marks it for the model; the text is only
 * `code: hint` (or a clean extension error), never a stderr line or a stack.
 */
function failure<D extends FilerToolDetails>(error: unknown): AgentToolResult<D> {
  const details = errorDetails(error);
  const text =
    error instanceof FilerToolError
      ? error.toToolMessage()
      : `${details.code}: ${details.message}`;
  return {
    content: textContent(text),
    details: { summary: text, warnings: [], error: details } as unknown as D,
    isError: true,
  };
}

function success<D extends FilerToolDetails>(
  text: string,
  details: D,
  data?: unknown,
): AgentToolResult<D> {
  const result: AgentToolResult<D> = { content: textContent(text), details };
  if (data !== undefined) result.structuredContent = asStructured(data);
  return result;
}

// --- Parameter schemas ----------------------------------------------------

const BundleParams = Type.Object({
  seeds: Type.Array(
    Type.String({
      description: "Path, `path::Symbol`, or `path::Class.method` to bundle",
    }),
    { minItems: 1, description: "Seeds to bundle plus their import/call closure" },
  ),
  depth: Type.Optional(
    Type.Integer({ minimum: 0, description: "Import hops to follow (default 2)" }),
  ),
  reset: Type.Optional(
    Type.Boolean({ description: "Start from an empty bundle instead of the saved state" }),
  ),
  persist: Type.Optional(
    Type.Boolean({
      description:
        "Carry the previous bundle over from <root>/.filer/bundle.toml and save the result",
    }),
  ),
  withConfig: Type.Optional(
    Type.Boolean({
      description: "Also include build files (pom.xml, Cargo.toml, tsconfig.json, ...)",
    }),
  ),
  title: Type.Optional(Type.String({ description: "Bundle title in the header" })),
});

const ShowParams = Type.Object({
  branch: Type.Optional(
    Type.String({ description: "Branch whose saved bundle to show (default: current)" }),
  ),
});

const SearchParams = Type.Object({
  query: Type.String({ description: "Matched against FQN, path and symbol names" }),
  limit: Type.Optional(
    Type.Integer({ minimum: 1, description: "Maximum rows (CLI default 200)" }),
  ),
});

const DepsParams = Type.Object({
  seed: Type.String({ description: "Unit: `path`, `path::Symbol`, `path::Class.method`" }),
  unit: Type.Optional(
    Type.Boolean({ description: "Return units instead of collapsing to files" }),
  ),
  depth: Type.Optional(
    Type.Integer({ minimum: 0, description: "Import/call hops to follow (default 2)" }),
  ),
});

const CallersParams = Type.Object({
  unit: Type.String({ description: "Unit whose callers to invert" }),
  depth: Type.Optional(
    Type.Integer({ minimum: 1, description: "Caller hops to follow (default 1)" }),
  ),
});

const WhyParams = Type.Object({
  unit: Type.String({ description: "Unit to explain against the saved bundle" }),
});

const RefsParams = Type.Object({
  name: Type.String({ description: "Symbol or member name to invert" }),
});

const StateParams = Type.Object({});

// --- Output schemas for the data-shaped tools -----------------------------
// Additive CLI fields are allowed (objects leave `additionalProperties`
// open), so a newer binary does not fail a tool's own output schema.

const NullableString = Type.Union([Type.String(), Type.Null()]);
const NullableNumber = Type.Union([Type.Number(), Type.Null()]);

const SearchRowSchema = Type.Object({
  schema_version: Type.Number(),
  path: Type.String(),
  scope: Type.String(),
  symbol: NullableString,
  member: NullableString,
  kind: Type.String(),
  fqn: Type.String(),
  line: NullableNumber,
  start_line: Type.Number(),
  end_line: Type.Number(),
  bytes: Type.Number(),
  est_tokens: Type.Number(),
  package: Type.Optional(NullableString),
  included: Type.Boolean(),
  depth: NullableNumber,
  reason: NullableString,
});

const SearchOutput = Type.Object({
  schema_version: Type.Number(),
  query: Type.String(),
  results: Type.Number(),
  rows: Type.Array(SearchRowSchema),
  hint: Type.Optional(Type.String()),
});

const DepsOutput = Type.Object({
  schema_version: Type.Number(),
  seed: Type.String(),
  granularity: Type.String(),
  deps: Type.Array(
    Type.Object({
      path: Type.String(),
      fqn: Type.String(),
      depth: Type.Number(),
      reason: Type.String(),
      unit: Type.Optional(Type.String()),
      scope: Type.Optional(Type.String()),
      symbol: Type.Optional(NullableString),
      member: Type.Optional(NullableString),
    }),
  ),
  edges: Type.Array(
    Type.Object({
      from: Type.String(),
      to: Type.String(),
      reason: Type.String(),
      line: NullableNumber,
    }),
  ),
  diagnostics: Type.Array(
    Type.Object({ kind: Type.String(), path: Type.String() }),
  ),
});

const CallersOutput = Type.Object({
  schema_version: Type.Number(),
  unit: Type.String(),
  rows: Type.Array(
    Type.Object({
      caller: Type.String(),
      line: Type.Number(),
      depth: Type.Number(),
    }),
  ),
});

const WhyOutput = Type.Object({
  schema_version: Type.Number(),
  target: Type.String(),
  reason: Type.String(),
  depth: Type.Number(),
  chain: Type.Array(
    Type.Object({
      unit: Type.String(),
      path: NullableString,
      scope: Type.String(),
      symbol: NullableString,
      member: NullableString,
      line: NullableNumber,
      reason: Type.String(),
      depth: Type.Number(),
    }),
  ),
});

const RefsOutput = Type.Object({
  schema_version: Type.Number(),
  name: Type.String(),
  rows: Type.Array(
    Type.Object({
      path: Type.String(),
      symbol: Type.String(),
      member: NullableString,
      line: Type.Number(),
      kind: Type.String(),
    }),
  ),
});

const StateOutput = Type.Object({
  schema_version: Type.Number(),
  root: Type.String(),
  state_path: Type.String(),
  branch: Type.String(),
  label: Type.String(),
  exists: Type.Boolean(),
  current: Type.Union([
    Type.Object({
      branch: Type.String(),
      created: Type.Number(),
      depth: Type.Number(),
      entries: Type.Array(
        Type.Object({
          path: Type.String(),
          scope: Type.String(),
          symbol: NullableString,
          methods: Type.Array(Type.String()),
          depth: Type.Number(),
          reason: Type.String(),
        }),
      ),
    }),
    Type.Null(),
  ]),
  branches: Type.Array(
    Type.Object({
      branch: Type.String(),
      label: Type.String(),
      entries: Type.Number(),
      created: Type.Number(),
      depth: Type.Number(),
    }),
  ),
});

// --- Registration ---------------------------------------------------------

function flagValue(value: boolean | string | undefined): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length === 0 ? undefined : trimmed;
}

function registerTool<P extends TSchema, D extends FilerToolDetails>(
  pi: ExtensionAPI,
  tool: ToolDefinition<P, D>,
): void {
  pi.registerTool(tool);
}

/**
 * Register the eight `filer_*` tools and the session-start usability check.
 *
 * The factory never spawns; the first lookup happens when a tool runs or when
 * pi emits `session_start`. A failed `filer --version` notifies once and
 * removes the `filer_*` tools from the active set, so calls fail fast instead
 * of once per turn (a `/reload` retries).
 */
export function registerFilerTools(pi: ExtensionAPI, deps: FilerToolsDeps): void {
  const binFlag = deps.binFlag ?? "filer-bin";
  const rootFlag = deps.rootFlag ?? "filer-root";

  const resolveClient = async (
    ctx: ExtensionToolContext,
    signal: AbortSignal | undefined,
  ): Promise<OperationContext> => {
    const flagBin = flagValue(pi.getFlag(binFlag));
    const flagRoot = flagValue(pi.getFlag(rootFlag));
    const resolved = await deps.adapter.resolve({
      cwd: ctx.cwd,
      ...(flagBin === undefined ? {} : { flagBin }),
      ...(flagRoot === undefined ? {} : { flagRoot }),
      ...(signal === undefined ? {} : { signal }),
    });
    return {
      client: deps.adapter.client(resolved),
      ...(signal === undefined ? {} : { signal }),
    };
  };

  const runTool = async <T>(
    ctx: ExtensionToolContext,
    signal: AbortSignal | undefined,
    operation: (context: OperationContext) => Promise<OperationResult<T>>,
  ): Promise<OperationResult<T>> => operation(await resolveClient(ctx, signal));

  registerTool(pi, {
    name: "filer_bundle",
    label: "filer bundle",
    description:
      "Bundle source files and their import/call closure into markdown. Seeds are `path`, `path::Symbol` or `path::Class.method`; `depth` follows imports (default 2). `persist` carries the previous bundle over from <root>/.filer/bundle.toml. Large bundles are truncated with a note; the details list units, budget, omitted units, diagnostics and coverage.",
    parameters: BundleParams,
    namespace: FILER_NAMESPACE,
    annotations: BUNDLE_ANNOTATIONS,
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      try {
        const result = await runTool(ctx, signal, (context) =>
          bundle(context, {
            seeds: params.seeds,
            ...(params.depth === undefined ? {} : { depth: params.depth }),
            ...(params.reset === undefined ? {} : { reset: params.reset }),
            ...(params.persist === undefined ? {} : { persist: params.persist }),
            ...(params.withConfig === undefined ? {} : { withConfig: params.withConfig }),
            ...(params.title === undefined ? {} : { title: params.title }),
          }),
        );
        const data = result.data;
        const details: BundleToolDetails = {
          summary: `filer bundle: ${data.entries.length} units, depth ${data.depth}${
            result.truncation?.truncated === true ? " (truncated)" : ""
          }`,
          warnings: result.warnings,
          ...truncationDetails(result.truncation),
          units: data.entries.length,
          depth: data.depth,
          ...(data.branch === undefined ? {} : { branch: data.branch }),
          budget: data.budget,
          omitted: data.omitted,
          diagnostics: data.diagnostics,
          coverage: data.coverage,
        };
        return success(result.text, details);
      } catch (error) {
        return failure(error);
      }
    },
    renderCall(args, theme) {
      const seeds = args.seeds.join(" ");
      return new Text(
        theme.fg("toolTitle", theme.bold("filer bundle ")) + theme.fg("accent", seeds),
        0,
        0,
      );
    },
    renderResult(result, _options, theme) {
      return new Text(
        theme.fg(result.isError === true ? "error" : "dim", result.details.summary),
        0,
        0,
      );
    },
  });

  registerTool(pi, {
    name: "filer_show",
    label: "filer show",
    description:
      "Re-render the saved bundle for a branch from <root>/.filer/bundle.toml without resolving anything. Large bundles are truncated with a note.",
    parameters: ShowParams,
    namespace: FILER_NAMESPACE,
    annotations: READ_ONLY,
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      try {
        const result = await runTool(ctx, signal, (context) =>
          show(context, params.branch === undefined ? {} : { branch: params.branch }),
        );
        const data = result.data;
        const details: BundleToolDetails = {
          summary: `filer show: ${data.entries.length} units, depth ${data.depth}${
            result.truncation?.truncated === true ? " (truncated)" : ""
          }`,
          warnings: result.warnings,
          ...truncationDetails(result.truncation),
          units: data.entries.length,
          depth: data.depth,
          ...(data.branch === undefined ? {} : { branch: data.branch }),
          budget: data.budget,
          omitted: data.omitted,
          diagnostics: data.diagnostics,
          coverage: data.coverage,
        };
        return success(result.text, details);
      } catch (error) {
        return failure(error);
      }
    },
    renderCall(args, theme) {
      return new Text(
        theme.fg("toolTitle", theme.bold("filer show ")) +
          theme.fg("accent", args.branch ?? "current"),
        0,
        0,
      );
    },
    renderResult(result, _options, theme) {
      return new Text(
        theme.fg(result.isError === true ? "error" : "dim", result.details.summary),
        0,
        0,
      );
    },
  });

  registerTool(pi, {
    name: "filer_search",
    label: "filer search",
    description:
      "Search the symbol/path index (not file contents) and return one row per unit: path, FQN, kind, line/span and whether it is already in the saved bundle. An empty result is a successful answer with the CLI's hint.",
    parameters: SearchParams,
    namespace: FILER_NAMESPACE,
    annotations: READ_ONLY,
    outputSchema: SearchOutput,
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      try {
        const result = await runTool(ctx, signal, (context) =>
          search(context, {
            query: params.query,
            ...(params.limit === undefined ? {} : { limit: params.limit }),
          }),
        );
        // The model-facing rows must carry path, FQN, kind, line/span and
        // inclusion; the structured envelope carries the same data in full.
        const rows = result.data.rows.map(searchRowLine).join("\n");
        const capped = capText(rows.length === 0 ? result.text : rows);
        return success(
          capped.text,
          {
            summary: `filer search: ${result.data.results} rows`,
            warnings: result.warnings,
            ...truncationDetails(capped.truncation),
          },
          result.data,
        );
      } catch (error) {
        return failure(error);
      }
    },
    renderCall(args, theme) {
      return new Text(
        theme.fg("toolTitle", theme.bold("filer search ")) +
          theme.fg("accent", args.query),
        0,
        0,
      );
    },
    renderResult(result, _options, theme) {
      return new Text(
        theme.fg(result.isError === true ? "error" : "dim", result.details.summary),
        0,
        0,
      );
    },
  });

  registerTool(pi, {
    name: "filer_deps",
    label: "filer deps",
    description:
      "Print the import/call closure of one seed as deps rows plus the materialized edges and diagnostics. `unit: true` keeps units (`path::Symbol`, `path::Class.method`) instead of collapsing to files.",
    parameters: DepsParams,
    namespace: FILER_NAMESPACE,
    annotations: READ_ONLY,
    outputSchema: DepsOutput,
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      try {
        const result = await runTool(ctx, signal, (context) =>
          depsOperation(context, {
            seed: params.seed,
            ...(params.unit === undefined ? {} : { unit: params.unit }),
            ...(params.depth === undefined ? {} : { depth: params.depth }),
          }),
        );
        return success(
          result.text,
          {
            summary: `filer deps: ${result.data.deps.length} units, ${result.data.edges.length} edges`,
            warnings: result.warnings,
            ...truncationDetails(result.truncation),
          },
          result.data,
        );
      } catch (error) {
        return failure(error);
      }
    },
    renderCall(args, theme) {
      return new Text(
        theme.fg("toolTitle", theme.bold("filer deps ")) +
          theme.fg("accent", args.seed),
        0,
        0,
      );
    },
    renderResult(result, _options, theme) {
      return new Text(
        theme.fg(result.isError === true ? "error" : "dim", result.details.summary),
        0,
        0,
      );
    },
  });

  registerTool(pi, {
    name: "filer_callers",
    label: "filer callers",
    description:
      "List who calls a unit, nearest first. `depth` 1 is direct callers (default); 2 follows callers of callers. Only call edges count, not imports.",
    parameters: CallersParams,
    namespace: FILER_NAMESPACE,
    annotations: READ_ONLY,
    outputSchema: CallersOutput,
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      try {
        const result = await runTool(ctx, signal, (context) =>
          callers(context, {
            unit: params.unit,
            ...(params.depth === undefined ? {} : { depth: params.depth }),
          }),
        );
        return success(
          result.text,
          {
            summary: `filer callers: ${result.data.rows.length} callers`,
            warnings: result.warnings,
            ...truncationDetails(result.truncation),
          },
          result.data,
        );
      } catch (error) {
        return failure(error);
      }
    },
    renderCall(args, theme) {
      return new Text(
        theme.fg("toolTitle", theme.bold("filer callers ")) +
          theme.fg("accent", args.unit),
        0,
        0,
      );
    },
    renderResult(result, _options, theme) {
      return new Text(
        theme.fg(result.isError === true ? "error" : "dim", result.details.summary),
        0,
        0,
      );
    },
  });

  registerTool(pi, {
    name: "filer_why",
    label: "filer why",
    description:
      "Explain how a unit entered this branch's saved bundle: the reason, the depth, and the chain back to the seed that pulled it in. Reads the state file; it does not resolve.",
    parameters: WhyParams,
    namespace: FILER_NAMESPACE,
    annotations: READ_ONLY,
    outputSchema: WhyOutput,
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      try {
        const result = await runTool(ctx, signal, (context) =>
          why(context, { unit: params.unit }),
        );
        return success(
          result.text,
          {
            summary: `filer why: ${result.data.chain.length} hops, ${result.data.reason}`,
            warnings: result.warnings,
            ...truncationDetails(result.truncation),
          },
          result.data,
        );
      } catch (error) {
        return failure(error);
      }
    },
    renderCall(args, theme) {
      return new Text(
        theme.fg("toolTitle", theme.bold("filer why ")) +
          theme.fg("accent", args.unit),
        0,
        0,
      );
    },
    renderResult(result, _options, theme) {
      return new Text(
        theme.fg(result.isError === true ? "error" : "dim", result.details.summary),
        0,
        0,
      );
    },
  });

  registerTool(pi, {
    name: "filer_refs",
    label: "filer refs",
    description:
      "Invert the uses and calls the resolver recorded for a name. Each row is path, symbol/member, line and `kind` (`use` or `call`); bodies, comments and string literals are out of scope.",
    parameters: RefsParams,
    namespace: FILER_NAMESPACE,
    annotations: READ_ONLY,
    outputSchema: RefsOutput,
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      try {
        const result = await runTool(ctx, signal, (context) =>
          refs(context, { name: params.name }),
        );
        return success(
          result.text,
          {
            summary: `filer refs: ${result.data.rows.length} references`,
            warnings: result.warnings,
            ...truncationDetails(result.truncation),
          },
          result.data,
        );
      } catch (error) {
        return failure(error);
      }
    },
    renderCall(args, theme) {
      return new Text(
        theme.fg("toolTitle", theme.bold("filer refs ")) +
          theme.fg("accent", args.name),
        0,
        0,
      );
    },
    renderResult(result, _options, theme) {
      return new Text(
        theme.fg(result.isError === true ? "error" : "dim", result.details.summary),
        0,
        0,
      );
    },
  });

  registerTool(pi, {
    name: "filer_state",
    label: "filer state",
    description:
      "Show what the saved bundle state holds: the state file, the current branch, every stored branch, and the current branch's entries (path, scope, depth, reason). Read-only; clearing stays a `/filer reset` command.",
    parameters: StateParams,
    namespace: FILER_NAMESPACE,
    annotations: READ_ONLY,
    outputSchema: StateOutput,
    async execute(_toolCallId, _params, signal, _onUpdate, ctx) {
      try {
        const result = await runTool(ctx, signal, (context) => state(context));
        return success(
          result.text,
          {
            summary: `filer state: ${result.data.branches.length} branches, ${result.data.current?.entries.length ?? 0} current entries`,
            warnings: result.warnings,
            ...truncationDetails(result.truncation),
          },
          result.data,
        );
      } catch (error) {
        return failure(error);
      }
    },
    renderCall(_args, theme) {
      return new Text(theme.fg("toolTitle", theme.bold("filer state")), 0, 0);
    },
    renderResult(result, _options, theme) {
      return new Text(
        theme.fg(result.isError === true ? "error" : "dim", result.details.summary),
        0,
        0,
      );
    },
  });

  // `index` stays an operation for the commands follow-up (#42): as a tool it
  // would duplicate the coverage `filer_bundle` already returns.

  pi.on("session_start", async (_event, ctx) => {
    const flagBin = flagValue(pi.getFlag(binFlag));
    const flagRoot = flagValue(pi.getFlag(rootFlag));
    const resolved = await deps.adapter.resolve({
      cwd: ctx.cwd,
      ...(flagBin === undefined ? {} : { flagBin }),
      ...(flagRoot === undefined ? {} : { flagRoot }),
    });
    const check = await deps.adapter
      .client({ ...resolved, timeoutMs: VERSION_TIMEOUT_MS })
      .run(["--version"]);
    if (check.code === 0 && /\bfiler\b/i.test(check.stdout)) return;

    ctx.ui.notify(
      `${FILER_INSTALL_HINT} (tried \`${resolved.bin}\`)`,
      "error",
    );
    pi.setActiveTools(
      pi.getActiveTools().filter((name) => !name.startsWith(FILER_TOOL_PREFIX)),
    );
  });
}
