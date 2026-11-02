# pi-filer

The pi extension: a thin adapter over the `filer` CLI. It spawns the binary, discovers which
binary and project root to use, guards the JSON `schema_version` and exposes one typed
operation per CLI command. It does not index, resolve imports, slice scopes or render
bundles — the CLI owns all of that, and the extension is versioned with it.

## From clone to green tests

```sh
cargo build --release            # the binary the integration tests spawn
cd pi-extension
bun test
```

Without `target/release/filer` the integration suite is skipped with a message, so `bun test`
stays green on a fresh clone. To run that suite against a binary somewhere else, point
`FILER_BIN` at it:

```sh
FILER_BIN=$(pwd)/../target/release/filer bun test test/integration.test.ts
```

Load the extension for one pi invocation, without installing it:

```sh
pi -e ./pi-extension
```

## Layout

| Path | Role |
| --- | --- |
| `index.ts` | entry point: registers `--filer-bin` / `--filer-root` and builds the adapter (the one module pi loads) |
| `src/cli.ts` | `execFile` runner, binary and root discovery, timeout, output buffer |
| `src/args.ts` | pure argv builders, one per CLI command (`--root <root> --json --quiet <cmd> ...`) |
| `src/operations.ts` | typed operations: run, classify the exit status, guard the version, return `{ data, text, warnings }` |
| `src/commands.ts` | `/filer` subcommands, `M new` diff, why-chain text, status line |
| `src/session.ts` | `filer-overrides` session state: save, restore, last branch entry wins |
| `src/guidance.ts` | steering: the `tool_guidance` prompt section and the `read` nudge, both gated on the usability flag |
| `src/schema.ts` | `schema_version` 1 guard and the shared mismatch message |
| `src/errors.ts` | exit codes and `{"error": ...}` payloads → `FilerToolError` with a hint |
| `src/format.ts` | envelope → markdown (the README `jq` recipe) and the 32 KiB / 2000-line cap |
| `test/` | unit tests per module plus `integration.test.ts` against the real binary and `tests/fixtures/java-scopes` |

## Rules

- **No runtime SDK import below `index.ts`.** `@earendil-works/pi-coding-agent` is a
  type-only import, `index.ts` included, so `bun test` runs without the pi runtime installed.
- One function per CLI command in `src/operations.ts`. Resolution, slicing and rendering stay
  in Rust; the adapter only builds argv and maps JSON.
- `--json --quiet` is mandatory on every call — the adapter parses the JSON surface and never
  reads the human summary.
- `src/args.ts` builds `string[]`, never a shell string, so nothing the model types can be
  quoted, split or interpolated.
- Bumping `schema_version` in `src/schema.rs` means bumping `SUPPORTED_SCHEMA_VERSION` in
  `src/schema.ts` in the same change.
- `SOURCE_EXTENSIONS` in `src/guidance.ts` mirrors `Language::from_path` in `src/lang.rs`; a new
  language needs the extension listed there too, or the model gets no nudge for its files.
- Steering never blocks a tool: `read` always succeeds and the nudge is appended, so a wrong
  guess costs one extra sentence, not a failed turn.

CI runs the same commands (`.github/workflows/ci.yml`, job `extension`):
`cargo build --release --locked`, then `bun test` with `working-directory: pi-extension`.
