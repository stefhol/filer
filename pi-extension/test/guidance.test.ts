//! Steering for #61: the prompt section and the `read` nudge. Pure functions
//! plus two handlers, so the fakes are a handler map and plain event objects.

import { describe, expect, test } from "bun:test";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
  GUIDANCE_SECTION,
  READ_NUDGE_MARKER,
  READ_PROBE_TIMEOUT_MS,
  TOOL_GUIDANCE,
  closureFacts,
  createAvailability,
  extensionOf,
  formatSize,
  memoizedProbe,
  nudgeForRead,
  readNudge,
  readSize,
  registerFilerGuidance,
  sourceExtensionOf,
  type ClosureProbe,
  type FilerAvailability,
} from "../src/guidance";

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
) {
  const { api, handlers } = fakePi();
  registerFilerGuidance(api, { availability, ...(probe === undefined ? {} : { probe }) });
  const prompt = handlers.get("before_agent_start") as (event: PromptEvent) => unknown;
  const result = handlers.get("tool_result") as (
    event: ReadResultEvent,
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
