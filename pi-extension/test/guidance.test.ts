//! Steering for #61: the prompt section and the `read` nudge. Pure functions
//! plus two handlers, so the fakes are a handler map and plain event objects.

import { describe, expect, test } from "bun:test";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
  GUIDANCE_SECTION,
  MAX_DECLARATION_NAMES,
  NUDGE_MARKER,
  READ_NUDGE_MARKER,
  READ_PROBE_TIMEOUT_MS,
  SYMBOL_PROBE_TIMEOUT_MS,
  TOOL_GUIDANCE,
  closureFacts,
  createAvailability,
  declarationNames,
  extensionOf,
  formatSize,
  memoizedProbe,
  memoizedSymbolProbe,
  nudgeForRead,
  readNudge,
  readSize,
  registerFilerGuidance,
  shellWords,
  sourceExtensionOf,
  symbolFacts,
  symbolNudge,
  type ClosureProbe,
  type FilerAvailability,
  type SymbolProbe,
} from "../src/guidance";
import type { SearchData, SearchRow } from "../src/operations";

// --- fakes ----------------------------------------------------------------

interface PromptEvent {
  type: "before_agent_start";
  messageType: "user";
  prompt: string;
  systemPrompt: string;
  systemPromptOptions: {
    selectedTools: string[];
    sections: Record<string, string>;
  };
}

interface ContentPart {
  type: "text" | "image";
  text?: string;
  data?: string;
  mimeType?: string;
}

interface ReadResultEvent {
  type: "tool_result";
  toolName: string;
  toolCallId: string;
  input: Record<string, unknown>;
  content: ContentPart[];
  isError: boolean;
  details: unknown;
}

type Handler = (event: unknown, ctx?: unknown) => unknown;

function fakePi(): { api: ExtensionAPI; handlers: Map<string, Handler> } {
  const handlers = new Map<string, Handler>();
  const api = {
    on(event: string, handler: Handler) {
      handlers.set(event, handler);
      return () => {};
    },
  } as unknown as ExtensionAPI;
  return { api, handlers };
}

function promptEvent(selectedTools: string[], sections: Record<string, string> = {}): PromptEvent {
  return {
    type: "before_agent_start",
    messageType: "user",
    prompt: "load src/main.rs and tell me what it does",
    systemPrompt: "",
    systemPromptOptions: { selectedTools, sections },
  };
}

function readEvent(path: string, overrides: Partial<ReadResultEvent> = {}): ReadResultEvent {
  return {
    type: "tool_result",
    toolName: "read",
    toolCallId: "call-1",
    input: { path },
    content: [{ type: "text", text: "fn main() {}" }],
    isError: false,
    details: undefined,
    ...overrides,
  };
}

function setup(
  availability: FilerAvailability = createAvailability(),
  probe?: ClosureProbe,
  symbolProbe?: SymbolProbe,
) {
  const { api, handlers } = fakePi();
  registerFilerGuidance(api, {
    availability,
    ...(probe === undefined ? {} : { probe }),
    ...(symbolProbe === undefined ? {} : { symbolProbe }),
  });
  const prompt = handlers.get("before_agent_start") as (event: PromptEvent) => unknown;
  const result = handlers.get("tool_result") as (
    event: ReadResultEvent,
    ctx?: { cwd: string },
  ) => Promise<{ content: ContentPart[] } | undefined>;
  return { availability, prompt, result };
}

function lastText(parts: ContentPart[]): string {
  return parts[parts.length - 1]?.text ?? "";
}

// --- prompt section -------------------------------------------------------

