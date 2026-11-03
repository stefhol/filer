//! Typed operations over the `filer` JSON surface.
//!
//! One function per command. Each builds an argv array, runs the binary,
//! classifies the exit status, guards `schema_version`, and returns
//! `{ data, text, warnings }`: `data` is the parsed envelope, `text` is what the
//! model reads (rebuilt markdown or one line per row), and `warnings` are the
//! parsed `{"warning":"..."}` lines from stderr.
//!
//! Every operation returns the same shapes the README documents; no resolution,
//! slicing or rendering happens in TypeScript.

import {
  bundleArgs,
  callersArgs,
  depsArgs,
  indexArgs,
  refsArgs,
  searchArgs,
  showArgs,
  stateArgs,
  whyArgs,
  type BundleArgs,
  type CallersArgs,
  type DepsArgs,
  type RefsArgs,
  type SearchArgs,
  type ShowArgs,
  type StateArgs,
  type WhyArgs,
} from "./args";
import type { FilerClient } from "./cli";
import { FilerToolError, classifyRun } from "./errors";
import {
  SUPPORTED_SCHEMA_VERSION,
  assertRowSchemaVersions,
  assertSchemaVersion,
  assertSchemaVersionIfPresent,
} from "./schema";
import {
  capText,
  diagnosticsLine,
  diagnosticsSummary,
  formatBundle,
  type Truncation,
} from "./format";
import {
  createSeedFilesystem,
  expandDepsSeed,
  expandDirectorySeed,
  type SeedFilesystem,
} from "./seeds";

// --- Wire types -----------------------------------------------------------
// Only the fields the adapter reads are named. Everything else stays in `data`
// so a tool can forward the envelope without a lossy round-trip.

export interface BundleBudget {
  max_file_bytes: number | null;
  max_bytes: number | null;
  max_tokens: number | null;
  used_bytes: number;
  used_tokens: number;
  truncated: boolean;
}

export interface BundleEntryWire {
  path: string;
  scope: string;
  symbol: string | null;
  methods: string[];
  depth: number;
  reason: string;
  start_line: number | null;
  end_line: number | null;
  start_byte: number | null;
  end_byte: number | null;
  tokens: number;
  hash?: string | null;
  rendered: boolean;
  label: string;
  markdown: string;
}

export interface OmittedWire {
  path: string;
  scope: string;
  symbol: string | null;
  reason: string;
}

export interface DiagnosticWire {
  kind: string;
  path: string;
  [payload: string]: unknown;
}

export interface Coverage {
  files_indexed: number;
  files_skipped: number;
  imports_unresolved: number;
}

export interface BundleEnvelope {
  schema_version: number;
  root: string;
  title: string;
  branch?: string;
  depth: number;
  files: number;
  excerpts: number;
  methods: number;
  budget: BundleBudget;
  entries: BundleEntryWire[];
  omitted: OmittedWire[];
  diagnostics: DiagnosticWire[];
  coverage: Coverage;
  state_toml?: string | null;
}

export interface SearchRow {
  schema_version: number;
  path: string;
  scope: string;
  symbol: string | null;
  member: string | null;
  kind: string;
  fqn: string;
  line: number | null;
  start_line: number;
  end_line: number;
  bytes: number;
  est_tokens: number;
  package: string | null;
  included: boolean;
  depth: number | null;
  reason: string | null;
}

export interface SearchData {
  schema_version: number;
  query: string;
  results: number;
  rows: SearchRow[];
  hint?: string;
}

export interface DepsRow {
  path: string;
  fqn: string;
  depth: number;
  reason: string;
  unit?: string;
  scope?: string;
  symbol?: string | null;
  member?: string | null;
}

export interface DepsEdge {
  from: string;
  to: string;
  reason: string;
  line: number | null;
}

export interface DepsEnvelope {
  schema_version: number;
  seed: string;
  granularity: string;
  deps: DepsRow[];
  edges: DepsEdge[];
  diagnostics: DiagnosticWire[];
}

