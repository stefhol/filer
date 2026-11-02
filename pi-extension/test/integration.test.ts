//! Integration tests against the real binary and the checked-in Java fixture.
//!
//! The suite is skipped with an explicit message when `target/release/filer`
//! (or `FILER_BIN`) is missing, so `bun test` stays green on a fresh clone
//! before `cargo build --release`.

import { describe, expect, test } from "bun:test";
import { cpSync, existsSync, mkdtempSync, rmSync } from "node:fs";
import type {
  AgentToolResult,
  ExtensionAPI,
  ExtensionToolContext,
} from "@earendil-works/pi-coding-agent";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createFilerAdapter } from "../index";
import { createFilerClient, defaultRunner, type FilerClient } from "../src/cli";
import { bundle, callers, deps, index, refs, search, state } from "../src/operations";
import { renderBundleMarkdown } from "../src/format";
import { registerFilerTools, type BundleToolDetails } from "../src/tools";

const repoRoot = resolve(import.meta.dir, "..", "..");
const fixtureRoot = join(repoRoot, "tests", "fixtures", "java-scopes", "class-seed");
const binaryPath = process.env.FILER_BIN ?? join(repoRoot, "target", "release", "filer");
const haveBinary = existsSync(binaryPath);

if (!haveBinary) {
  console.warn(
    `skipping filer integration tests: ${binaryPath} is missing (cargo build --release, or set FILER_BIN)`,
  );
}

function client(): FilerClient {
  return createFilerClient(
    { bin: binaryPath, root: fixtureRoot, cwd: fixtureRoot, timeoutMs: 60_000 },
    defaultRunner,
  );
}

async function cliJson(argv: string[]): Promise<{ code: number | null; stdout: string }> {
  return defaultRunner.run(binaryPath, argv, { cwd: fixtureRoot });
}

async function supportsEnvelope(subcommand: "state" | "index"): Promise<boolean> {
  const result = await cliJson([
    "--root",
    fixtureRoot,
    "--json",
    "--quiet",
    subcommand,
  ]);
  if (result.code !== 0) return false;
  try {
    const value = JSON.parse(result.stdout.trim()) as Record<string, unknown>;
    if (value.schema_version !== 1) return false;
    if (subcommand === "index" && typeof value.coverage !== "object") return false;
    return true;
  } catch {
    return false;
  }
}

interface RegisteredToolLike {
  execute(
    id: string,
    params: unknown,
    signal: AbortSignal | undefined,
    onUpdate: unknown,
    ctx: ExtensionToolContext,
  ): Promise<AgentToolResult<BundleToolDetails>>;
}

/** A pi stub that only records the tools and the session-start handler. */
function toolHarness(): { pi: ExtensionAPI; tools: Map<string, RegisteredToolLike> } {
  const tools = new Map<string, RegisteredToolLike>();
  const pi = {
    registerTool(tool: RegisteredToolLike & { name: string }) {
      tools.set(tool.name, tool);
    },
    on() {
      return () => {};
    },
    getFlag() {
      return undefined;
    },
    getActiveTools() {
      return [...tools.keys()];
    },
    setActiveTools() {},
  } as unknown as ExtensionAPI;
  return { pi, tools };
}

function toolContext(cwd: string): ExtensionToolContext {
  return {
    cwd,
    ui: { notify() {} },
    signal: undefined,
  } as unknown as ExtensionToolContext;
}

const stateJsonReady = haveBinary ? await supportsEnvelope("state") : false;
const indexJsonReady = haveBinary ? await supportsEnvelope("index") : false;
if (haveBinary && (!stateJsonReady || !indexJsonReady)) {
  console.warn(
    "skipping state/index integration tests: the binary predates #39 (state --json, index coverage)",
  );
}

const service = "src/main/java/com/example/shop/Service.java";
const seed = `${service}::Service.total`;

