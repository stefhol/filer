//! Session-local overrides for the `/filer` commands.
//!
//! `/filer bin` and `/filer root` set a per-session override that must survive a
//! session reload. The state is one `filer-overrides` custom entry; the last one
//! on the active branch wins. The extension appends one entry per change and
//! restores from `getBranch()` on `session_start` and `session_tree`, never from
//! `getEntries()` (abandoned branches represent alternative histories).
//!
//! The store is pure except for the injected `persist` callback, so `bun test`
//! exercises restore/save without the pi runtime.

import type { SessionEntry } from "@earendil-works/pi-coding-agent";

/** The custom entry type the commands append and restore. */
export const OVERRIDES_ENTRY = "filer-overrides";

export interface FilerOverrides {
  bin?: string;
  root?: string;
}

export interface SessionOverrides {
  /** The current overrides; a copy, so callers cannot mutate the store. */
  get(): FilerOverrides;
  /** Set or clear the binary override and append the new state. */
  setBin(bin: string | undefined): void;
  /** Set or clear the root override and append the new state. */
  setRoot(root: string | undefined): void;
  /** Restore the last `filer-overrides` entry on the branch. */
  restore(entries: readonly SessionEntry[]): void;
}

export interface SessionOverridesOptions {
  /** Called after every change; `index.ts` appends the session entry. */
  persist?: (overrides: FilerOverrides) => void;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return undefined;
  }
  return value as Record<string, unknown>;
}

function text(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length === 0 ? undefined : trimmed;
}

/** Read one `filer-overrides` payload; unknown keys are ignored. */
export function overridesFromData(value: unknown): FilerOverrides {
  const record = asRecord(value);
  if (record === undefined) return {};
  const bin = text(record.bin);
  const root = text(record.root);
  return {
    ...(bin === undefined ? {} : { bin }),
    ...(root === undefined ? {} : { root }),
  };
}

/**
 * The last `filer-overrides` custom entry on the branch wins. Every matching
 * entry is a full snapshot, so a later `setBin` also keeps the earlier `root`.
 */
export function overridesFromBranch(entries: readonly SessionEntry[]): FilerOverrides {
  let overrides: FilerOverrides = {};
  for (const entry of entries) {
    if (entry.type === "custom" && entry.customType === OVERRIDES_ENTRY) {
      overrides = overridesFromData(entry.data);
    }
  }
  return overrides;
}

export function createSessionOverrides(
  options: SessionOverridesOptions = {},
): SessionOverrides {
  let overrides: FilerOverrides = {};
  const save = (next: FilerOverrides): void => {
    overrides = next;
    options.persist?.(next);
  };
  return {
    get() {
      return { ...overrides };
    },
    setBin(bin) {
      const next = { ...overrides };
      if (bin === undefined) delete next.bin;
      else next.bin = bin;
      save(next);
    },
    setRoot(root) {
      const next = { ...overrides };
      if (root === undefined) delete next.root;
      else next.root = root;
      save(next);
    },
    restore(entries) {
      overrides = overridesFromBranch(entries);
    },
  };
}