export interface CallerRow {
  caller: string;
  line: number;
  depth: number;
}

export interface CallersData {
  schema_version: number;
  unit: string;
  rows: CallerRow[];
}

export interface WhyHop {
  unit: string;
  path: string | null;
  scope: string;
  symbol: string | null;
  member: string | null;
  line: number | null;
  reason: string;
  depth: number;
}

export interface WhyData {
  schema_version: number;
  target: string;
  reason: string;
  depth: number;
  chain: WhyHop[];
}

export interface RefRow {
  path: string;
  symbol: string;
  member: string | null;
  line: number;
  kind: string;
}

export interface RefsData {
  schema_version: number;
  name: string;
  rows: RefRow[];
}

export interface StateEntry {
  path: string;
  scope: string;
  symbol: string | null;
  methods: string[];
  depth: number;
  reason: string;
}

export interface StateCurrent {
  branch: string;
  created: number;
  depth: number;
  entries: StateEntry[];
}

export interface StateBranchSummary {
  branch: string;
  label: string;
  entries: number;
  created: number;
  depth: number;
}

export interface StateEnvelope {
  schema_version: number;
  root: string;
  state_path: string;
  branch: string;
  label: string;
  exists: boolean;
  current: StateCurrent | null;
  branches: StateBranchSummary[];
  cleared?: unknown;
}

export interface IndexEnvelope {
  schema_version: number;
  root: string;
  source_files: number;
  languages: Array<{ language: string; files: number }>;
  packages: number;
  types: number;
  symbols: number;
  cache: { reused: number; parsed: number };
  state: unknown;
  coverage?: Coverage;
}

// --- Result plumbing ------------------------------------------------------

export interface OperationContext {
  client: FilerClient;
  signal?: AbortSignal;
  /** Directory-seed listing; defaults to the real filesystem. */
  seedFilesystem?: SeedFilesystem;
}

/** The seed filesystem a context uses: the injected fake or the real one. */
function seedFilesystem(context: OperationContext): SeedFilesystem {
  return context.seedFilesystem ?? createSeedFilesystem();
}

export interface OperationResult<T> {
  data: T;
  /** Model-facing text: rebuilt markdown, or one line per row. */
  text: string;
  /** Parsed `{"warning":"..."}` stderr lines. */
  warnings: string[];
  /** Present only when the text was cut for size. */
  truncation?: Truncation;
  /** The CLI's hint for an empty (exit `4`) result. */
  hint?: string;
}

interface Executed {
  stdout: string;
  warnings: string[];
  empty: boolean;
  hint?: string;
}

async function execute(context: OperationContext, argv: string[]): Promise<Executed> {
  const result = await context.client.run(
    argv,
    context.signal === undefined ? {} : { signal: context.signal },
  );
  const outcome = classifyRun(result);
  if (!outcome.ok) throw outcome.error;
  return {
    stdout: result.stdout,
    warnings: outcome.warnings,
    empty: outcome.empty,
    ...(outcome.hint === undefined ? {} : { hint: outcome.hint }),
  };
}

function invalidJson(what: string): FilerToolError {
  return new FilerToolError(
    "invalid_json",
    `filer ${what} --json returned no JSON envelope`,
    { hint: "check that FILER_BIN points at a filer binary that supports --json" },
  );
}

function asRecord(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw invalidJson(what);
  }
  return value as Record<string, unknown>;
}

function parseObject(stdout: string, what: string): Record<string, unknown> {
  const text = stdout.trim();
  if (text.length === 0) throw invalidJson(what);
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw invalidJson(what);
  }
  return asRecord(value, what);
}

function parseArray(stdout: string, what: string): unknown[] {
  const text = stdout.trim();
  if (text.length === 0) return [];
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw invalidJson(what);
  }
  if (!Array.isArray(value)) throw invalidJson(what);
  return value;
}

