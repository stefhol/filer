import { describe, expect, test } from "bun:test";
import type { FilerClient, RunResult } from "../src/cli";
import {
  bundle,
  callers,
  deps,
  index,
  refs,
  search,
  show,
  state,
  why,
  type BundleEnvelope,
} from "../src/operations";
import { FilerToolError, ERROR_HINT_SEARCH } from "../src/errors";
import { renderBundleMarkdown } from "../src/format";

const root = "/repo";

function envelopeBundle(overrides: Partial<BundleEnvelope> = {}): BundleEnvelope {
  return {
    schema_version: 1,
    root,
    title: "demo",
    branch: "main",
    depth: 2,
    files: 1,
    excerpts: 1,
    methods: 0,
    budget: {
      max_file_bytes: 1048576,
      max_bytes: null,
      max_tokens: null,
      used_bytes: 120,
      used_tokens: 30,
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
        start_line: 3,
        end_line: 6,
        start_byte: 10,
        end_byte: 130,
        tokens: 30,
        hash: "abc",
        rendered: true,
        label: "`a/A.java` :: `A` [use]",
        markdown: "\n### `a/A.java` :: `A` (line 3)\n\n```java\nclass A {}\n```\n",
      },
    ],
    omitted: [],
    diagnostics: [
      { kind: "unresolved-import", path: "a/A.java", import: "com.acme.Tax" },
    ],
    coverage: { files_indexed: 2, files_skipped: 0, imports_unresolved: 1 },
    state_toml: null,
    ...overrides,
  };
}

interface Fake {
  client: FilerClient;
  calls: string[][];
}

function fake(
  handler: (argv: string[]) => Partial<RunResult> | Promise<Partial<RunResult>>,
  fakeRoot = root,
): Fake {
  const calls: string[][] = [];
  const client: FilerClient = {
    root: fakeRoot,
    async run(argv) {
      calls.push(argv);
      const result = await handler(argv);
      return {
        stdout: "",
        stderr: "",
        code: 0,
        signal: null,
        timedOut: false,
        aborted: false,
        bin: "filer",
        ...result,
      };
    },
  };
  return { client, calls };
}

async function capture(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
    throw new Error("expected the operation to reject");
  } catch (error) {
    return error;
  }
}

describe("bundle", () => {
  test("maps the envelope and rebuilds the markdown", async () => {
    const payload = envelopeBundle();
    const { client, calls } = fake(() => ({
      stdout: JSON.stringify(payload),
      stderr: `${JSON.stringify({ warning: "dropping a stale unit" })}\n`,
    }));

    const result = await bundle({ client }, { seeds: ["a/A.java::A.use"], depth: 1 });

    expect(result.data.entries).toHaveLength(1);
    expect(result.data.coverage).toEqual(payload.coverage);
    expect(result.data.diagnostics).toEqual(payload.diagnostics);
    expect(result.text).toBe(renderBundleMarkdown(payload));
    expect(result.warnings).toEqual(["dropping a stale unit"]);
    expect(result.truncation).toBeUndefined();

    const argv = calls[0]!;
    expect(argv.slice(0, 4)).toEqual(["--root", root, "--json", "--quiet"]);
    expect(argv).toContain("bundle");
    expect(argv.indexOf("--")).toBeLessThan(argv.indexOf("a/A.java::A.use"));
  });

  test("exit 4 is a successful empty bundle with the CLI hint", async () => {
    const { client } = fake(() => ({
      code: 4,
      stderr: JSON.stringify({
        error: {
          code: "empty_result",
          message: "nothing to bundle",
          hint: "pass a file that exists",
        },
      }),
    }));
    const result = await bundle({ client }, { seeds: ["a/Missing.java"] });
    expect(result.data.entries).toEqual([]);
    expect(result.data.files).toBe(0);
    expect(result.hint).toBe("pass a file that exists");
    expect(result.text).toContain("pass a file that exists");
  });

  test("needs at least one seed", async () => {
    const { client } = fake(() => ({}));
    const error = await capture(bundle({ client }, { seeds: [] }));
    expect(error).toBeInstanceOf(FilerToolError);
    expect((error as FilerToolError).code).toBe("invalid_arguments");
  });

  test("a schema mismatch fails loudly", async () => {
    const { client } = fake(() => ({
      stdout: JSON.stringify({ ...envelopeBundle(), schema_version: 2 }),
    }));
    const error = await capture(bundle({ client }, { seeds: ["a/A.java"] }));
    expect(error).toBeInstanceOf(FilerToolError);
    expect((error as FilerToolError).message).toBe(
      "extension expects schema_version 1, binary reports 2 — update one of them",
    );
  });

  test("a truncated bundle carries the cap note in text", async () => {
    const entries = Array.from({ length: 120 }, (_, index) => ({
      path: `a/F${index}.java`,
      scope: "file",
      symbol: null,
      methods: [],
      depth: 1,
      reason: "import",
      start_line: 1,
      end_line: 10,
      start_byte: 0,
      end_byte: 400,
      tokens: 100,
      rendered: true,
      label: `\`a/F${index}.java\``,
      markdown: `\n### \`a/F${index}.java\` (line 1)\n\n\`\`\`java\nclass F${index} {\n${"x".repeat(400)}\n}\n\`\`\`\n`,
    }));
    const payload = envelopeBundle({ files: 120, excerpts: 0, entries });
    const { client } = fake(() => ({ stdout: JSON.stringify(payload) }));
    const result = await bundle({ client }, { seeds: ["a/F0.java"] });
    expect(result.truncation?.truncated).toBe(true);
    expect(result.truncation?.shownUnits).toBeLessThan(120);
    expect(result.text).toContain("read the full bundle with /filer show");
  });
});

