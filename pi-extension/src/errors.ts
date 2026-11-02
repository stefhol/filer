//! Exit codes, error envelopes and warnings.
//!
//! The CLI is explicit about failure: `--json` writes
//! `{"error":{"code","message","hint"}}` to stderr and exits `1` (internal),
//! `2` (not found) or `3` (not indexed); exit `4` means the command succeeded
//! with an empty result. Soft notices are `{"warning":"..."}` lines.
//!
//! Raw stderr never reaches the model. Either it parses as the envelope above,
//! or the caller gets `filer exited N without a JSON envelope`.

import type { RunResult } from "./cli";

export const ERROR_HINT_SEARCH =
  "symbol/path index only; use text search for file contents";

const NOT_FOUND_MESSAGE =
  "filer not found — install with cargo install --path . or set FILER_BIN";

export interface ErrorEnvelope {
  code: string;
  message?: string;
  hint?: string;
}

export interface StderrReport {
  error?: ErrorEnvelope;
  warnings: string[];
}

export interface FilerToolErrorOptions {
  hint?: string;
  details?: Record<string, unknown>;
  cause?: unknown;
}

/**
 * An error the tool layer can hand to the model: a stable `code`, a short
 * message, and an optional hint. `details` carries machine-readable context
 * (including the CLI's prose message) without forwarding it as the message.
 */
export class FilerToolError extends Error {
  readonly code: string;
  readonly hint?: string;
  readonly details?: Record<string, unknown>;

  constructor(code: string, message: string, options: FilerToolErrorOptions = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "FilerToolError";
    this.code = code;
    this.hint = options.hint;
    this.details = options.details;
  }

  /** `code: hint` (or `code: message` without a hint), the model-facing line. */
  toToolMessage(): string {
    return `${this.code}: ${this.hint ?? this.message}`;
  }

  toJSON(): Record<string, unknown> {
    return {
      code: this.code,
      message: this.message,
      ...(this.hint === undefined ? {} : { hint: this.hint }),
      ...(this.details === undefined ? {} : { details: this.details }),
    };
  }
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return undefined;
  }
  return value as Record<string, unknown>;
}

/** Parse the JSON lines of stderr: one error envelope, every warning line. */
export function parseStderr(stderr: string): StderrReport {
  const report: StderrReport = { warnings: [] };
  for (const line of stderr.split("\n")) {
    const text = line.trim();
    if (text.length === 0) continue;
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      // Prose on stderr is a binary that ignored --json (or a crash); drop it.
      continue;
    }
    const record = asRecord(parsed);
    if (record === undefined) continue;
    const error = asRecord(record.error);
    if (error !== undefined && typeof error.code === "string") {
      report.error = {
        code: error.code,
        ...(typeof error.message === "string" ? { message: error.message } : {}),
        ...(typeof error.hint === "string" ? { hint: error.hint } : {}),
      };
      continue;
    }
    if (typeof record.warning === "string") {
      report.warnings.push(record.warning);
    }
  }
  return report;
}

export type RunOutcome =
  | { ok: true; empty: false; warnings: string[]; hint?: string }
  | { ok: true; empty: true; warnings: string[]; hint?: string }
  | { ok: false; error: FilerToolError; warnings: string[] };

/**
 * Map a completed run onto "success", "success but empty" (exit `4`) or a
 * structured error. Spawn failures (missing binary, timeout, abort, overflow)
 * become actionable text of their own.
 */
export function classifyRun(result: RunResult): RunOutcome {
  const report = parseStderr(result.stderr);
  const warnings = report.warnings;

  const spawnError = result.spawnError;
  if (spawnError !== undefined) {
    if (spawnError.code === "ENOENT") {
      return {
        ok: false,
        warnings,
        error: new FilerToolError("filer_not_found", NOT_FOUND_MESSAGE, {
          hint: "set FILER_BIN or pass --filer-bin <path>",
          details: { bin: result.bin ?? "filer" },
        }),
      };
    }
    if (spawnError.code === "ENOBUFS") {
      return {
        ok: false,
        warnings,
        error: new FilerToolError("output_too_large", "filer output exceeded 32 MiB", {
          hint: "narrow the bundle with fewer seeds, a lower depth, or --max-bytes",
        }),
      };
    }
    return {
      ok: false,
      warnings,
      error: new FilerToolError(`spawn_${spawnError.code.toLowerCase()}`, spawnError.message, {
        hint: "check that FILER_BIN points at an executable",
      }),
    };
  }

  if (result.timedOut) {
    const timeout = result.timeoutMs === undefined ? "" : ` after ${result.timeoutMs} ms`;
    return {
      ok: false,
      warnings,
      error: new FilerToolError("filer_timeout", `filer timed out${timeout}`, {
        hint: "raise FILER_TIMEOUT_MS or narrow the bundle",
        details: result.timeoutMs === undefined ? undefined : { timeout_ms: result.timeoutMs },
      }),
    };
  }

  if (result.aborted) {
    return {
      ok: false,
      warnings,
      error: new FilerToolError("aborted", "filer call was aborted", {
        hint: "retry the call",
      }),
    };
  }

  const code = result.code;
  if (code === 0) {
    return { ok: true, empty: false, warnings };
  }

  const envelope = report.error;
  if (code === 4) {
    // Success with an empty result. The hint is the CLI's own.
    return {
      ok: true,
      empty: true,
      warnings,
      ...(envelope?.hint !== undefined
        ? { hint: envelope.hint }
        : envelope?.message !== undefined
          ? { hint: envelope.message }
          : {}),
    };
  }

  if (envelope !== undefined) {
    const details =
      envelope.message === undefined ? undefined : { message: envelope.message };
    return {
      ok: false,
      warnings,
      error: new FilerToolError(
        envelope.code,
        envelope.hint ?? envelope.message ?? `filer failed with ${envelope.code}`,
        {
          ...(envelope.hint === undefined ? {} : { hint: envelope.hint }),
          ...(details === undefined ? {} : { details }),
        },
      ),
    };
  }

  return {
    ok: false,
    warnings,
    error: new FilerToolError(
      "unparsable_stderr",
      `filer exited ${code ?? "on a signal"} without a JSON envelope`,
    ),
  };
}
