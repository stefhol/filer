import { describe, expect, test } from "bun:test";
import { join } from "node:path";
import {
  bundleArgs,
  callersArgs,
  depsArgs,
  globalArgs,
  indexArgs,
  refsArgs,
  searchArgs,
  showArgs,
  stateArgs,
  statePath,
  whyArgs,
} from "../src/args";

const root = "/repo";
const state = join(root, ".filer", "bundle.toml");

describe("global argv", () => {
  test("every invocation starts with --root, --json and --quiet", () => {
    expect(globalArgs(root)).toEqual(["--root", root, "--json", "--quiet"]);
  });

  test("statePath is absolute and keeps a trailing slash out", () => {
    expect(statePath(root)).toBe(state);
    expect(statePath(`${root}/`)).toBe(state);
  });
});

describe("bundle argv", () => {
  test("flags precede the -- separated seed list", () => {
    expect(
      bundleArgs(root, {
        seeds: ["app/Service.java", "app/Repo.java::Repo.size"],
        depth: 1,
        reset: true,
        persist: true,
        withConfig: true,
        withSamePackage: true,
        title: "demo",
        maxFileBytes: 1024,
        maxBytes: 4096,
        atLine: 3,
        skip: ["app/Unused.java"],
      }),
    ).toEqual([
      "--root",
      root,
      "--json",
      "--quiet",
      "bundle",
      "--depth",
      "1",
      "--max-file-bytes",
      "1024",
      "--max-bytes",
      "4096",
      "--with-same-package",
      "--with-config",
      "--title",
      "demo",
      "--state",
      state,
      "--reset",
      "--at-line",
      "3",
      "--skip",
      "app/Unused.java",
      "--",
      "app/Service.java",
      "app/Repo.java::Repo.size",
    ]);
  });

  test("--state is the absolute default state file, never a relative path", () => {
    const argv = bundleArgs(root, { seeds: ["A.java"], persist: true });
    const at = argv.indexOf("--state");
    expect(argv[at + 1]).toBe(state);
    expect(argv[at + 1]?.startsWith("/")).toBe(true);
  });

  test("max-bytes and max-tokens never appear together", () => {
    expect(() =>
      bundleArgs(root, { seeds: ["A.java"], maxBytes: 1, maxTokens: 1 }),
    ).toThrow("mutually exclusive");
  });

  test("no seeds means no trailing --", () => {
    expect(bundleArgs(root, { seeds: [], fromState: true })).toEqual([
      "--root",
      root,
      "--json",
      "--quiet",
      "bundle",
      "--from-state",
    ]);
  });
});

describe("per-command argv", () => {
  test("search", () => {
    expect(searchArgs(root, { query: "Service", limit: 5 })).toEqual([
      "--root",
      root,
      "--json",
      "--quiet",
      "search",
      "--query",
      "Service",
      "--limit",
      "5",
    ]);
  });

  test("show", () => {
    expect(showArgs(root, { branch: "main" })).toEqual([
      "--root",
      root,
      "--json",
      "--quiet",
      "show",
      "--branch",
      "main",
    ]);
  });

  test("state uses --file for the state path", () => {
    expect(stateArgs(root, { file: state, branch: "main", clear: true })).toEqual([
      "--root",
      root,
      "--json",
      "--quiet",
      "state",
      "--file",
      state,
      "--branch",
      "main",
      "--clear",
    ]);
  });

  test("index", () => {
    expect(indexArgs(root)).toEqual(["--root", root, "--json", "--quiet", "index"]);
  });

  test("deps", () => {
    expect(depsArgs(root, { seed: "app/Repo.java::Repo.size", unit: true, depth: 2 })).toEqual([
      "--root",
      root,
      "--json",
      "--quiet",
      "deps",
      "--depth",
      "2",
      "--unit",
      "--",
      "app/Repo.java::Repo.size",
    ]);
  });

  test("callers", () => {
    expect(callersArgs(root, { unit: "app/Repo.java::Repo.size", depth: 2 })).toEqual([
      "--root",
      root,
      "--json",
      "--quiet",
      "callers",
      "--depth",
      "2",
      "--",
      "app/Repo.java::Repo.size",
    ]);
  });

  test("why", () => {
    expect(
      whyArgs(root, { unit: "app/Repo.java::Repo.size", state, branch: "main" }),
    ).toEqual([
      "--root",
      root,
      "--json",
      "--quiet",
      "why",
      "--state",
      state,
      "--branch",
      "main",
      "--",
      "app/Repo.java::Repo.size",
    ]);
  });

  test("refs", () => {
    expect(refsArgs(root, { name: "TaxRate" })).toEqual([
      "--root",
      root,
      "--json",
      "--quiet",
      "refs",
      "--",
      "TaxRate",
    ]);
  });

  test("every builder is a fresh array (no shared mutable argv)", () => {
    const first = indexArgs(root);
    first.push("--mutated");
    expect(indexArgs(root)).toEqual(["--root", root, "--json", "--quiet", "index"]);
  });

  test("--json and --quiet are on every command, in that order", () => {
    const argvs = [
      bundleArgs(root, { seeds: ["A.java"] }),
      showArgs(root),
      stateArgs(root),
      indexArgs(root),
      searchArgs(root, { query: "" }),
      depsArgs(root, { seed: "A.java" }),
      callersArgs(root, { unit: "A.java" }),
      whyArgs(root, { unit: "A.java" }),
      refsArgs(root, { name: "A" }),
    ];
    for (const argv of argvs) {
      expect(argv.slice(0, 4)).toEqual(["--root", root, "--json", "--quiet"]);
    }
  });

  test("a seed that starts with a dash is still a positional", () => {
    const argv = bundleArgs(root, { seeds: ["--weird.java"] });
    expect(argv.slice(-2)).toEqual(["--", "--weird.java"]);
  });
});
