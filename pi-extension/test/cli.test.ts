import { afterAll, describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  binaryFrom,
  createFilerClient,
  createRootResolver,
  defaultRunner,
  detectGitRoot,
  timeoutFrom,
  type ProcessRunner,
} from "../src/cli";
import { bundle, search } from "../src/operations";
import { FilerToolError, ERROR_HINT_SEARCH } from "../src/errors";

const workdir = mkdtempSync(join(tmpdir(), "filer-pi-stub-"));
afterAll(() => rmSync(workdir, { recursive: true, force: true }));

function stub(name: string, body: string): string {
  const path = join(workdir, name);
  writeFileSync(path, `#!/bin/sh\n${body}\n`);
  chmodSync(path, 0o755);
  return path;
}

function clientFor(bin: string, timeoutMs = 60_000) {
  return createFilerClient({ bin, root: workdir, cwd: workdir, timeoutMs });
}

const bundleJson = JSON.stringify({
  schema_version: 1,
  root: workdir,
  title: "stub",
  depth: 2,
  files: 0,
  excerpts: 0,
  methods: 0,
  budget: {
    max_file_bytes: 1048576,
    max_bytes: null,
    max_tokens: null,
    used_bytes: 0,
    used_tokens: 0,
    truncated: false,
  },
  entries: [],
  omitted: [],
  diagnostics: [],
  coverage: { files_indexed: 0, files_skipped: 0, imports_unresolved: 0 },
  state_toml: null,
});

async function capture(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
    throw new Error("expected the operation to reject");
  } catch (error) {
    return error;
  }
}

describe("stub binary: exit-code mapping", () => {
  test("exit 4 becomes an empty success carrying the hint", async () => {
    const bin = stub(
      "exit4.sh",
      `cat <<'JSON'\n${JSON.stringify({
        schema_version: 1,
        hint: ERROR_HINT_SEARCH,
        results: 0,
      })}\nJSON\nexit 4`,
    );
    const result = await search({ client: clientFor(bin) }, { query: "nothing" });
    expect(result.data.results).toBe(0);
    expect(result.hint).toBe(ERROR_HINT_SEARCH);
    expect(result.text).toBe(ERROR_HINT_SEARCH);
  });

  test("exit 2 becomes a structured code + hint, never raw stderr", async () => {
    const json = JSON.stringify({
      error: {
        code: "symbol_not_found",
        message: "src/App.java has no symbol Nope",
        hint: "pass a path alone to bundle the whole file",
      },
    });
    const bin = stub(
      "exit2.sh",
      `echo 'filer: this prose must not reach the model' >&2\ncat <<'JSON' >&2\n${json}\nJSON\nexit 2`,
    );
    const error = await capture(
      bundle({ client: clientFor(bin) }, { seeds: ["src/App.java::Nope"] }),
    );
    expect(error).toBeInstanceOf(FilerToolError);
    const toolError = error as FilerToolError;
    expect(toolError.code).toBe("symbol_not_found");
    expect(toolError.message).toBe("pass a path alone to bundle the whole file");
    expect(toolError.message).not.toContain("must not reach the model");
    expect(toolError.details).toEqual({
      message: "src/App.java has no symbol Nope",
    });
  });

  test("a warning line is metadata, not an error", async () => {
    const bin = stub(
      "warning.sh",
      `cat <<'JSON' >&2\n${JSON.stringify({ warning: "dropping stale unit" })}\nJSON\ncat <<'JSON'\n${bundleJson}\nJSON`,
    );
    const result = await bundle({ client: clientFor(bin) }, { seeds: ["src/App.java"] });
    expect(result.warnings).toEqual(["dropping stale unit"]);
    expect(result.data.schema_version).toBe(1);
  });

  test("exit 1 with prose stderr reports only the exit code", async () => {
    const bin = stub("exit1.sh", `echo 'filer: internal prose' >&2\nexit 1`);
    const error = await capture(bundle({ client: clientFor(bin) }, { seeds: ["A.java"] }));
    expect((error as FilerToolError).message).toBe(
      "filer exited 1 without a JSON envelope",
    );
    expect((error as FilerToolError).message).not.toContain("internal prose");
  });

  test("a missing binary is an install hint", async () => {
    const error = await capture(
      bundle(
        { client: clientFor(join(workdir, "no-such-filer")) },
        { seeds: ["A.java"] },
      ),
    );
    expect((error as FilerToolError).code).toBe("filer_not_found");
    expect((error as FilerToolError).message).toContain(
      "install with cargo install --path . or set FILER_BIN",
    );
  });
});

