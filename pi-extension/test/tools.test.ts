//! The eight tools: registration, argv, result mapping and the session-start
//! usability check. Everything runs through injected fakes, so no binary and no
//! pi runtime are needed.

import { describe, expect, test } from "bun:test";
import type {
  AgentToolResult,
  ExtensionAPI,
  ExtensionToolContext,
} from "@earendil-works/pi-coding-agent";
import { Value } from "typebox/value";
import { createFilerAdapter } from "../index";
import type { ProcessRunner, RunResult } from "../src/cli";
import { renderBundleMarkdown } from "../src/format";
import {
  FILER_INSTALL_HINT,
  registerFilerTools,
  type BundleToolDetails,
  type FilerToolDetails,
} from "../src/tools";

// --- fakes ----------------------------------------------------------------

interface FakeTheme {
  fg(token: string, text: string): string;
  bold(text: string): string;
}

const theme: FakeTheme = {
  fg: (_token, text) => text,
  bold: (text) => text,
};

interface RegisteredTool {
  name: string;
  annotations?: { readOnlyHint?: boolean; destructiveHint?: boolean };
  namespace?: { name?: string };
  outputSchema?: unknown;
  execute(
    id: string,
    params: unknown,
    signal: AbortSignal | undefined,
    onUpdate: unknown,
    ctx: ExtensionToolContext,
  ): Promise<AgentToolResult<FilerToolDetails>>;
  renderCall?: (args: unknown, theme: FakeTheme, context: unknown) => unknown;
  renderResult?: (
    result: AgentToolResult<FilerToolDetails>,
    options: unknown,
    theme: FakeTheme,
    context: unknown,
  ) => unknown;
}

interface FakePi {
  api: ExtensionAPI;
  tools: Map<string, RegisteredTool>;
  handlers: Map<string, (event: unknown, ctx: ExtensionToolContext) => unknown>;
  notifications: Array<{ message: string; type?: string }>;
  activeTools: string[];
}

function fakePi(flags: Record<string, string> = {}): FakePi {
  const tools = new Map<string, RegisteredTool>();
  const handlers = new Map<string, (event: unknown, ctx: ExtensionToolContext) => unknown>();
  const notifications: Array<{ message: string; type?: string }> = [];
  const activeTools: string[] = [];
  const api = {
    registerTool(tool: RegisteredTool) {
      tools.set(tool.name, tool);
      activeTools.push(tool.name);
    },
    on(event: string, handler: (event: unknown, ctx: ExtensionToolContext) => unknown) {
      handlers.set(event, handler);
      return () => {};
    },
    getFlag(name: string) {
      return flags[name];
    },
    getActiveTools() {
      return [...activeTools];
    },
    setActiveTools(names: string[]) {
      activeTools.splice(0, activeTools.length, ...names);
    },
  } as unknown as ExtensionAPI;
  return { api, tools, handlers, notifications, activeTools };
}

function fakeCtx(notifications: Array<{ message: string; type?: string }>): ExtensionToolContext {
  return {
    cwd: "/repo",
    ui: {
      notify(message: string, type?: string) {
        notifications.push({ message, ...(type === undefined ? {} : { type }) });
      },
    },
    signal: undefined,
  } as unknown as ExtensionToolContext;
}

interface Recorded {
  bin: string;
  argv: string[];
  cwd?: string;
  timeoutMs?: number;
}

function fakeRunner(
  handler: (argv: string[]) => Partial<RunResult> | Promise<Partial<RunResult>>,
): { runner: ProcessRunner; calls: Recorded[] } {
  const calls: Recorded[] = [];
  return {
    calls,
    runner: {
      async run(bin, argv, options = {}) {
        calls.push({
          bin,
          argv,
          ...(options.cwd === undefined ? {} : { cwd: options.cwd }),
          ...(options.timeoutMs === undefined ? {} : { timeoutMs: options.timeoutMs }),
        });
        const result = await handler(argv);
        return {
          stdout: "",
          stderr: "",
          code: 0,
          signal: null,
          timedOut: false,
          aborted: false,
          bin,
          ...result,
        };
      },
    },
  };
}

function setup(
  handler: (argv: string[]) => Partial<RunResult> | Promise<Partial<RunResult>>,
  flags: Record<string, string> = {},
): FakePi & { calls: Recorded[]; ctx: ExtensionToolContext } {
  const pi = fakePi(flags);
  const { runner, calls } = fakeRunner(handler);
  const adapter = createFilerAdapter({
    env: { FILER_BIN: "/opt/filer" },
    runner,
    detectGitRoot: async () => "/repo",
  });
  registerFilerTools(pi.api, { adapter });
  return { ...pi, calls, ctx: fakeCtx(pi.notifications) };
}

