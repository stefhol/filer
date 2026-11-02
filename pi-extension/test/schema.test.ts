import { describe, expect, test } from "bun:test";
import {
  SUPPORTED_SCHEMA_VERSION,
  assertRowSchemaVersions,
  assertSchemaVersion,
  assertSchemaVersionIfPresent,
  schemaMismatchMessage,
  schemaVersionOf,
} from "../src/schema";
import { FilerToolError } from "../src/errors";

const mismatch = (reported?: number | string | undefined): string =>
  `extension expects schema_version 1, binary reports ${reported ?? "none"} — update one of them`;

describe("schema_version guard", () => {
  test("the adapter speaks version 1", () => {
    expect(SUPPORTED_SCHEMA_VERSION).toBe(1);
  });

  test("accepts a matching envelope", () => {
    expect(() => assertSchemaVersion({ schema_version: 1 })).not.toThrow();
  });

  test("rejects a newer envelope with the exact message", () => {
    expect(() => assertSchemaVersion({ schema_version: 2 }, "bundle")).toThrow(
      mismatch(2),
    );
  });

  test("rejects a missing version instead of parsing prose", () => {
    expect(() => assertSchemaVersion({ entries: [] }, "bundle")).toThrow(mismatch());
  });

  test("the thrown error is a structured tool error", () => {
    try {
      assertSchemaVersion({ schema_version: 7 }, "show");
      throw new Error("guard did not throw");
    } catch (error) {
      expect(error).toBeInstanceOf(FilerToolError);
      expect((error as FilerToolError).code).toBe("schema_version_mismatch");
      expect((error as FilerToolError).details).toEqual({
        expected: 1,
        reported: 7,
        what: "show",
      });
    }
  });

  test("guards every search row, not just the first", () => {
    expect(() =>
      assertRowSchemaVersions([{ schema_version: 1 }, { path: "A.java" }]),
    ).toThrow(mismatch());
    expect(() =>
      assertRowSchemaVersions([{ schema_version: 1 }, { schema_version: 2 }]),
    ).toThrow(mismatch(2));
  });

  test("unversioned surfaces are guarded only once they carry a version", () => {
    expect(() => assertSchemaVersionIfPresent({ target: "A.java" }, "why")).not.toThrow();
    expect(() => assertSchemaVersionIfPresent({ schema_version: 2 }, "why")).toThrow(
      mismatch(2),
    );
  });

  test("schemaVersionOf reads a number only", () => {
    expect(schemaVersionOf({ schema_version: 1 })).toBe(1);
    expect(schemaVersionOf({ schema_version: "1" })).toBeUndefined();
    expect(schemaVersionOf([])).toBeUndefined();
    expect(schemaVersionOf(null)).toBeUndefined();
  });

  test("schemaMismatchMessage is shared with the tests", () => {
    expect(schemaMismatchMessage(3)).toBe(mismatch(3));
    expect(schemaMismatchMessage(undefined)).toBe(mismatch());
  });
});
