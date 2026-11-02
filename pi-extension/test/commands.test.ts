import { describe, expect, test } from "bun:test";
import {
  countNewUnits,
  parseFilerCommand,
  tokenize,
  unitKey,
  whyChainText,
} from "../src/commands";
import type { WhyData, WhyHop } from "../src/operations";

describe("tokenize", () => {
  test("splits on whitespace", () => {
    expect(tokenize("bundle src/App.java --depth 2")).toEqual([
      "bundle",
      "src/App.java",
      "--depth",
      "2",
    ]);
  });

  test("honors double quotes and escapes", () => {
    expect(tokenize('bundle "src/App with space.java"')).toEqual([
      "bundle",
      "src/App with space.java",
    ]);
    expect(tokenize('bin "/opt/my filer"')).toEqual(["bin", "/opt/my filer"]);
    expect(tokenize('bundle "a\\"b"')).toEqual(["bundle", 'a"b']);
  });

  test("honors single quotes literally", () => {
    expect(tokenize("bundle 'a b'")).toEqual(["bundle", "a b"]);
    expect(tokenize("bin 'a\\b'")).toEqual(["bin", "a\\b"]);
  });

  test("ignores extra whitespace and an unterminated quote", () => {
    expect(tokenize("  bundle   src  ")).toEqual(["bundle", "src"]);
    expect(tokenize('bundle "unterminated')).toEqual(["bundle", "unterminated"]);
  });
});

describe("parseFilerCommand", () => {
  test("empty is help", () => {
    expect(parseFilerCommand("")).toEqual({ kind: "help" });
    expect(parseFilerCommand("   ")).toEqual({ kind: "help" });
  });

  test("bundle seeds/depth/reset", () => {
    expect(parseFilerCommand("bundle src/App.java")).toEqual({
      kind: "bundle",
      seeds: ["src/App.java"],
      reset: false,
    });
    expect(parseFilerCommand("bundle src/App.java --depth 2")).toEqual({
      kind: "bundle",
      seeds: ["src/App.java"],
      depth: 2,
      reset: false,
    });
    expect(parseFilerCommand("bundle src/App.java --depth=2 --reset")).toEqual({
      kind: "bundle",
      seeds: ["src/App.java"],
      depth: 2,
      reset: true,
    });
    expect(parseFilerCommand("bundle -- --weird")).toEqual({
      kind: "bundle",
      seeds: ["--weird"],
      reset: false,
    });
  });

  test("bundle rejects bad input", () => {
    expect(parseFilerCommand("bundle")).toMatchObject({ kind: "invalid" });
    expect(parseFilerCommand("bundle --depth x src/App.java")).toMatchObject({
      kind: "invalid",
    });
    expect(parseFilerCommand("bundle --depth -1 src/App.java")).toMatchObject({
      kind: "invalid",
    });
    expect(parseFilerCommand("bundle --unknown src/App.java")).toMatchObject({
      kind: "invalid",
    });
  });

  test("show", () => {
    expect(parseFilerCommand("show")).toEqual({ kind: "show" });
    expect(parseFilerCommand("show main")).toEqual({ kind: "show", branch: "main" });
    expect(parseFilerCommand("show main extra")).toMatchObject({ kind: "invalid" });
    expect(parseFilerCommand("show --branch")).toMatchObject({ kind: "invalid" });
  });

  test("state/index take no arguments", () => {
    expect(parseFilerCommand("state")).toEqual({ kind: "state" });
    expect(parseFilerCommand("state extra")).toMatchObject({ kind: "invalid" });
    expect(parseFilerCommand("index")).toEqual({ kind: "index" });
    expect(parseFilerCommand("index extra")).toMatchObject({ kind: "invalid" });
  });

  test("reset", () => {
    expect(parseFilerCommand("reset")).toEqual({ kind: "reset", all: false });
    expect(parseFilerCommand("reset --all")).toEqual({ kind: "reset", all: true });
    expect(parseFilerCommand("reset --nope")).toMatchObject({ kind: "invalid" });
    expect(parseFilerCommand("reset extra")).toMatchObject({ kind: "invalid" });
  });

  test("why", () => {
    expect(parseFilerCommand("why src/App.java")).toEqual({
      kind: "why",
      unit: "src/App.java",
    });
    expect(parseFilerCommand("why src/App.java service")).toEqual({
      kind: "why",
      unit: "src/App.java service",
    });
    expect(parseFilerCommand("why")).toMatchObject({ kind: "invalid" });
    expect(parseFilerCommand("why --nope")).toMatchObject({ kind: "invalid" });
  });

  test("bin/root", () => {
    expect(parseFilerCommand("bin")).toEqual({ kind: "bin" });
    expect(parseFilerCommand("bin /opt/filer")).toEqual({
      kind: "bin",
      path: "/opt/filer",
    });
    expect(parseFilerCommand("bin /opt/my filer")).toEqual({
      kind: "bin",
      path: "/opt/my filer",
    });
    expect(parseFilerCommand("root")).toEqual({ kind: "root" });
    expect(parseFilerCommand("root /repo")).toEqual({ kind: "root", path: "/repo" });
    expect(parseFilerCommand("bin --nope")).toMatchObject({ kind: "invalid" });
  });

  test("unknown", () => {
    expect(parseFilerCommand("nope")).toEqual({ kind: "unknown", subcommand: "nope" });
  });
});

describe("countNewUnits", () => {
  const unit = (path: string, methods: string[] = [], symbol: string | null = null) => ({
    path,
    scope: "file",
    symbol,
    methods,
  });

  test("all new against empty", () => {
    expect(countNewUnits([], [unit("a"), unit("b")])).toBe(2);
  });

  test("counts only the units not already present", () => {
    expect(countNewUnits([unit("a")], [unit("a"), unit("b")])).toBe(1);
  });

  test("method order and symbol/scope matter", () => {
    expect(countNewUnits([unit("a", ["x", "y"])], [unit("a", ["y", "x"])])).toBe(0);
    expect(countNewUnits([unit("a", ["x"])], [unit("a", ["x"], "A")])).toBe(1);
    expect(
      countNewUnits(
        [{ ...unit("a"), scope: "file" }],
        [{ ...unit("a"), scope: "methods" }],
      ),
    ).toBe(1);
  });

  test("unitKey sorts methods", () => {
    expect(unitKey(unit("a", ["y", "x"]))).toBe(unitKey(unit("a", ["x", "y"])));
  });
});

describe("whyChainText", () => {
  const hop = (unit: string, line: number | null, target = false): WhyHop => ({
    unit,
    path: target ? null : "a.ts",
    scope: "file",
    symbol: null,
    member: null,
    line,
    reason: target ? "target" : "import",
    depth: target ? 2 : 1,
  });
  const data = (target: string, chain: WhyHop[]): WhyData => ({
    schema_version: 1,
    target,
    reason: "target",
    depth: 2,
    chain,
  });

  test("seed -> hop (line N) -> target", () => {
    expect(
      whyChainText(data("C", [hop("A", null), hop("B", 12), hop("C", 4, true)])),
    ).toBe("A -> B (line 12) -> C (line 4)");
  });

  test("appends the target when it is not the last hop", () => {
    expect(whyChainText(data("C", [hop("A", null), hop("B", 12)]))).toBe(
      "A -> B (line 12) -> C",
    );
  });

  test("empty chain falls back to the target", () => {
    expect(whyChainText(data("C", []))).toBe("C (target)");
  });
});