describe("integration: java-scopes/class-seed", () => {
  test.skipIf(!haveBinary)("bundle reproduces the CLI envelope and markdown", async () => {
    const result = await bundle({ client: client() }, { seeds: [seed], depth: 2 });

    const direct = await cliJson([
      "--root",
      fixtureRoot,
      "--json",
      "--quiet",
      "bundle",
      "--depth",
      "2",
      "--",
      seed,
    ]);
    expect(direct.code).toBe(0);
    const envelope = JSON.parse(direct.stdout) as {
      entries: Array<{ path: string; symbol: string | null; scope: string }>;
    };

    const shape = (entries: Array<{ path: string; symbol?: string | null; scope: string }>) =>
      entries.map((entry) => ({
        path: entry.path,
        symbol: entry.symbol ?? null,
        scope: entry.scope,
      }));

    expect(shape(result.data.entries)).toEqual(shape(envelope.entries));
    expect(result.data.entries.length).toBeGreaterThan(0);
    expect(result.data.entries[0]?.path).toBe(service);
    expect(result.data.entries[0]?.scope).toBe("methods");
    expect(result.text).toBe(renderBundleMarkdown(envelope as never));

    // The call graph is respected: the ledger parameter is part of the closure.
    expect(
      result.data.entries.some((entry) => entry.path.endsWith("Ledger.java")),
    ).toBe(true);
  }, 60_000);

  test.skipIf(!haveBinary)("a persisted bundle rebuilds the CLI's markdown, state included", async () => {
    const workspace = mkdtempSync(join(tmpdir(), "filer-pi-bundle-"));
    try {
      cpSync(fixtureRoot, workspace, { recursive: true });
      const statePath = join(workspace, ".filer", "bundle.toml");
      const persisted = await bundle(
        {
          client: createFilerClient(
            { bin: binaryPath, root: workspace, cwd: workspace, timeoutMs: 60_000 },
            defaultRunner,
          ),
        },
        { seeds: [seed], depth: 1, persist: true },
      );
      expect(persisted.data.state_toml).toBeString();
      expect(existsSync(statePath)).toBe(true);

      // Ask the CLI for its own prose rendering with a second state file so
      // neither run carries the other over.
      const prose = await defaultRunner.run(
        binaryPath,
        [
          "--root",
          workspace,
          "--quiet",
          "bundle",
          "--depth",
          "1",
          "--state",
          join(workspace, ".filer", "prose.toml"),
          "--",
          seed,
        ],
        { cwd: workspace },
      );
      expect(prose.code).toBe(0);
      expect(persisted.text).toBe(prose.stdout);
    } finally {
      rmSync(workspace, { recursive: true, force: true });
    }
  }, 60_000);

  test.skipIf(!haveBinary)("search returns versioned rows", async () => {
    const result = await search({ client: client() }, { query: "Service", limit: 20 });
    expect(result.data.results).toBeGreaterThan(0);
    for (const row of result.data.rows) {
      expect(row.schema_version).toBe(1);
      expect(typeof row.path).toBe("string");
    }
    expect(result.data.rows.some((row) => row.path === service)).toBe(true);
    expect(result.text.length).toBeGreaterThan(0);
  }, 60_000);

  test.skipIf(!haveBinary)("deps carries units and edges", async () => {
    const result = await deps(
      { client: client() },
      { seed, unit: true, depth: 1 },
    );
    expect(result.data.granularity).toBe("unit");
    expect(result.data.seed).toBe(seed);
    expect(result.data.deps.length).toBeGreaterThan(0);
    expect(result.data.edges.length).toBeGreaterThan(0);
    for (const edge of result.data.edges) {
      expect(typeof edge.from).toBe("string");
      expect(typeof edge.to).toBe("string");
    }
    expect(result.text).toContain("(depth ");
  }, 60_000);

  test.skipIf(!haveBinary)("callers inverts the call edge", async () => {
    const result = await callers(
      { client: client() },
      { unit: "src/main/java/com/example/shop/Repo.java::Repo.repoSize", depth: 1 },
    );
    expect(result.data.rows.length).toBeGreaterThan(0);
    expect(result.data.rows[0]?.caller).toBe(`${service}::Service.total`);
    expect(result.text).toContain("Service.total");
  }, 60_000);

  test.skipIf(!haveBinary)("refs lists recorded uses", async () => {
    const result = await refs({ client: client() }, { name: "Repo" });
    expect(result.data.rows.length).toBeGreaterThan(0);
    expect(result.data.rows.some((row) => row.path === service)).toBe(true);
    expect(result.text).toContain("::");
  }, 60_000);

  test.skipIf(!haveBinary)("bundle of a missing symbol is a structured exit 2", async () => {
    let code: string | undefined;
    try {
      await bundle({ client: client() }, { seeds: [`${service}::Service.nope`] });
    } catch (error) {
      code = (error as { code?: string }).code;
    }
    expect(code).toBe("symbol_not_found");
  }, 60_000);
});

