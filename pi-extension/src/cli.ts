//! Spawning the `filer` binary and discovering where it lives.
//!
//! Nothing in this module runs at import time or at factory time: discovery is
//! lazy (`RootResolver.resolve`) and every process is a short-lived
//! `execFile(bin, argv, ...)`. There is no shell, no watcher and no timer that
//! outlives a call.

import { execFile, type ChildProcess } from "node:child_process";

export const DEFAULT_TIMEOUT_MS = 60_000;
/** The CLI renders whole bundles; 32 MiB is far above any useful one. */
export const MAX_BUFFER_BYTES = 32 * 1024 * 1024;
/** The `PATH` fallback when nothing overrides the binary. */
export const FILER_BINARY_NAME = "filer";
/** `git rev-parse` is a lookup, not a long operation. */
export const GIT_LOOKUP_TIMEOUT_MS = 10_000;

export type Env = Record<string, string | undefined>;

export interface RunResult {
  stdout: string;
  stderr: string;
  /** Exit status, or `null` when the child died on a signal. */
  code: number | null;
  signal: string | null;
  timedOut: boolean;
  aborted: boolean;
  /** Set when the process could not be started or its output overflowed. */
  spawnError?: { code: string; message: string };
  /** The binary that was spawned, for actionable error details. */
  bin?: string;
  timeoutMs?: number;
}

export interface RunOptions {
  cwd?: string;
  env?: Env;
  timeoutMs?: number;
  signal?: AbortSignal;
  maxBuffer?: number;
}

/** The single seam tests replace: everything else is pure. */
export interface ProcessRunner {
  run(bin: string, argv: string[], options?: RunOptions): Promise<RunResult>;
}

function mergedEnv(env?: Env): NodeJS.ProcessEnv {
  const merged: Record<string, string> = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (value !== undefined) merged[key] = value;
  }
  for (const [key, value] of Object.entries(env ?? {})) {
    if (value !== undefined) merged[key] = value;
  }
  return merged;
}

/** The default runner: one short-lived `execFile` per call. */
export const defaultRunner: ProcessRunner = {
  run(bin, argv, options = {}) {
    return new Promise<RunResult>((resolve) => {
      const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
      let aborted = false;
      let child: ChildProcess;
      const onAbort = () => {
        aborted = true;
        child.kill("SIGKILL");
      };
      child = execFile(
        bin,
        argv,
        {
          cwd: options.cwd,
          env: mergedEnv(options.env),
          timeout: timeoutMs,
          killSignal: "SIGKILL",
          maxBuffer: options.maxBuffer ?? MAX_BUFFER_BYTES,
          windowsHide: true,
        },
        (error, stdout, stderr) => {
          options.signal?.removeEventListener("abort", onAbort);
          const errno = error as NodeJS.ErrnoException | null;
          // execFile's callback omits the exit status on success, so a null
          // error is exit 0. A failure carries the numeric code on `error`.
          const numericCode = errno === null ? 0 : typeof errno.code === "number" ? errno.code : null;
          const killed = Boolean(errno && (errno as { killed?: boolean }).killed);
          const spawnError =
            errno && typeof errno.code === "string"
              ? {
                  code: errno.code.includes("MAXBUFFER") ? "ENOBUFS" : errno.code,
                  message: errno.message,
                }
              : undefined;
          resolve({
            stdout: String(stdout ?? ""),
            stderr: String(stderr ?? ""),
            code: numericCode,
            signal: (errno as { signal?: string } | null)?.signal ?? null,
            timedOut: !aborted && spawnError === undefined && killed && errno !== null && numericCode === null,
            aborted,
            ...(spawnError === undefined ? {} : { spawnError }),
            bin,
            timeoutMs,
          });
        },
      );
      if (options.signal !== undefined) {
        if (options.signal.aborted) onAbort();
        else options.signal.addEventListener("abort", onAbort, { once: true });
      }
    });
  },
};

function nonEmpty(value: string | undefined | null): string | undefined {
  if (value === undefined || value === null) return undefined;
  const trimmed = value.trim();
  return trimmed.length === 0 ? undefined : trimmed;
}

export interface BinaryInput {
  /** `--filer-bin`. */
  flag?: string;
  env?: Env;
  /** `/filer bin`, persisted for the session by the commands follow-up. */
  session?: string;
}