describe("show", () => {
  test("passes --branch and rebuilds the saved bundle", async () => {
    const payload = envelopeBundle();
    const { client, calls } = fake(() => ({ stdout: JSON.stringify(payload) }));
    const result = await show({ client }, { branch: "main" });
    expect(result.text).toBe(renderBundleMarkdown(payload));
    expect(calls[0]).toContain("show");
    expect(calls[0]![calls[0]!.indexOf("--branch") + 1]).toBe("main");
  });
});

describe("search", () => {
  const rows = [
    {
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
    },
    {
      schema_version: 1,
      path: "a/B.java",
      scope: "file",
      symbol: null,
      member: null,
      kind: "file",
      fqn: "a/B.java",
      line: null,
      start_line: 1,
      end_line: 9,
      bytes: 200,
      est_tokens: 50,
      package: "app",
      included: false,
      depth: null,
      reason: null,
    },
  ];

  test("maps JSON lines to rows and text", async () => {
    const { client, calls } = fake(() => ({
      stdout: rows.map((row) => JSON.stringify(row)).join("\n"),
    }));
    const result = await search({ client }, { query: "A", limit: 10 });
    expect(result.data.results).toBe(2);
    expect(result.data.rows).toHaveLength(2);
    expect(result.text).toContain("a/A.java::A.use (method line 3)");
    expect(result.text).toContain("a/B.java (file)");
    expect(calls[0]!.slice(0, 6)).toEqual([
      "--root",
      root,
      "--json",
      "--quiet",
      "search",
      "--query",
    ]);
  });

  test("exit 4 carries the empty-result hint", async () => {
    const { client } = fake(() => ({
      code: 4,
      stdout: JSON.stringify({
        schema_version: 1,
        hint: ERROR_HINT_SEARCH,
        results: 0,
      }),
    }));
    const result = await search({ client }, { query: "discount rounding" });
    expect(result.data.results).toBe(0);
    expect(result.data.hint).toBe(ERROR_HINT_SEARCH);
    expect(result.text).toBe(ERROR_HINT_SEARCH);
  });

  test("every row must carry schema_version 1", async () => {
    const { client } = fake(() => ({
      stdout: [rows[0], { ...rows[1], schema_version: 2 }]
        .map((row) => JSON.stringify(row))
        .join("\n"),
    }));
    const error = await capture(search({ client }, { query: "A" }));
    expect((error as FilerToolError).message).toBe(
      "extension expects schema_version 1, binary reports 2 — update one of them",
    );
  });
});