describe("stub binary: timeout and cleanup", () => {
  test("a hung filer times out and leaves no child process", async () => {
    const pidFile = join(workdir, "hung.pid");
    const bin = stub("hang.sh", `echo $$ > '${pidFile}'\nexec sleep 30`);

    const error = await capture(
      bundle({ client: clientFor(bin, 300) }, { seeds: ["A.java"] }),
    );
    expect(error).toBeInstanceOf(FilerToolError);
    expect((error as FilerToolError).code).toBe("filer_timeout");
    expect((error as FilerToolError).message).toContain("timed out");

    const pid = Number.parseInt(readFileSync(pidFile, "utf8").trim(), 10);
    expect(Number.isInteger(pid)).toBe(true);
    let alive = true;
    for (let attempt = 0; attempt < 40 && alive; attempt += 1) {
      try {
        process.kill(pid, 0);
        await new Promise((resolve) => setTimeout(resolve, 25));
      } catch {
        alive = false;
      }
    }
    expect(alive).toBe(false);
  }, 10_000);

  test("an AbortSignal kills the child", async () => {
    const pidFile = join(workdir, "aborted.pid");
    const bin = stub("abort.sh", `echo $$ > '${pidFile}'\nexec sleep 30`);
    const controller = new AbortController();
    const pending = capture(
      bundle(
        { client: clientFor(bin), signal: controller.signal },
        { seeds: ["A.java"] },
      ),
    );
    await new Promise((resolve) => setTimeout(resolve, 200));
    controller.abort();
    const error = await pending;
    expect((error as FilerToolError).code).toBe("aborted");
  }, 10_000);
});

describe("discovery", () => {
  test("binary precedence: flag > FILER_BIN > session > PATH", () => {
    expect(
      binaryFrom({
        flag: "/from/flag",
        env: { FILER_BIN: "/from/env" },
        session: "/from/session",
      }),
    ).toBe("/from/flag");
    expect(binaryFrom({ env: { FILER_BIN: "/from/env" }, session: "/from/session" })).toBe(
      "/from/env",
    );
    expect(binaryFrom({ session: "/from/session" })).toBe("/from/session");
    expect(binaryFrom()).toBe("filer");
    expect(binaryFrom({ flag: "   ", env: { FILER_BIN: "" } })).toBe("filer");
  });

  test("FILER_TIMEOUT_MS overrides the 60 s default", () => {
    expect(timeoutFrom()).toBe(60_000);
    expect(timeoutFrom({ FILER_TIMEOUT_MS: "2500" })).toBe(2500);
    expect(timeoutFrom({ FILER_TIMEOUT_MS: "nope" })).toBe(60_000);
    expect(timeoutFrom({ FILER_TIMEOUT_MS: "-1" })).toBe(60_000);
    expect(timeoutFrom({ FILER_TIMEOUT_MS: "0" })).toBe(60_000);
  });

  test("root precedence: flag > FILER_ROOT > git > cwd, git cached per cwd", async () => {
    const calls: string[] = [];
    const roots = createRootResolver({
      env: { FILER_ROOT: "/from/env" },
      detectGitRoot: async (cwd) => {
        calls.push(cwd);
        return "/from/git";
      },
    });
    expect(await roots.resolve({ flagRoot: "/from/flag", cwd: "/work" })).toBe(
      "/from/flag",
    );
    expect(await roots.resolve({ cwd: "/work" })).toBe("/from/env");
    expect(calls).toEqual([]);

    const noEnv = createRootResolver({
      detectGitRoot: async (cwd) => {
        calls.push(cwd);
        return cwd === "/repo" ? "/repo" : null;
      },
    });
    expect(await noEnv.resolve({ cwd: "/repo" })).toBe("/repo");
    expect(await noEnv.resolve({ cwd: "/repo" })).toBe("/repo");
    expect(calls).toEqual(["/repo"]);
    expect(await noEnv.resolve({ cwd: "/not-a-repo" })).toBe("/not-a-repo");
  });

  test("detectGitRoot returns null outside a repository", async () => {
    const runner: ProcessRunner = {
      async run() {
        return {
          stdout: "",
          stderr: "fatal: not a git repository",
          code: 128,
          signal: null,
          timedOut: false,
          aborted: false,
        };
      },
    };
    expect(await detectGitRoot("/tmp", { runner })).toBeNull();
  });

  test("the client passes bin, argv, cwd and timeout to the runner", async () => {
    const seen: Array<{ bin: string; argv: string[]; cwd?: string; timeoutMs?: number }> = [];
    const runner: ProcessRunner = {
      async run(bin, argv, options) {
        seen.push({ bin, argv, cwd: options?.cwd, timeoutMs: options?.timeoutMs });
        return {
          stdout: "",
          stderr: "",
          code: 0,
          signal: null,
          timedOut: false,
          aborted: false,
        };
      },
    };
    const client = createFilerClient(
      { bin: "/opt/filer", root: "/repo", cwd: "/work", timeoutMs: 1234 },
      runner,
    );
    await client.run(["--json", "index"]);
    expect(seen).toEqual([
      { bin: "/opt/filer", argv: ["--json", "index"], cwd: "/work", timeoutMs: 1234 },
    ]);
  });

  test("the real runner never shells out (argv with spaces stays one argument)", async () => {
    const bin = stub("argv.sh", 'printf \'%s\\n\' "$@"');
    const result = await defaultRunner.run(bin, ["two words", "--flag"], {
      cwd: workdir,
    });
    expect(result.code).toBe(0);
    expect(result.stdout.trim().split("\n")).toEqual(["two words", "--flag"]);
  });
});
