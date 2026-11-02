import { describe, expect, test } from "bun:test";
import {
  MAX_TEXT_BYTES,
  MAX_TEXT_LINES,
  capText,
  formatBundle,
  renderBundleMarkdown,
  truncationNote,
  type BundleMarkdownInput,
} from "../src/format";

function bundleWith(count: number, stateToml: string | null = null): BundleMarkdownInput {
  const entries = Array.from({ length: count }, (_, index) => ({
    label: `\`src/F${index}.java\``,
    markdown: [
      "",
      `### \`src/F${index}.java\` (line ${index + 1})`,
      "",
      "```java",
      `public class F${index} {`,
      `    int value = ${index};`,
      "}",
      "```",
      "",
    ].join("\n"),
  }));
  return {
    title: "demo",
    root: "/repo",
    files: count,
    excerpts: count > 0 ? 1 : 0,
    methods: 0,
    depth: 2,
    entries,
    state_toml: stateToml,
  };
}

describe("markdown rebuild", () => {
  const entries = [
    {
      label: "`a/A.java` :: `A`",
      markdown: "\n### `a/A.java` :: `A` (line 1)\n\n```java\nclass A {}\n```\n",
    },
    {
      label: "`b/B.java`",
      markdown: "\n### `b/B.java` (line 1)\n\n```java\nclass B {}\n```\n",
    },
  ];

  test("matches the jq recipe line by line, state appendix included", () => {
    const bundle: BundleMarkdownInput = {
      title: "demo",
      root: "/repo",
      files: 2,
      excerpts: 1,
      methods: 1,
      depth: 2,
      entries,
      state_toml: "version = 3\n[branches.main]\n",
    };
    const expected =
      [
        "# Code bundle: demo",
        "",
        "- root: `/repo`",
        "- files: 2",
        "- excerpts: 1",
        "- methods: 1",
        "- import depth: 2",
        "",
        "## Files",
        "",
        "- `a/A.java` :: `A`",
        "- `b/B.java`",
        "",
        "## Contents",
      ].join("\n") +
      "\n" +
      entries[0]!.markdown +
      entries[1]!.markdown +
      "\n## Bundle state\n\n```toml\nversion = 3\n[branches.main]\n```\n";
    expect(renderBundleMarkdown(bundle)).toBe(expected);
  });

  test("omits excerpts/methods when zero and the state when null", () => {
    const bundle: BundleMarkdownInput = {
      title: "t",
      root: "/r",
      files: 1,
      excerpts: 0,
      methods: 0,
      depth: 0,
      entries: [entries[1]!],
      state_toml: null,
    };
    const text = renderBundleMarkdown(bundle);
    expect(text).not.toContain("- excerpts:");
    expect(text).not.toContain("- methods:");
    expect(text).not.toContain("## Bundle state");
    expect(text.startsWith("# Code bundle: t\n\n- root: `/r`\n- files: 1\n")).toBe(true);
  });
});

describe("output cap", () => {
  test("a small bundle is not touched", () => {
    const bundle = bundleWith(3);
    const { text, truncation } = formatBundle(bundle);
    expect(truncation.truncated).toBe(false);
    expect(text).toBe(renderBundleMarkdown(bundle));
    expect(text.length).toBeLessThan(MAX_TEXT_BYTES);
  });

  test("a big bundle keeps the head and names the omitted units", () => {
    const total = 200;
    const bundle = bundleWith(total);
    const { text, truncation } = formatBundle(bundle, { maxBytes: 2000 });
    expect(truncation.truncated).toBe(true);
    expect(truncation.totalUnits).toBe(total);
    expect(truncation.shownUnits).toBeGreaterThan(0);
    expect(truncation.shownUnits).toBeLessThan(total);
    expect(truncation.omittedUnits).toBe(total - (truncation.shownUnits ?? 0));
    expect(truncation.note).toBe(truncationNote(truncation.shownUnits ?? 0, total));
    expect(text).toContain("## Files");
    expect(text).toContain("## Contents");
    expect(text.endsWith(truncation.note ?? "")).toBe(true);
    expect(text).toContain("[truncated: showing ");
    expect(text).toContain("read the full bundle with /filer show");
    // The state appendix is not worth keeping in a truncated bundle.
    expect(text).not.toContain("## Bundle state");
    expect(Buffer.byteLength(text)).toBeLessThanOrEqual(2000);
  });

  test("the line limit also cuts and says so", () => {
    const total = 100;
    const bundle = bundleWith(total, "version = 3\n");
    const { truncation } = formatBundle(bundle, { maxLines: 40 });
    expect(truncation.truncated).toBe(true);
    expect(truncation.reason).toBe("lines");
    expect(truncation.shownUnits).toBeLessThan(total);
    expect(truncation.maxLines).toBe(40);
  });

  test("capText keeps the head of non-bundle text", () => {
    const text = Array.from({ length: 300 }, (_, index) => `row ${index}`).join("\n");
    const { text: capped, truncation } = capText(text, { maxLines: 10 });
    expect(truncation.truncated).toBe(true);
    expect(truncation.reason).toBe("lines");
    expect(truncation.shownLines).toBe(10);
    expect(truncation.totalLines).toBe(300);
    expect(capped.startsWith("row 0\nrow 1\n")).toBe(true);
    expect(capped).toContain("showing 10 of 300 lines");
  });

  test("capText enforces the byte limit too", () => {
    const text = "x".repeat(10_000);
    const { text: capped, truncation } = capText(text, { maxBytes: 100 });
    expect(truncation.truncated).toBe(true);
    expect(truncation.reason).toBe("bytes");
    expect(capped.startsWith("x".repeat(100))).toBe(true);
  });

  test("capText leaves short text alone", () => {
    const text = "row 1\nrow 2";
    expect(capText(text)).toEqual({
      text,
      truncation: { truncated: false, maxBytes: MAX_TEXT_BYTES, maxLines: MAX_TEXT_LINES },
    });
  });
});