function parseLines(stdout: string, what: string): unknown[] {
  const values: unknown[] = [];
  for (const line of stdout.split("\n")) {
    const text = line.trim();
    if (text.length === 0) continue;
    try {
      values.push(JSON.parse(text));
    } catch {
      throw invalidJson(what);
    }
  }
  return values;
}

function buildResult<T>(
  data: T,
  text: string,
  executed: Executed,
  truncation?: Truncation,
): OperationResult<T> {
  const result: OperationResult<T> = { data, text, warnings: executed.warnings };
  if (truncation?.truncated === true) result.truncation = truncation;
  if (executed.hint !== undefined) result.hint = executed.hint;
  return result;
}

function emptyBudget(): BundleBudget {
  return {
    max_file_bytes: null,
    max_bytes: null,
    max_tokens: null,
    used_bytes: 0,
    used_tokens: 0,
    truncated: false,
  };
}

function emptyCoverage(): Coverage {
  return { files_indexed: 0, files_skipped: 0, imports_unresolved: 0 };
}

function emptyBundle(
  root: string,
  options: { depth?: number; title?: string },
  executed: Executed,
): OperationResult<BundleEnvelope> {
  const data: BundleEnvelope = {
    schema_version: SUPPORTED_SCHEMA_VERSION,
    root,
    title: options.title ?? "",
    depth: options.depth ?? 2,
    files: 0,
    excerpts: 0,
    methods: 0,
    budget: emptyBudget(),
    entries: [],
    omitted: [],
    diagnostics: [],
    coverage: emptyCoverage(),
    state_toml: null,
  };
  const text = executed.hint === undefined ? "filer: nothing to bundle" : `filer: ${executed.hint}`;
  return buildResult(data, text, executed);
}

// --- Envelope coercion ----------------------------------------------------

function asBundleEnvelope(value: unknown): BundleEnvelope {
  const object = asRecord(value, "bundle");
  assertSchemaVersion(object, "bundle");
  return {
    ...(object as unknown as BundleEnvelope),
    entries: Array.isArray(object.entries) ? (object.entries as BundleEntryWire[]) : [],
    omitted: Array.isArray(object.omitted) ? (object.omitted as OmittedWire[]) : [],
    diagnostics: Array.isArray(object.diagnostics)
      ? (object.diagnostics as DiagnosticWire[])
      : [],
  };
}

function asDepsEnvelope(value: unknown): DepsEnvelope {
  const object = asRecord(value, "deps");
  assertSchemaVersion(object, "deps");
  return {
    ...(object as unknown as DepsEnvelope),
    deps: Array.isArray(object.deps) ? (object.deps as DepsRow[]) : [],
    edges: Array.isArray(object.edges) ? (object.edges as DepsEdge[]) : [],
    diagnostics: Array.isArray(object.diagnostics)
      ? (object.diagnostics as DiagnosticWire[])
      : [],
  };
}

function asStateEnvelope(value: unknown): StateEnvelope {
  const object = asRecord(value, "state");
  assertSchemaVersion(object, "state");
  return {
    ...(object as unknown as StateEnvelope),
    branches: Array.isArray(object.branches)
      ? (object.branches as StateBranchSummary[])
      : [],
    current:
      typeof object.current === "object" && object.current !== null
        ? (object.current as StateCurrent)
        : null,
  };
}

function asIndexEnvelope(value: unknown): IndexEnvelope {
  const object = asRecord(value, "index");
  assertSchemaVersion(object, "index");
  return {
    ...(object as unknown as IndexEnvelope),
    languages: Array.isArray(object.languages)
      ? (object.languages as Array<{ language: string; files: number }>)
      : [],
  };
}

// --- Text formatting -----------------------------------------------------