async function run<D extends FilerToolDetails = FilerToolDetails>(
  pi: FakePi,
  ctx: ExtensionToolContext,
  name: string,
  params: unknown,
): Promise<AgentToolResult<D>> {
  const tool = pi.tools.get(name);
  if (tool === undefined) throw new Error(`tool ${name} is not registered`);
  return tool.execute("call-1", params, undefined, undefined, ctx) as Promise<
    AgentToolResult<D>
  >;
}

function textOf(result: AgentToolResult<FilerToolDetails>): string {
  const first = result.content[0];
  if (first === undefined || first.type !== "text") return "";
  return first.text;
}

function subcommand(argv: string[]): string {
  return argv.find((argument) =>
    ["bundle", "show", "search", "deps", "callers", "why", "refs", "state"].includes(
      argument,
    ),
  ) ?? "";
}

// --- canned envelopes -----------------------------------------------------

const bundleEnvelope = {
  schema_version: 1,
  root: "/repo",
  title: "demo",
  branch: "main",
  depth: 1,
  files: 1,
  excerpts: 1,
  methods: 0,
  budget: {
    max_file_bytes: 1048576,
    max_bytes: null,
    max_tokens: null,
    used_bytes: 10,
    used_tokens: 2,
    truncated: false,
  },
  entries: [
    {
      path: "a/A.java",
      scope: "methods",
      symbol: "A",
      methods: ["use"],
      depth: 0,
      reason: "seed",
      start_line: 1,
      end_line: 2,
      start_byte: 0,
      end_byte: 10,
      tokens: 2,
      hash: "h",
      rendered: true,
      label: "`a/A.java` :: `A` [use]",
      markdown: "\n### `a/A.java` :: `A` (line 1)\n\n```java\nclass A {}\n```\n",
    },
  ],
  omitted: [],
  diagnostics: [],
  coverage: { files_indexed: 1, files_skipped: 0, imports_unresolved: 0 },
  state_toml: null,
};

const searchRow = {
  schema_version: 1,
  path: "a/A.java",
  scope: "methods",
  symbol: "A",
  member: "use",
  kind: "method",
  fqn: "app.A.use",
  line: 3,
  start_line: 3,
  end_line: 6,
  bytes: 120,
  est_tokens: 30,
  package: "app",
  included: true,
  depth: 1,
  reason: "import",
};

const depsEnvelope = {
  schema_version: 1,
  seed: "a/A.java::A.use",
  granularity: "unit",
  deps: [
    {
      path: "a/B.java",
      fqn: "app.B",
      depth: 1,
      reason: "call",
      unit: "a/B.java::B.size",
      scope: "methods",
      symbol: "B",
      member: "size",
    },
  ],
  edges: [{ from: "a/A.java::A.use", to: "a/B.java::B.size", reason: "call", line: 5 }],
  diagnostics: [],
};

const whyEnvelope = {
  target: "a/B.java::B.size",
  reason: "call",
  depth: 1,
  chain: [
    {
      unit: "a/A.java::A.use",
      path: "a/A.java",
      scope: "methods",
      symbol: "A",
      member: "use",
      line: null,
      reason: "seed",
      depth: 0,
    },
    {
      unit: "a/B.java::B.size",
      path: "a/B.java",
      scope: "methods",
      symbol: "B",
      member: "size",
      line: 5,
      reason: "call",
      depth: 1,
    },
  ],
};

const stateEnvelope = {
  schema_version: 1,
  root: "/repo",
  state_path: "/repo/.filer/bundle.toml",
  branch: "main",
  label: "main",
  exists: true,
  current: {
    branch: "main",
    created: 1793618434,
    depth: 1,
    entries: [
      {
        path: "a/A.java",
        scope: "methods",
        symbol: "A",
        methods: ["use"],
        depth: 0,
        reason: "seed",
      },
    ],
  },
  branches: [{ branch: "main", label: "main", entries: 1, created: 1793618434, depth: 1 }],
};

const JSON_BY_SUBCOMMAND: Record<string, unknown> = {
  bundle: bundleEnvelope,
  show: bundleEnvelope,
  search: searchRow,
  deps: depsEnvelope,
  callers: [{ caller: "a/A.java::A.use", line: 5, depth: 1 }],
  why: whyEnvelope,
  refs: [{ path: "a/A.java", symbol: "A", member: null, line: 2, kind: "use" }],
  state: stateEnvelope,
};