/**
 * Binary discovery order: `--filer-bin` > `FILER_BIN` > session override >
 * `filer` on `PATH`.
 */
export function binaryFrom(input: BinaryInput = {}): string {
  const env = input.env ?? {};
  return (
    nonEmpty(input.flag) ??
    nonEmpty(env.FILER_BIN) ??
    nonEmpty(input.session) ??
    FILER_BINARY_NAME
  );
}

/** `FILER_TIMEOUT_MS` when it parses as a positive number, else 60 s. */
export function timeoutFrom(env: Env = {}): number {
  const raw = nonEmpty(env.FILER_TIMEOUT_MS);
  if (raw === undefined) return DEFAULT_TIMEOUT_MS;
  const parsed = Number(raw);
  if (!Number.isFinite(parsed) || parsed <= 0) return DEFAULT_TIMEOUT_MS;
  return Math.floor(parsed);
}

/** `--filer-root` > `FILER_ROOT`; `undefined` means "ask git, then cwd". */
export function rootFrom(input: { flag?: string; env?: Env } = {}): string | undefined {
  const env = input.env ?? {};
  return nonEmpty(input.flag) ?? nonEmpty(env.FILER_ROOT);
}

/** One `git rev-parse --show-toplevel`, or `null` outside a repository. */
export async function detectGitRoot(
  cwd: string,
  options: { env?: Env; runner?: ProcessRunner; signal?: AbortSignal } = {},
): Promise<string | null> {
  const runner = options.runner ?? defaultRunner;
  const result = await runner.run("git", ["rev-parse", "--show-toplevel"], {
    cwd,
    ...(options.env === undefined ? {} : { env: options.env }),
    ...(options.signal === undefined ? {} : { signal: options.signal }),
    timeoutMs: GIT_LOOKUP_TIMEOUT_MS,
  });
  if (result.code !== 0) return null;
  return nonEmpty(result.stdout) ?? null;
}

export interface RootResolutionInput {
  /** `--filer-root`. */
  flagRoot?: string;
  cwd: string;
  signal?: AbortSignal;
}

/** Resolution order: `--filer-root` > `FILER_ROOT` > git toplevel > cwd. */
export interface RootResolver {
  resolve(input: RootResolutionInput): Promise<string>;
}

export interface RootResolverOptions {
  env?: Env;
  runner?: ProcessRunner;
  /** Injectable git lookup; the default is {@link detectGitRoot}. */
  detectGitRoot?: (cwd: string, signal?: AbortSignal) => Promise<string | null>;
}

/**
 * A per-session root resolver. The git lookup runs at most once per cwd, so a
 * session in one directory spawns `git` once and every tool call reuses it.
 */
export function createRootResolver(options: RootResolverOptions = {}): RootResolver {
  const env = options.env ?? {};
  const runner = options.runner ?? defaultRunner;
  const cache = new Map<string, Promise<string | null>>();
  const detect =
    options.detectGitRoot ??
    ((cwd: string, signal?: AbortSignal) =>
      detectGitRoot(cwd, {
        ...(options.env === undefined ? {} : { env: options.env }),
        runner,
        ...(signal === undefined ? {} : { signal }),
      }));

  return {
    async resolve(input: RootResolutionInput): Promise<string> {
      const explicit = rootFrom({ flag: input.flagRoot, env });
      if (explicit !== undefined) return explicit;
      let pending = cache.get(input.cwd);
      if (pending === undefined) {
        pending = detect(input.cwd, input.signal).catch(() => null);
        cache.set(input.cwd, pending);
      }
      return (await pending) ?? input.cwd;
    },
  };
}

/** Everything a call needs: which binary, which root, which cwd and timeout. */
export interface FilerRuntime {
  bin: string;
  root: string;
  cwd: string;
  timeoutMs: number;
}

/** The narrow client the operations use, so tests can fake the process layer. */
export interface FilerClient {
  root: string;
  run(argv: string[], options?: { signal?: AbortSignal }): Promise<RunResult>;
}

/** Bind a runtime to the runner: no discovery, no shell, one argv array. */
export function createFilerClient(
  runtime: FilerRuntime,
  runner: ProcessRunner = defaultRunner,
): FilerClient {
  return {
    root: runtime.root,
    run(argv, options = {}) {
      return runner.run(runtime.bin, argv, {
        cwd: runtime.cwd,
        timeoutMs: runtime.timeoutMs,
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      });
    },
  };
}
