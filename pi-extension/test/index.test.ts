import { describe, expect, test } from "bun:test";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import filerExtension, {
  EXTENSION_VERSION,
  FILER_BIN_FLAG,
  FILER_ROOT_FLAG,
  createFilerAdapter,
} from "../index";
import type { ProcessRunner } from "../src/cli";

interface FakePi {
  api: ExtensionAPI;
  flags: Array<{ name: string; options: unknown }>;
  tools: unknown[];
  commands: unknown[];
}

function fakePi(): FakePi {
  const flags: FakePi["flags"] = [];
  const tools: unknown[] = [];
  const commands: unknown[] = [];
  const api = {
    registerFlag(name: string, options: unknown) {
      flags.push({ name, options });
    },
    registerTool(tool: unknown) {
      tools.push(tool);
    },
    registerCommand(name: string, options: unknown) {
      commands.push({ name, options });
    },
  } as unknown as ExtensionAPI;
  return { api, flags, tools, commands };
}

const failingRunner: ProcessRunner = {
  async run() {
    throw new Error("the factory must not spawn a process");
  },
};

describe("extension factory", () => {
  test("registers exactly the two flags and nothing else", () => {
    const { api, flags, tools, commands } = fakePi();
    let gitLookups = 0;
    filerExtension(api, {
      runner: failingRunner,
      detectGitRoot: async () => {
        gitLookups += 1;
        return "/repo";
      },
    });
    expect(flags.map((flag) => flag.name)).toEqual([FILER_BIN_FLAG, FILER_ROOT_FLAG]);
    for (const flag of flags) {
      expect(flag.options).toMatchObject({ type: "string" });
    }
    expect(tools).toEqual([]);
    expect(commands).toEqual([]);
    expect(gitLookups).toBe(0);
  });

  test("the factory is synchronous and process-free", () => {
    const { api } = fakePi();
    let spawns = 0;
    const runner: ProcessRunner = {
      async run() {
        spawns += 1;
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
    const returned = filerExtension(api, { runner, detectGitRoot: async () => null });
    expect(returned).toBeUndefined();
    expect(spawns).toBe(0);
    expect(EXTENSION_VERSION).toMatch(/^\d+\.\d+\.\d+$/);
  });
});

describe("adapter", () => {
  test("resolves binary, root and timeout without spawning", async () => {
    let spawns = 0;
    let lookups = 0;
    const runner: ProcessRunner = {
      async run() {
        spawns += 1;
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
    const adapter = createFilerAdapter({
      env: { FILER_BIN: "/from/env", FILER_TIMEOUT_MS: "2500" },
      runner,
      detectGitRoot: async (cwd) => {
        lookups += 1;
        return cwd === "/work" ? "/repo" : null;
      },
    });

    const flag = await adapter.resolve({
      cwd: "/work",
      flagBin: "/from/flag",
      flagRoot: "/from/flag-root",
    });
    expect(flag).toEqual({
      bin: "/from/flag",
      root: "/from/flag-root",
      cwd: "/work",
      timeoutMs: 2500,
    });

    const discovered = await adapter.resolve({ cwd: "/work" });
    expect(discovered.bin).toBe("/from/env");
    expect(discovered.root).toBe("/repo");

    // The git lookup is cached per session/cwd.
    await adapter.resolve({ cwd: "/work" });
    expect(lookups).toBe(1);
    expect(spawns).toBe(0);
  });

  test("session binary overrides the PATH fallback but not the env", async () => {
    const adapter = createFilerAdapter({
      detectGitRoot: async () => "/repo",
      runner: failingRunner,
    });
    expect((await adapter.resolve({ cwd: "/repo", sessionBin: "/session" })).bin).toBe(
      "/session",
    );
    expect((await adapter.resolve({ cwd: "/repo" })).bin).toBe("filer");
  });

  test("client binds the resolved runtime to the injected runner", async () => {
    const calls: string[][] = [];
    const runner: ProcessRunner = {
      async run(_bin, argv) {
        calls.push(argv);
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
    const adapter = createFilerAdapter({
      runner,
      detectGitRoot: async () => "/repo",
    });
    const resolved = await adapter.resolve({ cwd: "/repo" });
    await adapter.client(resolved).run(["--json", "index"]);
    expect(calls).toEqual([["--json", "index"]]);
    expect(resolved.root).toBe("/repo");
  });
});