function jsonHandler(): (argv: string[]) => Partial<RunResult> {
  return (argv) => {
    const command = subcommand(argv);
    const payload = JSON_BY_SUBCOMMAND[command];
    if (payload === undefined) return {};
    // Callers/refs are one JSON array; the other envelopes (and this single
    // search row) are one JSON object.
    return { stdout: JSON.stringify(payload) };
  };
}

// --- registration ---------------------------------------------------------

describe("registration", () => {
  test("registers exactly the eight documented tools", () => {
    const pi = setup(jsonHandler());
    expect([...pi.tools.keys()]).toEqual([
      "filer_bundle",
      "filer_show",
      "filer_search",
      "filer_deps",
      "filer_callers",
      "filer_why",
      "filer_refs",
      "filer_state",
    ]);
    for (const tool of pi.tools.values()) {
      expect(tool.namespace?.name).toBe("filer");
      expect(typeof tool.renderCall).toBe("function");
      expect(typeof tool.renderResult).toBe("function");
    }
  });

  test("read-only hints: everything but a persisting bundle", () => {
    const pi = setup(jsonHandler());
    expect(pi.tools.get("filer_bundle")?.annotations).toMatchObject({
      readOnlyHint: false,
      destructiveHint: false,
    });
    for (const name of [
      "filer_show",
      "filer_search",
      "filer_deps",
      "filer_callers",
      "filer_why",
      "filer_refs",
      "filer_state",
    ]) {
      expect(pi.tools.get(name)?.annotations).toMatchObject({ readOnlyHint: true });
    }
  });

  test("data tools declare an outputSchema; bundle tools carry markdown instead", () => {
    const pi = setup(jsonHandler());
    for (const name of [
      "filer_search",
      "filer_deps",
      "filer_callers",
      "filer_why",
      "filer_refs",
      "filer_state",
    ]) {
      expect(pi.tools.get(name)?.outputSchema).toBeDefined();
    }
    expect(pi.tools.get("filer_bundle")?.outputSchema).toBeUndefined();
    expect(pi.tools.get("filer_show")?.outputSchema).toBeUndefined();
  });
});

// --- argv and result mapping ---------------------------------------------

describe("bundle", () => {
  test("passes words as positionals after --, with an absolute state path", async () => {
    const pi = setup(jsonHandler());
    const result = await run<BundleToolDetails>(pi, pi.ctx, "filer_bundle", {
      seeds: ["a/A.java::A.use", "--not-a-flag.java"],
      depth: 1,
      persist: true,
      title: "demo",
    });
    const call = pi.calls[0]!;
    expect(call.bin).toBe("/opt/filer");
    expect(call.cwd).toBe("/repo");
    expect(call.argv.slice(0, 4)).toEqual(["--root", "/repo", "--json", "--quiet"]);
    expect(call.argv).toEqual([
      "--root",
      "/repo",
      "--json",
      "--quiet",
      "bundle",
      "--depth",
      "1",
      "--title",
      "demo",
      "--state",
      "/repo/.filer/bundle.toml",
      "--",
      "a/A.java::A.use",
      "--not-a-flag.java",
    ]);
    expect(textOf(result)).toBe(renderBundleMarkdown(bundleEnvelope as never));
    expect(result.details.units).toBe(1);
    expect(result.details.depth).toBe(1);
    expect(result.details.branch).toBe("main");
    expect(result.details.coverage).toEqual(bundleEnvelope.coverage);
    expect(result.structuredContent).toBeUndefined();
    expect(result.isError).toBeUndefined();
  });

  test("a missing symbol is a clean code + hint error", async () => {
    const stderr = JSON.stringify({
      error: {
        code: "symbol_not_found",
        message: "a/A.java has no symbol Nope",
        hint: "pass a path alone to bundle the whole file",
      },
    });
    const pi = setup((argv) =>
      subcommand(argv) === "bundle" ? { code: 2, stderr } : {},
    );
    const result = await run(pi, pi.ctx, "filer_bundle", { seeds: ["a/A.java::Nope"] });
    expect(result.isError).toBe(true);
    expect(textOf(result)).toBe(
      "symbol_not_found: pass a path alone to bundle the whole file",
    );
    expect(textOf(result)).not.toContain("a/A.java has no symbol Nope");
    expect(result.details.error).toEqual({
      code: "symbol_not_found",
      message: "pass a path alone to bundle the whole file",
      hint: "pass a path alone to bundle the whole file",
      details: { message: "a/A.java has no symbol Nope" },
    });
  });

  test("renderers are one-liners that survive errors", () => {
    const pi = setup(jsonHandler());
    const tool = pi.tools.get("filer_bundle")!;
    expect(
      tool.renderCall?.({ seeds: ["a/A.java"], depth: 1 }, theme, undefined),
    ).toBeDefined();
    expect(
      tool.renderResult?.(
        {
          content: [{ type: "text", text: "" }],
          details: { summary: "filer bundle: 0 units", warnings: [] },
          isError: true,
        },
        {},
        theme,
        undefined,
      ),
    ).toBeDefined();
  });
});

