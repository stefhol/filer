import { describe, expect, test } from "bun:test";
import type { RunResult } from "../src/cli";
import { FilerToolError, classifyRun, parseStderr } from "../src/errors";

function run(partial: Partial<RunResult> = {}): RunResult {
  return {
    stdout: "",
    stderr: "",
    code: 0,
    signal: null,
    timedOut: false,
    aborted: false,
    bin: "filer",
    ...partial,
  };
}

const envelope = (code: string, message: string, hint?: string): string =>
  JSON.stringify({ error: { code, message, ...(hint === undefined ? {} : { hint }) } });

describe("parseStderr", () => {
  test("collects one error envelope and every warning line", () => {
    const report = parseStderr(
      [
        "filer: prose the model must never see",
        JSON.stringify({ warning: "dropping src/legacy/Old.java" }),
        envelope("file_not_found", "app/Missing.java does not exist", "check the path"),
        JSON.stringify({ warning: "3 unresolved imports" }),
        "",
      ].join("\n"),
    );
    expect(report.error).toEqual({
      code: "file_not_found",
      message: "app/Missing.java does not exist",
      hint: "check the path",
    });
    expect(report.warnings).toEqual([
      "dropping src/legacy/Old.java",
      "3 unresolved imports",
    ]);
  });

  test("ignores non-JSON and non-envelope JSON lines", () => {
    expect(parseStderr('filer: boom\n{"other":1}\n[1,2]\n')).toEqual({ warnings: [] });
  });

  test("keeps going after a malformed line", () => {
    const report = parseStderr('{"warning":"first"}\n{not json}\n{"warning":"second"}\n');
    expect(report.warnings).toEqual(["first", "second"]);
  });
});

describe("classifyRun", () => {
  test("exit 0 is success", () => {
    const outcome = classifyRun(run());
    expect(outcome.ok).toBe(true);
    expect(outcome.ok && outcome.empty).toBe(false);
  });

  test("exit 4 is a successful empty result carrying the hint", () => {
    const outcome = classifyRun(
      run({
        code: 4,
        stderr: envelope("empty_result", "nothing to bundle", "pass a different seed"),
      }),
    );
    expect(outcome).toEqual({
      ok: true,
      empty: true,
      hint: "pass a different seed",
      warnings: [],
    });
  });

  test("exit 4 without an envelope is still success, without a hint", () => {
    const outcome = classifyRun(run({ code: 4 }));
    expect(outcome).toEqual({ ok: true, empty: true, warnings: [] });
  });

  test("exit 2 keeps code and hint and hides the CLI message in details", () => {
    const outcome = classifyRun(
      run({
        code: 2,
        stderr: envelope(
          "symbol_not_found",
          "app/Service.java has no symbol Nope",
          "pass a path alone to bundle the whole file",
        ),
      }),
    );
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error).toBeInstanceOf(FilerToolError);
    expect(outcome.error.code).toBe("symbol_not_found");
    expect(outcome.error.hint).toBe("pass a path alone to bundle the whole file");
    expect(outcome.error.message).toBe("pass a path alone to bundle the whole file");
    expect(outcome.error.details).toEqual({
      message: "app/Service.java has no symbol Nope",
    });
    expect(outcome.error.toToolMessage()).toBe(
      "symbol_not_found: pass a path alone to bundle the whole file",
    );
  });

  test("exit 3 keeps the not_indexed code", () => {
    const outcome = classifyRun(
      run({ code: 3, stderr: envelope("not_indexed", "README.md is not indexed", "pass a source file") }),
    );
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error.code).toBe("not_indexed");
    expect(outcome.error.hint).toBe("pass a source file");
  });

  test("exit 1 with an envelope stays structured", () => {
    const outcome = classifyRun(
      run({ code: 1, stderr: envelope("internal", "cannot write state", "check permissions") }),
    );
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error.code).toBe("internal");
    expect(outcome.error.details).toEqual({ message: "cannot write state" });
  });

  test("unparsable stderr never reaches the model", () => {
    const outcome = classifyRun(
      run({ code: 1, stderr: "filer: disk on fire\nstack trace at 0x42" }),
    );
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error.message).toBe("filer exited 1 without a JSON envelope");
    expect(outcome.error.message).not.toContain("disk on fire");
    expect(outcome.error.message).not.toContain("0x42");
  });

  test("a missing binary becomes an install hint", () => {
    const outcome = classifyRun(
      run({ spawnError: { code: "ENOENT", message: "spawn filer ENOENT" } }),
    );
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error.code).toBe("filer_not_found");
    expect(outcome.error.message).toBe(
      "filer not found — install with cargo install --path . or set FILER_BIN",
    );
    expect(outcome.error.details).toEqual({ bin: "filer" });
  });

  test("a timeout is actionable and names the budget", () => {
    const outcome = classifyRun(run({ timeoutMs: 250, timedOut: true }));
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error.code).toBe("filer_timeout");
    expect(outcome.error.message).toBe("filer timed out after 250 ms");
    expect(outcome.error.hint).toContain("FILER_TIMEOUT_MS");
  });

  test("an abort is its own code", () => {
    const outcome = classifyRun(run({ aborted: true }));
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error.code).toBe("aborted");
  });

  test("a 32 MiB overflow is reported, not parsed", () => {
    const outcome = classifyRun(
      run({ spawnError: { code: "ENOBUFS", message: "maxBuffer exceeded" } }),
    );
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error.code).toBe("output_too_large");
  });

  test("warnings survive a failure", () => {
    const outcome = classifyRun(
      run({
        code: 2,
        stderr: `${JSON.stringify({ warning: "dropping a file" })}\n${envelope(
          "branch_not_found",
          "no bundle for branch nope",
          "list the ones that do",
        )}`,
      }),
    );
    expect(outcome.warnings).toEqual(["dropping a file"]);
    expect(outcome.ok).toBe(false);
  });
});