describe("integration: tools over the real binary", () => {
  test.skipIf(!haveBinary)("filer_bundle from a subdirectory matches the CLI envelope", async () => {
    const { pi, tools } = toolHarness();
    // No injected git lookup: FILER_ROOT wins from a `src/` subdirectory.
    const adapter = createFilerAdapter({
      env: { FILER_BIN: binaryPath, FILER_ROOT: fixtureRoot },
    });
    registerFilerTools(pi, { adapter });

    const tool = tools.get("filer_bundle");
    expect(tool).toBeDefined();
    const result = await tool!.execute(
      "call-1",
      { seeds: [seed], depth: 2 },
      undefined,
      undefined,
      toolContext(join(fixtureRoot, "src")),
    );

    const direct = await cliJson([
      "--root",
      fixtureRoot,
      "--json",
      "--quiet",
      "bundle",
      "--depth",
      "2",
      "--",
      seed,
    ]);
    const envelope = JSON.parse(direct.stdout);
    expect(result.content[0]).toMatchObject({
      type: "text",
      text: renderBundleMarkdown(envelope),
    });
    expect(result.details.units).toBe(envelope.entries.length);
    expect(result.details.depth).toBe(2);
    expect(result.details.coverage).toEqual(envelope.coverage);
    expect(result.isError).toBeUndefined();
  }, 60_000);

  test.skipIf(!haveBinary || !stateJsonReady)("filer_state reads #39's envelope", async () => {
    const { pi, tools } = toolHarness();
    const adapter = createFilerAdapter({
      env: { FILER_BIN: binaryPath, FILER_ROOT: fixtureRoot },
    });
    registerFilerTools(pi, { adapter });
    const result = await tools.get("filer_state")!.execute(
      "call-2",
      {},
      undefined,
      undefined,
      toolContext(fixtureRoot),
    );
    expect(result.structuredContent).toMatchObject({
      schema_version: 1,
      root: fixtureRoot,
      state_path: join(fixtureRoot, ".filer", "bundle.toml"),
    });
    expect(result.isError).toBeUndefined();
  }, 60_000);
});

describe("integration: #39 surfaces", () => {
  test.skipIf(!haveBinary || !stateJsonReady)("state --json maps the envelope", async () => {
    const result = await state({ client: client() });
    expect(result.data.schema_version).toBe(1);
    expect(result.data.root).toBe(fixtureRoot);
    expect(result.data.state_path).toBe(join(fixtureRoot, ".filer", "bundle.toml"));
    expect(Array.isArray(result.data.branches)).toBe(true);
    expect(result.text).toContain("state:");
  }, 60_000);

  test.skipIf(!haveBinary || !indexJsonReady)("index --json carries coverage", async () => {
    const result = await index({ client: client() });
    expect(result.data.coverage).toBeDefined();
    expect(result.data.coverage?.files_indexed).toBeGreaterThan(0);
    expect(result.data.coverage?.files_indexed).toBe(result.data.source_files);
    expect(result.text).toContain("coverage:");
  }, 60_000);
});