describe("show / search / deps / callers / why / refs / state", () => {
  test("show passes --branch and returns the rebuilt markdown", async () => {
    const pi = setup(jsonHandler());
    const result = await run<BundleToolDetails>(pi, pi.ctx, "filer_show", { branch: "main" });
    expect(pi.calls[0]!.argv).toEqual([
      "--root",
      "/repo",
      "--json",
      "--quiet",
      "show",
      "--branch",
      "main",
    ]);
    expect(textOf(result)).toBe(renderBundleMarkdown(bundleEnvelope as never));
    expect(result.details.units).toBe(1);
  });

  test("search returns structuredContent matching its outputSchema", async () => {
    const pi = setup(jsonHandler());
    const tool = pi.tools.get("filer_search")!;
    const result = await run(pi, pi.ctx, "filer_search", { query: "Service", limit: 5 });
    expect(pi.calls[0]!.argv).toEqual([
      "--root",
      "/repo",
      "--json",
      "--quiet",
      "search",
      "--query",
      "Service",
      "--limit",
      "5",
    ]);
    expect(result.structuredContent).toBeDefined();
    expect(Value.Check(tool.outputSchema as never, result.structuredContent)).toBe(true);
    expect(textOf(result)).toBe(
      "a/A.java::A.use — app.A.use (method, line 3, in bundle)",
    );
    expect(result.details.summary).toBe("filer search: 1 rows");
  });

  test("search exit 4 is a successful empty result with the hint", async () => {
    const hint = "symbol/path index only; use text search for file contents";
    const pi = setup((argv) =>
      subcommand(argv) === "search"
        ? {
            code: 4,
            stdout: JSON.stringify({ schema_version: 1, hint, results: 0 }),
          }
        : {},
    );
    const tool = pi.tools.get("filer_search")!;
    const result = await run(pi, pi.ctx, "filer_search", { query: "nothing" });
    expect(result.isError).toBeUndefined();
    expect(textOf(result)).toBe(hint);
    expect(result.structuredContent).toMatchObject({ results: 0, hint });
    expect(Value.Check(tool.outputSchema as never, result.structuredContent)).toBe(true);
  });

  test("deps keeps the unit flag and validates its schema", async () => {
    const pi = setup(jsonHandler());
    const tool = pi.tools.get("filer_deps")!;
    const result = await run(pi, pi.ctx, "filer_deps", {
      seed: "a/A.java::A.use",
      unit: true,
      depth: 2,
    });
    expect(pi.calls[0]!.argv).toEqual([
      "--root",
      "/repo",
      "--json",
      "--quiet",
      "deps",
      "--depth",
      "2",
      "--unit",
      "--",
      "a/A.java::A.use",
    ]);
    expect(Value.Check(tool.outputSchema as never, result.structuredContent)).toBe(true);
    expect(result.details.summary).toBe("filer deps: 1 units, 1 edges");
  });

  test("callers and refs keep -- before the unit/name", async () => {
    const pi = setup(jsonHandler());
    const callersTool = pi.tools.get("filer_callers")!;
    const refsTool = pi.tools.get("filer_refs")!;

    const callersResult = await run(pi, pi.ctx, "filer_callers", {
      unit: "a/B.java::B.size",
      depth: 2,
    });
    expect(pi.calls[0]!.argv).toEqual([
      "--root",
      "/repo",
      "--json",
      "--quiet",
      "callers",
      "--depth",
      "2",
      "--",
      "a/B.java::B.size",
    ]);
    expect(Value.Check(callersTool.outputSchema as never, callersResult.structuredContent)).toBe(
      true,
    );

    const refsResult = await run(pi, pi.ctx, "filer_refs", { name: "TaxRate" });
    expect(pi.calls[1]!.argv).toEqual([
      "--root",
      "/repo",
      "--json",
      "--quiet",
      "refs",
      "--",
      "TaxRate",
    ]);
    expect(Value.Check(refsTool.outputSchema as never, refsResult.structuredContent)).toBe(true);
    expect(refsResult.details.summary).toBe("filer refs: 1 references");
  });

  test("why explains the chain and validates its schema", async () => {
    const pi = setup(jsonHandler());
    const tool = pi.tools.get("filer_why")!;
    const result = await run(pi, pi.ctx, "filer_why", { unit: "a/B.java::B.size" });
    expect(pi.calls[0]!.argv).toEqual([
      "--root",
      "/repo",
      "--json",
      "--quiet",
      "why",
      "--",
      "a/B.java::B.size",
    ]);
    expect(Value.Check(tool.outputSchema as never, result.structuredContent)).toBe(true);
    expect(textOf(result)).toContain("seed a/A.java::A.use");
    expect(result.details.summary).toBe("filer why: 2 hops, call");
  });

  test("state is read-only and validates its schema", async () => {
    const pi = setup(jsonHandler());
    const tool = pi.tools.get("filer_state")!;
    const result = await run(pi, pi.ctx, "filer_state", {});
    expect(pi.calls[0]!.argv).toEqual(["--root", "/repo", "--json", "--quiet", "state"]);
    expect(Value.Check(tool.outputSchema as never, result.structuredContent)).toBe(true);
    expect(result.details.summary).toBe("filer state: 1 branches, 1 current entries");
  });

  test("the --filer-bin and --filer-root flags win over the env", async () => {
    const pi = setup(jsonHandler(), {
      "filer-bin": "/from/flag",
      "filer-root": "/from/flag-root",
    });
    await run(pi, pi.ctx, "filer_state", {});
    expect(pi.calls[0]!.bin).toBe("/from/flag");
    expect(pi.calls[0]!.argv.slice(0, 2)).toEqual(["--root", "/from/flag-root"]);
  });

  test("the client timeout is the 60 s default, not the version-check budget", async () => {
    const pi = setup(jsonHandler());
    await run(pi, pi.ctx, "filer_state", {});
    expect(pi.calls[0]!.timeoutMs).toBe(60_000);
  });
});