describe("tool guidance", () => {
  test("is absent until the binary answered --version", () => {
    const { availability, prompt } = setup();
    const event = promptEvent(["read", "filer_bundle"]);
    prompt(event);
    expect(event.systemPromptOptions.sections[GUIDANCE_SECTION]).toBeUndefined();

    availability.usable = true;
    prompt(event);
    expect(event.systemPromptOptions.sections[GUIDANCE_SECTION]).toBe(TOOL_GUIDANCE);
  });

  test("leaves sections added by other extensions alone", () => {
    const { availability, prompt } = setup();
    availability.usable = true;
    const event = promptEvent(["filer_bundle"], { project_rules: "- be nice" });
    prompt(event);
    expect(event.systemPromptOptions.sections["project_rules"]).toBe("- be nice");
    expect(event.systemPromptOptions.sections[GUIDANCE_SECTION]).toBe(TOOL_GUIDANCE);
  });

  test("is silent when the bundle tool is not active", () => {
    const { availability, prompt } = setup();
    availability.usable = true;
    const event = promptEvent(["read"]);
    prompt(event);
    expect(event.systemPromptOptions.sections[GUIDANCE_SECTION]).toBeUndefined();
  });

  test("replaces its own section instead of stacking it", () => {
    const { availability, prompt } = setup();
    availability.usable = true;
    const event = promptEvent(["filer_bundle"]);
    prompt(event);
    prompt(event);
    expect(event.systemPromptOptions.sections[GUIDANCE_SECTION]).toBe(TOOL_GUIDANCE);
    expect(event.systemPromptOptions.sections[GUIDANCE_SECTION]).not.toContain(
      `${TOOL_GUIDANCE}\n`,
    );
  });

  test("mentions the three routing cases and the /filer boundary", () => {
    expect(TOOL_GUIDANCE).toContain("instead of `read`");
    expect(TOOL_GUIDANCE).toContain("import/call closure");
    expect(TOOL_GUIDANCE).toContain("`filer_deps`");
    expect(TOOL_GUIDANCE).toContain("`filer_callers`");
    expect(TOOL_GUIDANCE).toContain("/filer");
  });

  test("points a crate question at the crate, not one file (#68)", () => {
    expect(TOOL_GUIDANCE).toContain("workspace crate");
    expect(TOOL_GUIDANCE).toContain("`crates/hub`");
    expect(TOOL_GUIDANCE).toContain("src/lib.rs");
  });
});

// --- read nudge -----------------------------------------------------------

describe("read nudge", () => {
  test("appends the note to a full read of a source file", async () => {
    const { availability, result } = setup();
    availability.usable = true;
    const event = readEvent("src/main.rs");
    const updated = await result(event);

    expect(updated).toBeDefined();
    const text = lastText(updated!.content);
    expect(text).toContain("fn main() {}");
    expect(text).toContain(READ_NUDGE_MARKER);
    expect(text).toContain("filer_bundle");
    expect(text).toContain("src/main.rs");
    // The input event is not mutated in place.
    expect(event.content[0]?.text).toBe("fn main() {}");
  });

  test("is absent while the binary is unusable", async () => {
    const { result } = setup();
    expect(await result(readEvent("src/main.rs"))).toBeUndefined();
  });

  test("skips ranged reads, non-source files, images and failed reads", async () => {
    const { availability, result } = setup();
    availability.usable = true;
    expect(
      await result(readEvent("src/main.rs", { input: { path: "src/main.rs", offset: 10 } })),
    ).toBeUndefined();
    expect(
      await result(readEvent("src/main.rs", { input: { path: "src/main.rs", limit: 20 } })),
    ).toBeUndefined();
    expect(await result(readEvent("README.md"))).toBeUndefined();
    expect(await result(readEvent("Makefile"))).toBeUndefined();
    expect(await result(readEvent("src/main.rs", { isError: true }))).toBeUndefined();
    expect(
      await result(
        readEvent("logo.png", {
          content: [{ type: "image", data: "AAAA", mimeType: "image/png" }],
        }),
      ),
    ).toBeUndefined();
    expect(await result(readEvent("src/main.rs", { toolName: "grep" }))).toBeUndefined();
  });

  test("does not stack the note when it is already there", async () => {
    const { availability, result } = setup();
    availability.usable = true;
    const first = (await result(readEvent("src/main.rs")))!;
    const second = await result(readEvent("src/main.rs", { content: first.content }));
    expect(second).toBeUndefined();
    expect(lastText(first.content).match(/\[filer\]/g)).toHaveLength(1);
  });
});