function searchRowText(row: SearchRow): string {
  const target =
    row.symbol !== null && row.symbol.length > 0
      ? row.member !== null && row.member.length > 0
        ? `${row.path}::${row.symbol}.${row.member}`
        : `${row.path}::${row.symbol}`
      : row.path;
  const line = row.line === null ? "" : ` line ${row.line}`;
  return `${target} (${row.kind}${line})`;
}

function depsRowText(row: DepsRow): string {
  const unit = row.unit ?? row.path;
  return `${unit} (depth ${row.depth}, ${row.reason})`;
}

function callerRowText(row: CallerRow): string {
  return `${row.caller} (line ${row.line}, depth ${row.depth})`;
}

function refRowText(row: RefRow): string {
  const target =
    row.member === null || row.member.length === 0
      ? `${row.path}::${row.symbol}`
      : `${row.path}::${row.symbol}.${row.member}`;
  return `${target} (${row.kind}, line ${row.line})`;
}

function whyText(data: WhyData): string {
  const lines = [`${data.target} (${data.reason} at depth ${data.depth})`];
  data.chain.forEach((hop, position) => {
    if (position === 0) {
      lines.push(`seed ${hop.unit}`);
      return;
    }
    const line = hop.line === null ? "" : ` line ${hop.line}`;
    lines.push(`  -> ${hop.unit} (${hop.reason}${line})`);
  });
  return lines.join("\n");
}

function stateText(data: StateEnvelope): string {
  if (data.cleared !== undefined) {
    return `cleared ${typeof data.cleared === "string" ? data.cleared : JSON.stringify(data.cleared)}`;
  }
  const lines = [
    `root: ${data.root}`,
    `state: ${data.state_path}`,
    `branch: ${data.label}`,
  ];
  if (!data.exists) lines.push("(no saved bundle)");
  for (const branch of data.branches) {
    lines.push(
      `  ${branch.label}: ${branch.entries} entries (depth ${branch.depth}, created ${branch.created})`,
    );
  }
  if (data.current !== null) {
    for (const entry of data.current.entries) {
      const symbol =
        entry.symbol === null || entry.symbol.length === 0 ? "" : `::${entry.symbol}`;
      const methods =
        entry.methods.length === 0 ? "" : `.${entry.methods.join(",")}`;
      lines.push(
        `  - ${entry.path}${symbol}${methods} (${entry.scope}, depth ${entry.depth}, ${entry.reason})`,
      );
    }
  }
  return lines.join("\n");
}

function indexText(data: IndexEnvelope): string {
  const lines = [
    `root: ${data.root}`,
    `source files: ${data.source_files}`,
    `packages: ${data.packages}, types: ${data.types}, symbols: ${data.symbols}`,
    `cache: ${data.cache.reused} reused, ${data.cache.parsed} parsed`,
  ];
  if (data.languages.length > 0) {
    lines.push(
      `languages: ${data.languages
        .map((language) => `${language.language} ${language.files}`)
        .join(", ")}`,
    );
  }
  if (data.coverage !== undefined) {
    lines.push(
      `coverage: ${data.coverage.files_indexed} indexed, ${data.coverage.files_skipped} skipped, ${data.coverage.imports_unresolved} unresolved imports`,
    );
  }
  return lines.join("\n");
}

// --- Operations -----------------------------------------------------------

/** The tool-facing subset of `bundle` flags (#41). */
export type BundleOptions = Pick<
  BundleArgs,
  "seeds" | "depth" | "reset" | "persist" | "withConfig" | "title"
>;

