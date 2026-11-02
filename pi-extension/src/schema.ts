//! Wire-format guard.
//!
//! The extension parses the JSON surface directly, so a binary that speaks a
//! different `schema_version` must fail loudly instead of yielding half-parsed
//! data. The version lives in `src/schema.rs` (`SCHEMA_VERSION = 1`); bump both
//! sides in the same change.

import { FilerToolError } from "./errors";

export const SUPPORTED_SCHEMA_VERSION = 1;

/** The version an envelope reports, or `undefined` when it carries none. */
export function schemaVersionOf(value: unknown): number | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return undefined;
  }
  const reported = (value as Record<string, unknown>).schema_version;
  return typeof reported === "number" ? reported : undefined;
}

/** The one error message both the guard and its tests use. */
export function schemaMismatchMessage(reported: unknown): string {
  const shown =
    typeof reported === "number" || typeof reported === "string"
      ? String(reported)
      : "none";
  return `extension expects schema_version ${SUPPORTED_SCHEMA_VERSION}, binary reports ${shown} — update one of them`;
}

function schemaMismatch(reported: unknown, what: string): FilerToolError {
  return new FilerToolError("schema_version_mismatch", schemaMismatchMessage(reported), {
    hint: "update the pi extension and the filer binary together",
    details: { expected: SUPPORTED_SCHEMA_VERSION, reported: reported ?? null, what },
  });
}

/**
 * Require `value.schema_version === SUPPORTED_SCHEMA_VERSION`.
 *
 * Used for every envelope the CLI versions (`bundle`, `show`, `deps`, `state`,
 * `index`) and for every `search` row.
 */
export function assertSchemaVersion(value: unknown, what = "envelope"): void {
  const reported = schemaVersionOf(value);
  if (reported !== SUPPORTED_SCHEMA_VERSION) {
    throw schemaMismatch(reported, what);
  }
}

/**
 * Guard payloads that predate versioning (`why` is a bare object today) but
 * must still fail when a newer binary adds a version we do not understand.
 */
export function assertSchemaVersionIfPresent(value: unknown, what = "envelope"): void {
  if (schemaVersionOf(value) === undefined) return;
  assertSchemaVersion(value, what);
}

/** Every row of the JSON-lines `search` surface carries the version. */
export function assertRowSchemaVersions(rows: readonly unknown[], what = "search row"): void {
  for (const row of rows) {
    if (schemaVersionOf(row) === undefined) {
      throw schemaMismatch(undefined, what);
    }
    assertSchemaVersion(row, what);
  }
}
