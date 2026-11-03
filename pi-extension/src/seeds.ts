//! Directory seeds: the natural seed for "how is this crate built" is the
//! crate's directory, but the CLI only accepts files and units (#68).
//!
//! Expansion happens here, in the adapter, so the CLI keeps one seed grammar.
//! Every seed that comes out is still re-checked by the CLI: `--roots` and the
//! seed resolver reject anything outside the project root (`walk_roots` in
//! `src/filter.rs`), so this module only shapes the list, it is not the only
//! escape check. The refusals below are about *cost*: `$HOME`, the project
//! root and a directory with hundreds of files would spend a bundle's budget
//! before the resolver ever ran.

import { readdir } from "node:fs/promises";
import { homedir } from "node:os";
import { join, relative, resolve, sep } from "node:path";
import { FilerToolError } from "./errors";
import { sourceExtensionOf } from "./guidance";

/** A directory wider than this is refused; pass a narrower path instead. */
export const MAX_DIRECTORY_SEEDS = 40;

/** The filesystem view directory expansion needs; tests pass a fake. */
export interface SeedFilesystem {
  /** Absolute source files below `dir`, or null when `dir` is not a directory. */
  listSourceFiles(dir: string): Promise<string[] | null>;
  /** The home directory, refused as a seed. */
  home(): string;
}

/**
 * The real filesystem. Symlinks are skipped rather than followed, so a seed
 * cannot leave the directory it names (or the project root) through a link.
 */
export function createSeedFilesystem(): SeedFilesystem {
  return {
    home: () => homedir(),
    async listSourceFiles(dir) {
      const files: string[] = [];
      const queue = [dir];
      let root = true;
      while (queue.length > 0 && files.length <= MAX_DIRECTORY_SEEDS) {
        const current = queue.shift() as string;
        let entries;
        try {
          entries = await readdir(current, { withFileTypes: true });
        } catch {
          // Only the seed itself failing means "not a directory"; an
          // unreadable subdirectory is skipped like an unreadable file.
          if (root) return null;
          continue;
        }
        root = false;
        entries.sort((a, b) => a.name.localeCompare(b.name));
        for (const entry of entries) {
          const path = join(current, entry.name);
          if (entry.isDirectory()) queue.push(path);
          else if (entry.isFile() && sourceExtensionOf(entry.name) !== null) files.push(path);
        }
      }
      return files;
    },
  };
}

/** A directory seed after expansion. */
export interface ExpandedSeeds {
  /** Seeds as the CLI takes them: relative to the root, entry file first. */
  seeds: string[];
  /** The expanded directory, relative to the root (`crates/hub`). */
  directory: string;
  /** The crate's `src/lib.rs` or `src/main.rs`, when it has one. */
  entry: string | null;
}

/** `a/b/c` on every platform: the CLI accepts either, the tests expect one. */
function portable(path: string): string {
  return path.split(sep).join("/");
}

function refusal(message: string, hint: string): FilerToolError {
  return new FilerToolError("invalid_arguments", message, { hint });
}

/**
 * Expand one seed when it names a directory, or return null when it is an
 * ordinary file/unit seed (the caller passes it through untouched).
 *
 * Entry files go first so `src/lib.rs` leads the bundle, and `depth` still
 * means what it means: the seeds are depth 0, their closure follows.
 */
export async function expandDirectorySeed(
  root: string,
  seed: string,
  filesystem: SeedFilesystem = createSeedFilesystem(),
): Promise<ExpandedSeeds | null> {
  // `path::Symbol` and `path::Class.method` are units, never directories.
  if (seed.includes("::")) return null;
  const project = resolve(root);
  const directory = resolve(project, seed);
  const relativeDirectory = relative(project, directory);
  if (directory === resolve(filesystem.home())) {
    throw refusal(
      "refusing to seed the home directory",
      "pass a narrower path such as crates/<crate>",
    );
  }
  if (relativeDirectory.startsWith("..") || relativeDirectory.startsWith(sep)) {
    throw refusal(
      `seed ${seed} is outside ${project}`,
      "seeds are paths inside the project root",
    );
  }
  if (relativeDirectory.length === 0) {
    throw refusal(
      `refusing to seed the project root ${project}`,
      "pass a narrower path such as crates/<crate> or one of its source files",
    );
  }
  const listed = await filesystem.listSourceFiles(directory);
  if (listed === null) return null;
  if (listed.length === 0) {
    throw refusal(
      `no source files below ${relativeDirectory}`,
      "pass a source file or a directory that contains one",
    );
  }
  if (listed.length > MAX_DIRECTORY_SEEDS) {
    throw refusal(
      `seed ${relativeDirectory} expands to more than ${MAX_DIRECTORY_SEEDS} source files`,
      `pass a narrower directory, or up to ${MAX_DIRECTORY_SEEDS} source files explicitly`,
    );
  }
  const names = listed.map((file) => portable(relative(project, file)));
  const entry =
    names.find((name) => name.endsWith("src/lib.rs")) ??
    names.find((name) => name.endsWith("src/main.rs")) ??
    null;
  const seeds = [...names].sort((a, b) => {
    if (a === entry) return -1;
    if (b === entry) return 1;
    return a.localeCompare(b);
  });
  return { seeds, directory: portable(relativeDirectory), entry };
}

/**
 * The seed that answers a whole-crate question: the crate's entry file, since
 * `filer_deps` takes one seed and everything the crate contains is in its
 * closure (`pub mod` / `import` edges).
 */
export function directoryEntrySeed(expanded: ExpandedSeeds): string {
  if (expanded.entry === null) {
    throw refusal(
      `seed ${expanded.directory} has no src/lib.rs or src/main.rs`,
      `pass one of its ${expanded.seeds.length} source files, or call filer_bundle on the directory`,
    );
  }
  return expanded.entry;
}

/** The starting point for `filer_deps`, or the seed unchanged. */
export async function expandDepsSeed(
  root: string,
  seed: string,
  filesystem: SeedFilesystem = createSeedFilesystem(),
): Promise<{ seed: string; note: string | null }> {
  const expanded = await expandDirectorySeed(root, seed, filesystem);
  if (expanded === null) return { seed, note: null };
  const entry = directoryEntrySeed(expanded);
  return {
    seed: entry,
    note: `${seed} -> ${entry} (${expanded.seeds.length} source files)`,
  };
}
