import { describe, expect, test } from "bun:test";
import type { SessionEntry } from "@earendil-works/pi-coding-agent";
import {
  OVERRIDES_ENTRY,
  createSessionOverrides,
  overridesFromBranch,
  overridesFromData,
} from "../src/session";

function customEntry(customType: string, data: unknown): SessionEntry {
  return {
    type: "custom",
    customType,
    data,
    id: "entry",
    parentId: null,
    timestamp: "2024-01-01T00:00:00Z",
  };
}

function labelEntry(): SessionEntry {
  return {
    type: "label",
    targetId: "target",
    label: "checkpoint",
    id: "label",
    parentId: null,
    timestamp: "2024-01-01T00:00:00Z",
  };
}

describe("overridesFromData", () => {
  test("reads bin and root and trims", () => {
    expect(overridesFromData({ bin: " /opt/filer ", root: " /repo " })).toEqual({
      bin: "/opt/filer",
      root: "/repo",
    });
  });

  test("drops unknown keys and empty values", () => {
    expect(overridesFromData({ bin: "  ", other: 1 })).toEqual({});
    expect(overridesFromData(null)).toEqual({});
    expect(overridesFromData("nope")).toEqual({});
  });
});

describe("overridesFromBranch", () => {
  test("returns {} without a matching entry", () => {
    expect(overridesFromBranch([])).toEqual({});
    expect(overridesFromBranch([labelEntry()])).toEqual({});
    expect(overridesFromBranch([customEntry("other", { bin: "/x" })])).toEqual({});
  });

  test("the last matching entry wins", () => {
    const entries = [
      customEntry(OVERRIDES_ENTRY, { bin: "/one", root: "/root" }),
      labelEntry(),
      customEntry(OVERRIDES_ENTRY, { bin: "/two" }),
    ];
    expect(overridesFromBranch(entries)).toEqual({ bin: "/two" });
  });
});

describe("createSessionOverrides", () => {
  test("setBin/setRoot keep the previous override and persist each snapshot", () => {
    const saved: unknown[] = [];
    const session = createSessionOverrides({
      persist: (overrides) => saved.push(overrides),
    });
    session.setBin("/opt/filer");
    session.setRoot("/repo");
    expect(session.get()).toEqual({ bin: "/opt/filer", root: "/repo" });
    expect(saved).toEqual([
      { bin: "/opt/filer" },
      { bin: "/opt/filer", root: "/repo" },
    ]);
  });

  test("setBin(undefined) clears only the binary", () => {
    const session = createSessionOverrides();
    session.setBin("/opt/filer");
    session.setRoot("/repo");
    session.setBin(undefined);
    expect(session.get()).toEqual({ root: "/repo" });
  });

  test("restore replaces the current state with the branch's last entry", () => {
    const session = createSessionOverrides();
    session.setBin("/old");
    session.restore([
      customEntry(OVERRIDES_ENTRY, { bin: "/one" }),
      customEntry(OVERRIDES_ENTRY, { root: "/repo" }),
    ]);
    expect(session.get()).toEqual({ root: "/repo" });
  });

  test("get returns a copy", () => {
    const session = createSessionOverrides();
    session.setBin("/opt/filer");
    const overrides = session.get();
    overrides.bin = "/mutated";
    expect(session.get().bin).toBe("/opt/filer");
  });
});
