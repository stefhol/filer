//! Pure argv builders, one per CLI command.
//!
//! Every builder returns a fresh `string[]` and never a shell string, so the
//! adapter cannot quote, split or interpolate user input. The order is fixed:
//!
//! ```text
//! --root <root> --json --quiet  <subcommand> [flags...]  --  [positionals...]
//! ```
//!
//! `--json --quiet` is mandatory. The adapter parses the JSON surface, while
//! the human summary and the prose warnings would only duplicate it.

import { join } from "node:path";

/** Where the CLI keeps the default branch state, relative to the root. */
export const STATE_RELATIVE_PATH = join(".filer", "bundle.toml");

/**
 * Absolute path of the default state file for `root`. The CLI resolves a
 * relative `--state` against the process cwd, which is not stable across a
 * session, so state-carrying flags always get an absolute path.
 */
export function statePath(root: string): string {
  return join(root, STATE_RELATIVE_PATH);
}

/** The global flags every invocation starts with. */
export function globalArgs(root: string): string[] {
  return ["--root", root, "--json", "--quiet"];
}

export interface BundleArgs {
  /** `path`, `path::Symbol` or `path::Class.method`. */
  seeds: string[];
  depth?: number;
  reset?: boolean;
  /** Carry the previous bundle over: adds `--state <root>/.filer/bundle.toml`. */
  persist?: boolean;
  withConfig?: boolean;
  withSamePackage?: boolean;
  title?: string;
  maxFileBytes?: number;
  maxBytes?: number;
  maxTokens?: number;
  atLine?: number;
  skip?: string[];
  fromState?: boolean;
}

/** `filer bundle`: flags first, then `--`, then the seed list. */
export function bundleArgs(root: string, options: BundleArgs): string[] {
  if (options.maxBytes !== undefined && options.maxTokens !== undefined) {
    throw new Error("maxBytes and maxTokens are mutually exclusive");
  }
  const argv = [...globalArgs(root), "bundle"];
  if (options.depth !== undefined) argv.push("--depth", String(options.depth));
  if (options.maxFileBytes !== undefined) {
    argv.push("--max-file-bytes", String(options.maxFileBytes));
  }
  if (options.maxBytes !== undefined) {
    argv.push("--max-bytes", String(options.maxBytes));
  }
  if (options.maxTokens !== undefined) {
    argv.push("--max-tokens", String(options.maxTokens));
  }
  if (options.withSamePackage) argv.push("--with-same-package");
  if (options.withConfig) argv.push("--with-config");
  if (options.title !== undefined) argv.push("--title", options.title);
  if (options.persist) argv.push("--state", statePath(root));
  if (options.reset) argv.push("--reset");
  if (options.fromState) argv.push("--from-state");
  if (options.atLine !== undefined) argv.push("--at-line", String(options.atLine));
  for (const unit of options.skip ?? []) argv.push("--skip", unit);
  if (options.seeds.length > 0) argv.push("--", ...options.seeds);
  return argv;
}

export interface ShowArgs {
  branch?: string;
  /** State file override; defaults to `<root>/.filer/bundle.toml` in the CLI. */
  state?: string;
  title?: string;
  maxBytes?: number;
  maxTokens?: number;
  maxFileBytes?: number;
}

/** `filer show`: re-render a saved branch. It takes no positionals. */
export function showArgs(root: string, options: ShowArgs = {}): string[] {
  if (options.maxBytes !== undefined && options.maxTokens !== undefined) {
    throw new Error("maxBytes and maxTokens are mutually exclusive");
  }
  const argv = [...globalArgs(root), "show"];
  if (options.state !== undefined) argv.push("--state", options.state);
  if (options.branch !== undefined) argv.push("--branch", options.branch);
  if (options.title !== undefined) argv.push("--title", options.title);
  if (options.maxBytes !== undefined) {
    argv.push("--max-bytes", String(options.maxBytes));
  }
  if (options.maxTokens !== undefined) {
    argv.push("--max-tokens", String(options.maxTokens));
  }
  if (options.maxFileBytes !== undefined) {
    argv.push("--max-file-bytes", String(options.maxFileBytes));
  }
  return argv;
}

export interface StateArgs {
  branch?: string;
  clear?: boolean;
  all?: boolean;
  /** The CLI spells this flag `--file`, unlike `--state` for bundle/show/why. */
  file?: string;
}

/** `filer state`: read or clear the per-branch bundle state. */
export function stateArgs(root: string, options: StateArgs = {}): string[] {
  const argv = [...globalArgs(root), "state"];
  if (options.file !== undefined) argv.push("--file", options.file);
  if (options.branch !== undefined) argv.push("--branch", options.branch);
  if (options.clear) argv.push("--clear");
  if (options.all) argv.push("--all");
  return argv;
}

/** `filer index`: coverage for the whole index. Takes no positionals. */
export function indexArgs(root: string): string[] {
  return [...globalArgs(root), "index"];
}

export interface SearchArgs {
  query: string;
  limit?: number;
  /** Mark files already in the bundle stored at this file. */
  state?: string;
  branch?: string;
}

/** `filer search`: JSON lines, one row per unit. */
export function searchArgs(root: string, options: SearchArgs): string[] {
  const argv = [...globalArgs(root), "search", "--query", options.query];
  if (options.limit !== undefined) argv.push("--limit", String(options.limit));
  if (options.state !== undefined) argv.push("--state", options.state);
  if (options.branch !== undefined) argv.push("--branch", options.branch);
  return argv;
}

export interface DepsArgs {
  seed: string;
  unit?: boolean;
  depth?: number;
}

/** `filer deps`: import/call closure of one seed. */
export function depsArgs(root: string, options: DepsArgs): string[] {
  const argv = [...globalArgs(root), "deps"];
  if (options.depth !== undefined) argv.push("--depth", String(options.depth));
  if (options.unit) argv.push("--unit");
  argv.push("--", options.seed);
  return argv;
}

export interface CallersArgs {
  unit: string;
  depth?: number;
}

/** `filer callers`: reverse call edges, nearest first. */
export function callersArgs(root: string, options: CallersArgs): string[] {
  const argv = [...globalArgs(root), "callers"];
  if (options.depth !== undefined) argv.push("--depth", String(options.depth));
  argv.push("--", options.unit);
  return argv;
}

export interface WhyArgs {
  unit: string;
  state?: string;
  branch?: string;
}

/** `filer why`: how a unit entered a saved bundle. */
export function whyArgs(root: string, options: WhyArgs): string[] {
  const argv = [...globalArgs(root), "why"];
  if (options.state !== undefined) argv.push("--state", options.state);
  if (options.branch !== undefined) argv.push("--branch", options.branch);
  argv.push("--", options.unit);
  return argv;
}

export interface RefsArgs {
  name: string;
}

/** `filer refs`: invert recorded uses and calls of one name. */
export function refsArgs(root: string, options: RefsArgs): string[] {
  return [...globalArgs(root), "refs", "--", options.name];
}
