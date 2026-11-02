//! pi extension entry point.
//!
//! This installment only wires discovery: the two CLI flags and the adapter
//! that every future tool and command calls. It deliberately registers no
//! tools and no commands (#41 / #42) and starts no process — some pi
//! invocations load extensions without a session, so the factory must stay
//! side-effect free. The first `resolve()` happens on demand.
//!
//! Everything below `index.ts` is pi-free at runtime (type-only imports), so
//! `bun test` does not need the pi runtime.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
  binaryFrom,
  createFilerClient,
  createRootResolver,
  timeoutFrom,
  type Env,
  type FilerClient,
  type FilerRuntime,
  type ProcessRunner,
  type RootResolver,
} from "./src/cli";
import { registerFilerTools } from "./src/tools";

/** `--filer-bin <path>`: highest-precedence binary override. */
export const FILER_BIN_FLAG = "filer-bin";
/** `--filer-root <dir>`: highest-precedence root override. */
export const FILER_ROOT_FLAG = "filer-root";

/** The extension version; the wire format it speaks is `src/schema.ts`. */
export const EXTENSION_VERSION = "0.1.0";

export interface FilerAdapterDeps {
  env?: Env;
  /** Injected process layer (tests); defaults to `execFile`. */
  runner?: ProcessRunner;
  /** Injected git lookup (tests); defaults to `git rev-parse --show-toplevel`. */
  detectGitRoot?: (cwd: string, signal?: AbortSignal) => Promise<string | null>;
}

export interface FilerResolutionInput {
  /** `ctx.cwd`, the session's working directory. */
  cwd: string;
  /** `pi.getFlag("filer-bin")`. */
  flagBin?: string;
  /** `pi.getFlag("filer-root")`. */
  flagRoot?: string;
  /** `/filer bin`, persisted for the session by the commands follow-up. */
  sessionBin?: string;
  signal?: AbortSignal;
}

export type ResolvedFiler = FilerRuntime;

export interface FilerAdapter {
  /** Discovery order: flag > env > session > `filer`; root: flag > env > git > cwd. */
  resolve(input: FilerResolutionInput): Promise<ResolvedFiler>;
  /** Bind a resolved runtime to the process layer. */
  client(resolved: ResolvedFiler): FilerClient;
  /** Exposed for the commands follow-up (`/filer root`). */
  roots: RootResolver;
}

/**
 * Build the adapter. Pure: no process runs until `resolve()` is called, and
 * the git toplevel lookup is cached per working directory for the session.
 */
export function createFilerAdapter(deps: FilerAdapterDeps = {}): FilerAdapter {
  const env = deps.env ?? {};
  const roots = createRootResolver({
    env,
    ...(deps.runner === undefined ? {} : { runner: deps.runner }),
    ...(deps.detectGitRoot === undefined ? {} : { detectGitRoot: deps.detectGitRoot }),
  });

  return {
    roots,
    async resolve(input: FilerResolutionInput): Promise<ResolvedFiler> {
      const root = await roots.resolve({
        ...(input.flagRoot === undefined ? {} : { flagRoot: input.flagRoot }),
        cwd: input.cwd,
        ...(input.signal === undefined ? {} : { signal: input.signal }),
      });
      return {
        bin: binaryFrom({
          ...(input.flagBin === undefined ? {} : { flag: input.flagBin }),
          env,
          ...(input.sessionBin === undefined ? {} : { session: input.sessionBin }),
        }),
        root,
        cwd: input.cwd,
        timeoutMs: timeoutFrom(env),
      };
    },
    client(resolved: ResolvedFiler): FilerClient {
      return createFilerClient(resolved, deps.runner);
    },
  };
}

/** Register the two CLI flags; pi reads them through `pi.getFlag(name)`. */
export function registerFilerFlags(pi: ExtensionAPI): void {
  pi.registerFlag(FILER_BIN_FLAG, {
    description: "Path to the filer binary (overrides FILER_BIN)",
    type: "string",
  });
  pi.registerFlag(FILER_ROOT_FLAG, {
    description: "Project root for filer (overrides FILER_ROOT and git)",
    type: "string",
  });
}

/**
 * pi's default extension factory. The second parameter is a test seam: pi
 * passes only the API, and every call site in the extension goes through the
 * adapter it returns.
 */
export default function filerExtension(
  pi: ExtensionAPI,
  deps: FilerAdapterDeps = {},
): void {
  registerFilerFlags(pi);
  // Created eagerly but process-free; the tools resolve the binary and root
  // lazily on their first call (and once at session_start for --version).
  const adapter = createFilerAdapter(deps);
  registerFilerTools(pi, {
    adapter,
    binFlag: FILER_BIN_FLAG,
    rootFlag: FILER_ROOT_FLAG,
  });
}