export async function bundle(
  context: OperationContext,
  options: BundleOptions,
): Promise<OperationResult<BundleEnvelope>> {
  if (options.seeds.length === 0) {
    throw new FilerToolError("invalid_arguments", "filer_bundle needs at least one seed", {
      hint: "pass a path or path::Symbol",
    });
  }
  // A directory seed expands to its source files at depth 0 (#68); the `--`
  // list is the only thing that changes, so the CLI resolves exactly the same
  // closure as if the files had been passed explicitly.
  const filesystem = seedFilesystem(context);
  const seeds: string[] = [];
  for (const seed of options.seeds) {
    const expanded = await expandDirectorySeed(context.client.root, seed, filesystem);
    if (expanded === null) seeds.push(seed);
    else seeds.push(...expanded.seeds);
  }
  const executed = await execute(
    context,
    bundleArgs(context.client.root, { ...options, seeds }),
  );
  if (executed.empty) return emptyBundle(context.client.root, options, executed);
  const data = asBundleEnvelope(parseObject(executed.stdout, "bundle"));
  const formatted = formatBundle(data);
  return buildResult(data, formatted.text, executed, formatted.truncation);
}

export async function show(
  context: OperationContext,
  options: ShowArgs = {},
): Promise<OperationResult<BundleEnvelope>> {
  const executed = await execute(context, showArgs(context.client.root, options));
  if (executed.empty) {
    return emptyBundle(context.client.root, { title: options.title }, executed);
  }
  const data = asBundleEnvelope(parseObject(executed.stdout, "show"));
  const formatted = formatBundle(data);
  return buildResult(data, formatted.text, executed, formatted.truncation);
}

export async function search(
  context: OperationContext,
  options: Omit<SearchArgs, "query"> & { query: string },
): Promise<OperationResult<SearchData>> {
  const executed = await execute(context, searchArgs(context.client.root, options));
  const values = parseLines(executed.stdout, "search");
  const rows: SearchRow[] = [];
  let hint = executed.hint;
  for (const value of values) {
    const record = asRecord(value, "search row");
    if (typeof record.hint === "string" && typeof record.path !== "string") {
      // The empty-result line: `{"schema_version":1,"hint":...,"results":0}`.
      assertSchemaVersion(record, "search envelope");
      hint = record.hint;
      continue;
    }
    rows.push(record as unknown as SearchRow);
  }
  assertRowSchemaVersions(rows);
  const data: SearchData = {
    schema_version: SUPPORTED_SCHEMA_VERSION,
    query: options.query,
    results: rows.length,
    rows,
    ...(hint === undefined ? {} : { hint }),
  };
  const text = rows.length === 0 ? hint ?? "no matches" : rows.map(searchRowText).join("\n");
  const capped = capText(text);
  const withHint: Executed =
    hint === undefined ? executed : { ...executed, hint };
  return buildResult(data, capped.text, withHint, capped.truncation);
}

export async function deps(
  context: OperationContext,
  options: DepsArgs,
): Promise<OperationResult<DepsEnvelope>> {
  const expanded = await expandDepsSeed(
    context.client.root,
    options.seed,
    seedFilesystem(context),
  );
  const executed = await execute(
    context,
    depsArgs(context.client.root, { ...options, seed: expanded.seed }),
  );
  if (executed.empty) {
    const data: DepsEnvelope = {
      schema_version: SUPPORTED_SCHEMA_VERSION,
      seed: expanded.seed,
      granularity: options.unit === true ? "unit" : "file",
      deps: [],
      edges: [],
      diagnostics: [],
    };
    return buildResult(data, executed.hint ?? "no dependencies", executed);
  }
  const data = asDepsEnvelope(parseObject(executed.stdout, "deps"));
  const prefix = expanded.note === null ? "" : `seed ${expanded.note}\n`;
  const rows =
    data.deps.length === 0
      ? executed.hint ?? "no dependencies"
      : data.deps.map(depsRowText).join("\n");
  const capped = capText(`${prefix}${rows}`);
  // Both lines stay outside the cap: a cut row list must still say what the
  // closure is and that it is incomplete (#68).
  const summary: string[] = [];
  const size = depsSizeLine(data);
  if (size !== null) summary.push(size);
  const closure = diagnosticsLine(diagnosticsSummary(data.diagnostics));
  if (closure !== null) summary.push(closure);
  const text = summary.length === 0 ? capped.text : `${capped.text}\n\n${summary.join("\n")}`;
  return buildResult(data, text, executed, capped.truncation);
}

