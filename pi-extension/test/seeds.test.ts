//! Directory seeds (#68): a crate directory becomes its source files, and the
//! expensive directories are refused before a bundle is ever built.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { FilerToolError } from "../src/errors";
import {
  MAX_DIRECTORY_SEEDS,
  createSeedFilesystem,
  directoryEntrySeed,
  expandDepsSeed,
  expandDirectorySeed,
  type SeedFilesystem,
} from "../src/seeds";

const created: string[] = [];

async function project(files: string[]): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "filer-seeds-"));
  created.push(root);
  for (const file of files) {
    const path = join(root, file);
    await mkdir(join(path, ".."), { recursive: true });
    await writeFile(path, "fn main() {}\n");
  }
  return root;
}

afterEach(async () => {
  while (created.length > 0) await rm(created.pop() as string, { recursive: true, force: true });
});

function fake(
  listed: string[] | null,
  home = "/home/me",
): SeedFilesystem {
  return {
    home: () => home,
    async listSourceFiles(dir) {
      if (listed === null) return null;
      return listed.map((file) => `${dir}/${file}`);
    },
  };
}

async function refusal(promise: Promise<unknown>): Promise<FilerToolError> {
  try {
    await promise;
    throw new Error("expected the seed expansion to be refused");
  } catch (error) {
    expect(error).toBeInstanceOf(FilerToolError);
    return error as FilerToolError;
  }
}

describe("directory expansion", () => {
  test("lists the crate's source files, entry file first", async () => {
    const root = await project([
      "crates/hub/src/lib.rs",
      "crates/hub/src/client.rs",
      "crates/hub/src/supervisor.rs",
      "crates/hub/src/snapshot.rs",
      "crates/hub/Cargo.toml",
      "crates/hub/README.md",
    ]);
    const expanded = await expandDirectorySeed(root, "crates/hub");
    expect(expanded).not.toBeNull();
    expect(expanded!.directory).toBe("crates/hub");
    expect(expanded!.entry).toBe("crates/hub/src/lib.rs");
    expect(expanded!.seeds).toEqual([
      "crates/hub/src/lib.rs",
      "crates/hub/src/client.rs",
      "crates/hub/src/snapshot.rs",
      "crates/hub/src/supervisor.rs",
    ]);
    expect(directoryEntrySeed(expanded!)).toBe("crates/hub/src/lib.rs");
  });

  test("a main-only crate uses src/main.rs", async () => {
    const root = await project(["cmd/src/main.rs", "cmd/src/args.rs"]);
    const expanded = await expandDirectorySeed(root, "cmd");
    expect(expanded!.entry).toBe("cmd/src/main.rs");
    expect(expanded!.seeds[0]).toBe("cmd/src/main.rs");
  });

  test("nested directories are walked and other languages count too", async () => {
    const root = await project([
      "app/src/main/kotlin/App.kt",
      "app/src/main/kotlin/util/Log.kt",
      "app/build.gradle",
      "app/src/test/kotlin/AppTest.kt",
    ]);
    const expanded = await expandDirectorySeed(root, "app");
    // `build.gradle` is a source file filer parses, so it is a seed too.
    expect(expanded!.seeds).toEqual([
      "app/build.gradle",
      "app/src/main/kotlin/App.kt",
      "app/src/main/kotlin/util/Log.kt",
      "app/src/test/kotlin/AppTest.kt",
    ]);
  });

  test("an ordinary file or unit seed is left alone", async () => {
    const root = await project(["crates/hub/src/lib.rs"]);
    expect(await expandDirectorySeed(root, "crates/hub/src/lib.rs")).toBeNull();
    expect(await expandDirectorySeed(root, "crates/hub/src/lib.rs::run")).toBeNull();
    expect(await expandDirectorySeed(root, "crates/hub/src/missing.rs")).toBeNull();
  });

  test("a directory with no source files is refused", async () => {
    const root = await project(["docs/README.md"]);
    const error = await refusal(expandDirectorySeed(root, "docs"));
    expect(error.code).toBe("invalid_arguments");
    expect(error.message).toContain("no source files below docs");
  });

  test("the project root, $HOME and outside paths are refused with a hint", async () => {
    const root = await project(["crates/hub/src/lib.rs"]);
    const filesystem = createSeedFilesystem();
    const atRoot = await refusal(expandDirectorySeed(root, ".", filesystem));
    expect(atRoot.message).toContain("project root");
    expect(atRoot.hint).toContain("narrower path");
    const atHome = await refusal(
      expandDirectorySeed(root, ".", { ...filesystem, home: () => root }),
    );
    expect(atHome.message).toContain("home directory");
    const outside = await refusal(expandDirectorySeed(root, "..", filesystem));
    expect(outside.message).toContain("outside");
    expect(outside.hint).toContain("inside the project root");
  });

  test("more than the cap is refused, naming the limit", async () => {
    const files = Array.from({ length: MAX_DIRECTORY_SEEDS + 1 }, (_, index) => `f/${index}.rs`);
    const error = await refusal(expandDirectorySeed("/repo", "f", fake(files)));
    expect(error.code).toBe("invalid_arguments");
    expect(error.message).toContain(String(MAX_DIRECTORY_SEEDS));
    expect(error.hint).toContain("narrower");
  });

  test("exactly the cap is allowed", async () => {
    const files = Array.from({ length: MAX_DIRECTORY_SEEDS }, (_, index) => `f/${index}.rs`);
    const expanded = await expandDirectorySeed("/repo", "f", fake(files));
    expect(expanded!.seeds).toHaveLength(MAX_DIRECTORY_SEEDS);
  });

  test("symlinks are not followed out of the seed", async () => {
    const root = await project(["crates/hub/src/lib.rs"]);
    const outside = await project(["secret/src/lib.rs"]);
    // A symlinked directory and a symlinked file: both are skipped.
    const { symlink } = await import("node:fs/promises");
    await symlink(join(outside, "secret"), join(root, "crates/hub/escape"));
    await symlink(
      join(outside, "secret/src/lib.rs"),
      join(root, "crates/hub/src/linked.rs"),
    );
    const expanded = await expandDirectorySeed(root, "crates/hub");
    expect(expanded!.seeds).toEqual(["crates/hub/src/lib.rs"]);
  });
});

describe("directory seed for filer_deps", () => {
  test("answers through the crate entry file, and says which file it used", async () => {
    const expanded = await expandDepsSeed("/repo", "crates/hub", fake([
      "src/lib.rs",
      "src/client.rs",
      "src/supervisor.rs",
    ]));
    expect(expanded.seed).toBe("crates/hub/src/lib.rs");
    expect(expanded.note).toBe("crates/hub -> crates/hub/src/lib.rs (3 source files)");
  });

  test("a file seed is reported unchanged", async () => {
    const root = await project(["crates/hub/src/lib.rs"]);
    const expanded = await expandDepsSeed(root, "crates/hub/src/lib.rs");
    expect(expanded).toEqual({ seed: "crates/hub/src/lib.rs", note: null });
  });

  test("a directory without an entry file is refused", async () => {
    const error = await refusal(
      expandDepsSeed("/repo", "crates/hub", fake(["src/client.rs", "src/supervisor.rs"])),
    );
    expect(error.code).toBe("invalid_arguments");
    expect(error.message).toContain("no src/lib.rs or src/main.rs");
    expect(error.hint).toContain("filer_bundle");
  });
});