// --- numbers in the nudge (#68) -------------------------------------------

function sizedReadEvent(path: string, lines: number): ReadResultEvent {
  const text = Array.from({ length: lines }, (_, index) => `let line_${index} = ${index};`).join(
    "\n",
  );
  return readEvent(path, { content: [{ type: "text", text }] });
}

describe("read nudge with numbers", () => {
  test("reports the read's own size", async () => {
    const { availability, result } = setup();
    availability.usable = true;
    const updated = await result(sizedReadEvent("src/main.rs", 12));
    const text = lastText(updated!.content);
    expect(text).toContain("12 lines / ");
    expect(text).toContain(READ_NUDGE_MARKER);
    expect(text).toContain("filer_bundle");
  });

  test("reports the closure from one probe, and its unresolved imports", async () => {
    const probe: ClosureProbe = async () => ({ files: 12, unresolved: 69, depth: 2 });
    const { availability, result } = setup(createAvailability(), probe);
    availability.usable = true;
    const text = lastText((await result(sizedReadEvent("crates/hub/src/lib.rs", 725)))!.content);
    expect(text).toContain("725 lines");
    expect(text).toContain("Its closure at depth 2 is 12 files (+69 unresolved imports)");
  });

  test("a failed probe drops the closure number instead of lying", async () => {
    const probe: ClosureProbe = async () => null;
    const { availability, result } = setup(createAvailability(), probe);
    availability.usable = true;
    const text = lastText((await result(sizedReadEvent("src/main.rs", 5)))!.content);
    expect(text).toContain("5 lines");
    expect(text).not.toContain("Its closure at depth");
    expect(text).toContain("import/call closure");
    expect(text).toContain("filer_bundle");
  });

  test("the probe is memoized per path, failures included", async () => {
    let calls = 0;
    const probe: ClosureProbe = async (path) => {
      calls += 1;
      return path.endsWith("main.rs") ? null : { files: 3, unresolved: 0, depth: 2 };
    };
    const { availability, result } = setup(createAvailability(), probe);
    availability.usable = true;
    await result(sizedReadEvent("src/main.rs", 3));
    await result(sizedReadEvent("src/main.rs", 3));
    expect(calls).toBe(1);
    await result(sizedReadEvent("src/lib.rs", 3));
    expect(calls).toBe(2);
  });

  test("a clean closure says so by omission", async () => {
    const text = readNudge("a.rs", { lines: 3, bytes: 300 }, { files: 2, unresolved: 0, depth: 1 });
    expect(text).toContain("Its closure at depth 2 is 2 files;");
    expect(text).not.toContain("unresolved");
  });

  test("readSize sums the text blocks and formatSize stays honest", () => {
    expect(readSize([{ type: "text", text: "a\nb\n" }])).toEqual({ lines: 2, bytes: 4 });
    expect(readSize([{ type: "image", data: "AAAA", mimeType: "image/png" }])).toEqual({
      lines: 0,
      bytes: 0,
    });
    expect(readSize([{ type: "text", text: "" }])).toEqual({ lines: 0, bytes: 0 });
    expect(formatSize(24 * 1024)).toBe("24 KB");
    expect(formatSize(4300)).toBe("4.2 KB");
  });

  test("closureFacts counts every distinct file, the seed row included", () => {
    const facts = closureFacts({
      schema_version: 1,
      seed: "a/A.java",
      granularity: "file",
      deps: [
        { path: "a/A.java", fqn: "app.A", depth: 0, reason: "seed" },
        { path: "a/B.java", fqn: "app.B", depth: 1, reason: "import" },
        { path: "a/B.java", fqn: "app.B", depth: 1, reason: "call" },
        { path: "a/C.java", fqn: "app.C", depth: 2, reason: "import" },
      ],
      edges: [],
      diagnostics: [
        { kind: "unresolved-import", path: "a/A.java", import: "protocol::Message" },
        { kind: "unknown-receiver", path: "a/A.java", call: "go" },
      ],
    });
    expect(facts).toEqual({ files: 3, unresolved: 1, depth: 2 });
  });

  test("a memoized probe never waits longer than the budget", async () => {
    const probe = memoizedProbe(async () => {
      await new Promise((resolve) => setTimeout(resolve, 5_000));
      return { files: 1, unresolved: 0, depth: 0 };
    });
    const started = Date.now();
    // A probe that overruns resolves as unknown, it does not reject.
    const pending = probe("a.rs", { cwd: "/repo" });
    expect(READ_PROBE_TIMEOUT_MS).toBeLessThan(5_000);
    expect(await Promise.race([pending, new Promise((resolve) => setTimeout(() => resolve("late"), 10))]))
      .toBe("late");
    expect(Date.now() - started).toBeLessThan(1_000);
  });
});