describe("deps / callers / why / refs", () => {
  test("deps keeps rows, edges and diagnostics", async () => {
    const payload = {
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
      edges: [
        { from: "a/A.java::A.use", to: "a/B.java::B.size", reason: "call", line: 5 },
      ],
      diagnostics: [],
    };
    const { client } = fake(() => ({ stdout: JSON.stringify(payload) }));
    const result = await deps({ client }, { seed: "a/A.java::A.use", unit: true, depth: 2 });
    expect(result.data.seed).toBe("a/A.java::A.use");
    expect(result.data.granularity).toBe("unit");
    expect(result.data.edges).toEqual(payload.edges);
    expect(result.text).toBe("a/B.java::B.size (depth 1, call)");
  });

  test("deps exit 4 is an empty closure", async () => {
    const { client } = fake(() => ({ code: 4, stderr: '{"error":{"code":"empty_result"}}' }));
    const result = await deps({ client }, { seed: "a/A.java" });
    expect(result.data.deps).toEqual([]);
    expect(result.data.edges).toEqual([]);
  });

  test("callers maps the bare array", async () => {
    const payload = [
      { caller: "a/A.java::A.use", line: 5, depth: 1 },
      { caller: "a/C.java::C.handle", line: 7, depth: 2 },
    ];
    const { client } = fake(() => ({ stdout: JSON.stringify(payload) }));
    const result = await callers({ client }, { unit: "a/B.java::B.size", depth: 2 });
    expect(result.data.rows).toEqual(payload);
    expect(result.text).toBe(
      "a/A.java::A.use (line 5, depth 1)\na/C.java::C.handle (line 7, depth 2)",
    );
  });

  test("callers exit 4 with [] is empty success", async () => {
    const { client } = fake(() => ({ code: 4, stdout: "[]" }));
    const result = await callers({ client }, { unit: "a/B.java::B.size" });
    expect(result.data.rows).toEqual([]);
    expect(result.text).toBe("no callers");
  });

  test("why renders the chain back to the seed", async () => {
    const payload = {
      target: "a/B.java::B.size",
      reason: "call",
      depth: 2,
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
          depth: 2,
        },
      ],
    };
    const { client } = fake(() => ({ stdout: JSON.stringify(payload) }));
    const result = await why({ client }, { unit: "a/B.java::B.size" });
    expect(result.data.target).toBe("a/B.java::B.size");
    expect(result.data.chain).toHaveLength(2);
    expect(result.text).toContain("a/B.java::B.size (call at depth 2)");
    expect(result.text).toContain("seed a/A.java::A.use");
    expect(result.text).toContain("-> a/B.java::B.size (call line 5)");
  });

  test("refs maps use and call rows", async () => {
    const payload = [
      { path: "a/A.java", symbol: "A", member: null, line: 2, kind: "use" },
      { path: "a/C.java", symbol: "C", member: "run", line: 9, kind: "call" },
    ];
    const { client } = fake(() => ({ stdout: JSON.stringify(payload) }));
    const result = await refs({ client }, { name: "TaxRate" });
    expect(result.data.rows).toEqual(payload);
    expect(result.text).toBe(
      "a/A.java::A (use, line 2)\na/C.java::C.run (call, line 9)",
    );
  });
});

describe("state / index (#39 envelopes)", () => {
  test("state maps the documented envelope", async () => {
    const payload = {
      schema_version: 1,
      root,
      state_path: "/repo/.filer/bundle.toml",
      branch: "main",
      label: "main",
      exists: true,
      current: {
        branch: "main",
        created: 1793618434,
        depth: 2,
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
      branches: [
        { branch: "main", label: "main", entries: 3, created: 1793618434, depth: 2 },
      ],
    };
    const { client } = fake(() => ({ stdout: JSON.stringify(payload) }));
    const result = await state({ client });
    expect(result.data.exists).toBe(true);
    expect(result.data.branches).toHaveLength(1);
    expect(result.data.current?.entries[0]?.reason).toBe("seed");
    expect(result.text).toContain("branch: main");
    expect(result.text).toContain("a/A.java::A.use (methods, depth 0, seed)");
  });

  test("state without a file is still a success", async () => {
    const payload = {
      schema_version: 1,
      root,
      state_path: "/repo/.filer/bundle.toml",
      branch: "main",
      label: "main",
      exists: false,
      current: null,
      branches: [],
    };
    const { client } = fake(() => ({ stdout: JSON.stringify(payload) }));
    const result = await state({ client });
    expect(result.data.exists).toBe(false);
    expect(result.text).toContain("(no saved bundle)");
  });

  test("index reports coverage", async () => {
    const payload = {
      schema_version: 1,
      root,
      source_files: 3,
      languages: [{ language: "Java", files: 3 }],
      packages: 1,
      types: 1,
      symbols: 2,
      cache: { reused: 2, parsed: 1 },
      state: null,
      coverage: { files_indexed: 3, files_skipped: 1, imports_unresolved: 2 },
    };
    const { client } = fake(() => ({ stdout: JSON.stringify(payload) }));
    const result = await index({ client });
    expect(result.data.coverage).toEqual(payload.coverage);
    expect(result.text).toContain(
      "coverage: 3 indexed, 1 skipped, 2 unresolved imports",
    );
    expect(result.text).toContain("languages: Java 3");
  });

  test("state prose (pre-#39 binaries) fails the JSON guard", async () => {
    const { client } = fake(() => ({
      stdout: "root: /repo\nsource files: 3",
    }));
    const error = await capture(state({ client }));
    expect((error as FilerToolError).code).toBe("invalid_json");
  });
});
