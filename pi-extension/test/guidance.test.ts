//! Steering for #61: the prompt section and the `read` nudge. Pure functions
//! plus two handlers, so the fakes are a handler map and plain event objects.

import { describe, expect, test } from "bun:test";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
  GUIDANCE_SECTION,
  READ_NUDGE_MARKER,
  TOOL_GUIDANCE,
  createAvailability,
  extensionOf,
  nudgeForRead,
  readNudge,
  registerFilerGuidance,
  sourceExtensionOf,
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

function setup(availability: FilerAvailability = createAvailability()) {
  const { api, handlers } = fakePi();
  registerFilerGuidance(api, { availability });
  const prompt = handlers.get("before_agent_start") as (event: PromptEvent) => unknown;
  const result = handlers.get("tool_result") as (
    event: ReadResultEvent,
  ) => { content: ContentPart[] } | undefined;
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
});

// --- read nudge -----------------------------------------------------------

describe("read nudge", () => {
  test("appends the note to a full read of a source file", () => {
    const { availability, result } = setup();
    availability.usable = true;
    const event = readEvent("src/main.rs");
    const updated = result(event);

    expect(updated).toBeDefined();
    const text = lastText(updated!.content);
    expect(text).toContain("fn main() {}");
    expect(text).toContain(READ_NUDGE_MARKER);
    expect(text).toContain("filer_bundle");
    expect(text).toContain("src/main.rs");
    // The input event is not mutated in place.
    expect(event.content[0]?.text).toBe("fn main() {}");
  });

  test("is absent while the binary is unusable", () => {
    const { result } = setup();
    expect(result(readEvent("src/main.rs"))).toBeUndefined();
  });

  test("skips ranged reads, non-source files, images and failed reads", () => {
    const { availability, result } = setup();
    availability.usable = true;
    expect(result(readEvent("src/main.rs", { input: { path: "src/main.rs", offset: 10 } })))
      .toBeUndefined();
    expect(result(readEvent("src/main.rs", { input: { path: "src/main.rs", limit: 20 } })))
      .toBeUndefined();
    expect(result(readEvent("README.md"))).toBeUndefined();
    expect(result(readEvent("Makefile"))).toBeUndefined();
    expect(result(readEvent("src/main.rs", { isError: true }))).toBeUndefined();
    expect(
      result(
        readEvent("logo.png", {
          content: [{ type: "image", data: "AAAA", mimeType: "image/png" }],
        }),
      ),
    ).toBeUndefined();
    expect(result(readEvent("src/main.rs", { toolName: "grep" }))).toBeUndefined();
  });

  test("does not stack the note when it is already there", () => {
    const { availability, result } = setup();
    availability.usable = true;
    const first = result(readEvent("src/main.rs"))!;
    const second = result(
      readEvent("src/main.rs", { content: first.content }),
    );
    expect(second).toBeUndefined();
    expect(lastText(first.content).match(/\[filer\]/g)).toHaveLength(1);
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