// --- path helpers ---------------------------------------------------------

// --- symbol nudge ---------------------------------------------------------

function searchRow(overrides: Partial<SearchRow> = {}): SearchRow {
  return {
    schema_version: 1,
    path: "src/state.rs",
    scope: "function",
    symbol: "to_toml",
    member: null,
    kind: "function",
    fqn: "src/state.rs::to_toml",
    line: 12,
    start_line: 12,
    end_line: 40,
    bytes: 640,
    est_tokens: 160,
    package: null,
    included: false,
    depth: null,
    reason: null,
    ...overrides,
  };
}

function searchData(rows: SearchRow[]): SearchData {
  return { schema_version: 1, query: "a", results: rows.length, rows };
}

function bashEvent(
  command: string,
  overrides: Partial<ReadResultEvent> = {},
): ReadResultEvent {
  return {
    type: "tool_result",
    toolName: "bash",
    toolCallId: "call-2",
    input: { command },
    content: [{ type: "text", text: "src/state.rs:12:fn to_toml() {}" }],
    isError: false,
    details: undefined,
    ...overrides,
  };
}

describe("declaration searches", () => {
  test("reads the pattern the way grep and rg do", () => {
    expect(shellWords(`grep -n "a b" 'c d'`)).toEqual(["grep", "-n", "a b", "c d"]);
    expect(shellWords(`rg -n 'fn to_toml'`)).toEqual(["rg", "-n", "fn to_toml"]);
  });

  test("extracts the declared name", () => {
    expect(declarationNames(`rg -n "fn to_toml" -A6 src/state.rs`)).toEqual([
      "to_toml",
    ]);
    expect(
      declarationNames(`grep -nE '^interface Subscribable' /tmp/schema.graphql`),
    ).toEqual(["Subscribable"]);
    expect(declarationNames(`grep -rn "class HazelcastProperties" .`)).toEqual([
      "HazelcastProperties",
    ]);
    expect(declarationNames(`grep -e "fn nested" src/a.rs`)).toEqual(["nested"]);
    expect(declarationNames(`rg --glob '*.rs' "struct Index" src`)).toEqual([
      "Index",
    ]);
  });

  test("stays silent on text, log and call searches", () => {
    expect(declarationNames(`grep -iE "error|panic" /var/log/x`)).toEqual([]);
    expect(declarationNames(`rg -n '\.walk\(' src`)).toEqual([]);
    expect(declarationNames(`rg -n "snapshot" crates`)).toEqual([]);
    expect(declarationNames(`grep -f patterns.txt file`)).toEqual([]);
    expect(declarationNames(`echo grep -n "fn not_a_search"`)).toEqual([]);
    expect(declarationNames("cat README.md")).toEqual([]);
  });

  test("follows separators and caps the names", () => {
    expect(declarationNames(`cd src && rg 'fn a' ; grep 'def b'`)).toEqual([
      "a",
      "b",
    ]);
    const many = declarationNames(`rg 'fn a|fn b|fn c|fn d' src`);
    expect(many).toHaveLength(MAX_DECLARATION_NAMES);
    expect(many).toEqual(["a", "b", "c"]);
  });
});