/**
 * `12 units from 1 seed, depth 3`: the closure's size in one line. Bytes are
 * not reported because `deps --json` carries no per-unit sizes; only the unit
 * count and depth are facts the envelope already has.
 */
function depsSizeLine(data: DepsEnvelope): string | null {
  if (data.deps.length === 0) return null;
  const depth = data.deps.reduce((deepest, row) => Math.max(deepest, row.depth), 0);
  const units = data.deps.length === 1 ? "unit" : "units";
  return `${data.deps.length} ${units} from 1 seed, depth ${depth}`;
}

export async function callers(
  context: OperationContext,
  options: CallersArgs,
): Promise<OperationResult<CallersData>> {
  const executed = await execute(context, callersArgs(context.client.root, options));
  const values = parseArray(executed.stdout, "callers");
  const rows = values.map((value) => asRecord(value, "caller row") as unknown as CallerRow);
  const data: CallersData = {
    schema_version: SUPPORTED_SCHEMA_VERSION,
    unit: options.unit,
    rows,
  };
  const text = rows.length === 0 ? executed.hint ?? "no callers" : rows.map(callerRowText).join("\n");
  const capped = capText(text);
  return buildResult(data, capped.text, executed, capped.truncation);
}

export async function why(
  context: OperationContext,
  options: WhyArgs,
): Promise<OperationResult<WhyData>> {
  const executed = await execute(context, whyArgs(context.client.root, options));
  if (executed.empty) {
    const data: WhyData = {
      schema_version: SUPPORTED_SCHEMA_VERSION,
      target: options.unit,
      reason: "unknown",
      depth: 0,
      chain: [],
    };
    return buildResult(data, executed.hint ?? "not in the saved bundle", executed);
  }
  const object = parseObject(executed.stdout, "why");
  // `why --json` predates versioning; guard it when a version shows up.
  assertSchemaVersionIfPresent(object, "why");
  const data: WhyData = {
    schema_version: SUPPORTED_SCHEMA_VERSION,
    target: typeof object.target === "string" ? object.target : options.unit,
    reason: typeof object.reason === "string" ? object.reason : "unknown",
    depth: typeof object.depth === "number" ? object.depth : 0,
    chain: Array.isArray(object.chain) ? (object.chain as WhyHop[]) : [],
  };
  const capped = capText(whyText(data));
  return buildResult(data, capped.text, executed, capped.truncation);
}

export async function refs(
  context: OperationContext,
  options: RefsArgs,
): Promise<OperationResult<RefsData>> {
  const executed = await execute(context, refsArgs(context.client.root, options));
  const values = parseArray(executed.stdout, "refs");
  const rows = values.map((value) => asRecord(value, "ref row") as unknown as RefRow);
  const data: RefsData = {
    schema_version: SUPPORTED_SCHEMA_VERSION,
    name: options.name,
    rows,
  };
  const text = rows.length === 0 ? executed.hint ?? "no references" : rows.map(refRowText).join("\n");
  const capped = capText(text);
  return buildResult(data, capped.text, executed, capped.truncation);
}

export async function state(
  context: OperationContext,
  options: StateArgs = {},
): Promise<OperationResult<StateEnvelope>> {
  const executed = await execute(context, stateArgs(context.client.root, options));
  const data = asStateEnvelope(parseObject(executed.stdout, "state"));
  const capped = capText(stateText(data));
  return buildResult(data, capped.text, executed, capped.truncation);
}

export async function index(
  context: OperationContext,
): Promise<OperationResult<IndexEnvelope>> {
  const executed = await execute(context, indexArgs(context.client.root));
  const data = asIndexEnvelope(parseObject(executed.stdout, "index"));
  const capped = capText(indexText(data));
  return buildResult(data, capped.text, executed, capped.truncation);
}