// --- session_start --------------------------------------------------------

describe("session_start usability check", () => {
  test("a working binary checks --version once and changes nothing", async () => {
    const pi = setup((argv) =>
      argv.includes("--version") ? { stdout: "filer 0.2.0\n" } : {},
    );
    const handler = pi.handlers.get("session_start");
    expect(handler).toBeDefined();
    await handler!({ type: "session_start", reason: "startup" }, pi.ctx);

    expect(pi.calls).toHaveLength(1);
    expect(pi.calls[0]!.argv).toEqual(["--version"]);
    expect(pi.calls[0]!.timeoutMs).toBe(5_000);
    expect(pi.notifications).toEqual([]);
    expect(pi.activeTools).toHaveLength(8);
  });

  test("a missing binary notifies and removes every filer_* tool", async () => {
    const pi = setup((argv) =>
      argv.includes("--version")
        ? { code: null, spawnError: { code: "ENOENT", message: "spawn filer ENOENT" } }
        : {},
    );
    const handler = pi.handlers.get("session_start")!;
    await handler({ type: "session_start", reason: "startup" }, pi.ctx);

    expect(pi.notifications).toHaveLength(1);
    expect(pi.notifications[0]?.type).toBe("error");
    expect(pi.notifications[0]?.message).toContain(FILER_INSTALL_HINT);
    expect(pi.notifications[0]?.message).not.toContain("ENOENT");
    expect(pi.activeTools).toEqual([]);
  });

  test("a binary that is not filer also disables the tools", async () => {
    const pi = setup((argv) =>
      argv.includes("--version") ? { stdout: "git version 2.44\n" } : {},
    );
    const handler = pi.handlers.get("session_start")!;
    await handler({ type: "session_start", reason: "startup" }, pi.ctx);
    expect(pi.notifications).toHaveLength(1);
    expect(pi.activeTools).toEqual([]);
  });

  test("a hung --version times out instead of blocking startup for 60 s", async () => {
    const pi = setup((argv) =>
      argv.includes("--version")
        ? { code: null, timedOut: true, timeoutMs: 5_000 }
        : {},
    );
    const handler = pi.handlers.get("session_start")!;
    await handler({ type: "session_start", reason: "startup" }, pi.ctx);
    expect(pi.notifications).toHaveLength(1);
    expect(pi.activeTools).toEqual([]);
  });
});