describe("symbol facts", () => {
  test("keeps declarations, drops file rows and duplicates", () => {
    const facts = symbolFacts(
      searchData([
        searchRow({ scope: "file", symbol: null, fqn: "src/state.rs", line: null }),
        searchRow(),
        searchRow({ fqn: "src/state.rs::to_toml" }),
        searchRow({
          fqn: "src/state.rs::State.to_toml",
          kind: "method",
          member: "to_toml",
          symbol: "State",
          line: 90,
        }),
      ]),
    );
    expect(facts).toHaveLength(2);
    expect(facts[0]).toEqual({
      unit: "src/state.rs::to_toml",
      kind: "function",
      path: "src/state.rs",
      line: 12,
      tokens: 160,
    });
  });

  test("caps the facts at three", () => {
    const facts = symbolFacts(
      searchData([
        searchRow({ path: "a", symbol: "one" }),
        searchRow({ path: "a", symbol: "two" }),
        searchRow({ path: "a", symbol: "three" }),
        searchRow({ path: "a", symbol: "four" }),
      ]),
    );
    expect(facts.map((fact) => fact.unit)).toEqual(["a::one", "a::two", "a::three"]);
  });

  test("the note carries the units, the size and the next tool", () => {
    const text = symbolNudge("to_toml", [
      {
        unit: "src/state.rs::to_toml",
      kind: "function",
        path: "src/state.rs",
        line: 12,
        tokens: 160,
      },
    ]);
    expect(text).toContain(NUDGE_MARKER);
    expect(text).toContain("src/state.rs::to_toml (function, src/state.rs:12, 160 tokens)");
    expect(text).toContain('`filer_search({ query: "to_toml" })`');
    expect(text).toContain("`filer_callers({ unit })`");
    expect(text).toContain("`filer_refs({ name })`");
  });
});

describe("symbol nudge", () => {
  const facts = [
    {
      unit: "src/state.rs::to_toml",
      kind: "function",
      path: "src/state.rs",
      line: 12,
      tokens: 160,
    },
  ];

  test("appends the units to a declaration grep", async () => {
    const probe: SymbolProbe = async () => facts;
    const { availability, result } = setup(createAvailability(), undefined, probe);
    availability.usable = true;
    const event = bashEvent(`rg -n "fn to_toml" -A6 src/state.rs`);
    const updated = await result(event, { cwd: "/repo" });

    expect(updated).toBeDefined();
    expect(updated!.content[0]?.text).toContain("fn to_toml() {}");
    expect(updated!.content[0]?.text).toContain("src/state.rs::to_toml");
    // The input event is not mutated in place.
    expect(event.content[0]?.text).toBe("src/state.rs:12:fn to_toml() {}");
  });

  test("probes once per name, memoized across calls", async () => {
    const seen: string[] = [];
    const probe: SymbolProbe = async (name) => {
      seen.push(name);
      return facts;
    };
    const { availability, result } = setup(createAvailability(), undefined, probe);
    availability.usable = true;
    await result(bashEvent(`rg "fn to_toml" src`), { cwd: "/repo" });
    await result(bashEvent(`grep "fn to_toml" src/a.rs`), { cwd: "/repo" });
    expect(seen).toEqual(["to_toml"]);
  });

  test("stays silent while unusable, on errors, and without a probe", async () => {
    let calls = 0;
    const probe: SymbolProbe = async () => {
      calls += 1;
      return facts;
    };
    const off = setup(createAvailability(), undefined, probe);
    expect(await off.result(bashEvent(`rg "fn to_toml" src`))).toBeUndefined();

    const { availability, result } = setup(createAvailability(), undefined, probe);
    availability.usable = true;
    expect(
      await result(bashEvent(`rg "fn to_toml" src`, { isError: true }), { cwd: "/repo" }),
    ).toBeUndefined();
    expect(calls).toBe(0);

    const bare = setup();
    bare.availability.usable = true;
    expect(await bare.result(bashEvent(`rg "fn to_toml" src`), { cwd: "/repo" })).toBeUndefined();
  });

  test("says nothing when the search finds nothing", async () => {
    const empty: SymbolProbe = async () => [];
    const unknown: SymbolProbe = async () => null;
    const emptySetup = setup(createAvailability(), undefined, empty);
    emptySetup.availability.usable = true;
    expect(await emptySetup.result(bashEvent(`rg "fn ghost" src`), { cwd: "/repo" })).toBeUndefined();

    const unknownSetup = setup(createAvailability(), undefined, unknown);
    unknownSetup.availability.usable = true;
    expect(
      await unknownSetup.result(bashEvent(`rg "fn ghost" src`), { cwd: "/repo" }),
    ).toBeUndefined();
  });

  test("does not annotate a log grep or an annotated result", async () => {
    let calls = 0;
    const probe: SymbolProbe = async () => {
      calls += 1;
      return facts;
    };
    const { availability, result } = setup(createAvailability(), undefined, probe);
    availability.usable = true;
    expect(await result(bashEvent(`grep -iE "error" /var/log/x`), { cwd: "/repo" })).toBeUndefined();
    expect(calls).toBe(0);
    const already = bashEvent(`rg "fn to_toml" src`, {
      content: [{ type: "text", text: `${NUDGE_MARKER} already there` }],
    });
    expect(await result(already, { cwd: "/repo" })).toBeUndefined();
    expect(calls).toBe(0);
  });

  test("a memoized probe never waits longer than its budget", async () => {
    const slow: SymbolProbe = () => new Promise(() => {});
    const probe = memoizedSymbolProbe(slow);
    const started = Date.now();
    expect(await probe("to_toml", { cwd: "/repo" })).toBeNull();
    expect(Date.now() - started).toBeLessThan(SYMBOL_PROBE_TIMEOUT_MS + 1_000);
  });
});

describe("extension matching", () => {
  test("reads the extension of a path", () => {
    expect(extensionOf("src/Main.java")).toBe("java");
    expect(extensionOf("src/Main.RS")).toBe("rs");
    expect(extensionOf("/a.b/c.d/Foo.d.ts")).toBe("ts");
    expect(extensionOf("C:\\repo\\App.java")).toBe("java");
    expect(extensionOf("Makefile")).toBeNull();
    expect(extensionOf(".bashrc")).toBeNull();
    expect(extensionOf("dir.with.dot/file")).toBeNull();
  });

  test("only lists what src/lang.rs parses", () => {
    for (const path of [
      "A.java",
      "A.kt",
      "A.kts",
      "A.scala",
      "A.sc",
      "A.sbt",
      "A.groovy",
      "build.gradle",
      "A.cs",
      "A.ts",
      "A.tsx",
      "A.js",
      "A.jsx",
      "A.mjs",
      "A.cjs",
      "A.vue",
      "A.fe",
      "A.rs",
      "A.py",
      "A.pyi",
    ]) {
      expect(sourceExtensionOf(path)).not.toBeNull();
    }
    for (const path of ["A.md", "A.toml", "A.json", "A.png", "Dockerfile", "A.go"]) {
      expect(sourceExtensionOf(path)).toBeNull();
    }
  });

  test("the nudge names the seed form the CLI accepts", () => {
    expect(readNudge("src/a b.ts")).toContain('"src/a b.ts"');
    expect(nudgeForRead({ path: "src/a.ts" })).toBe(readNudge("src/a.ts"));
    expect(nudgeForRead({})).toBeNull();
    expect(nudgeForRead({ path: "src/a.ts", offset: 1 })).toBeNull();
  });
});
