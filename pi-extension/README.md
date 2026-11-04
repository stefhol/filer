# pi-filer

The pi extension: a thin adapter over the `filer` CLI. It spawns the binary, discovers which
binary and project root to use, guards the JSON `schema_version` and exposes one typed
operation per CLI command. It does not index, resolve imports, slice scopes or render
bundles — the CLI owns all of that, and the extension is versioned with it.

## From clone to green tests

```sh
cargo build --release            # the binary the integration tests spawn
cd pi-extension
bun install                      # typebox, pi-tui and the type deps
bun test
```

Without `target/release/filer` the integration suite is skipped with a message, so `bun test`
stays green on a fresh clone. `bun.lock` is committed so CI can install reproducibly with
`bun install --frozen-lockfile`. Without `bun install` the run fails on `src/tools.ts`:
the tool schemas need `typebox` at import time and the result renderers need
`@earendil-works/pi-tui`. To run that suite against a binary somewhere else, point
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
| `src/guidance.ts` | steering: the `tool_guidance` prompt section, the numbered `read` nudge and the declaration-search (`bash`) nudge, all gated on the usability flag; memoizes the closure probe per path and the symbol probe per name |
| `src/seeds.ts` | directory seeds: expand a crate directory to its source files, with the root/`$HOME`/40-file refusals |
| `src/source.ts` | which extensions filer parses, shared by the steering and the seed expansion |
| `src/schema.ts` | `schema_version` 1 guard and the shared mismatch message |
| `src/errors.ts` | exit codes and `{"error": ...}` payloads → `FilerToolError` with a hint |
| `src/format.ts` | envelope → markdown (the README `jq` recipe), the closure summary (`diagnosticsSummary`, `## Closure`), and the 32 KiB / 2000-line cap |
| `test/` | unit tests per module plus `integration.test.ts` against the real binary and `tests/fixtures/java-scopes` |

## Rules

- **Tool exposure is part of the contract.** `filer_bundle`, `filer_search`, `filer_deps`,
  `filer_callers` and `filer_refs` are `direct`; `filer_show`, `filer_why` and `filer_state`
  are `deferred` (`tool_search` reaches them). Steering must never point at a `deferred`
  tool, because the model would not find it in its loadout (#71).
- **`@earendil-works/pi-coding-agent` stays type-only.** It is imported with `import type`
  everywhere, `index.ts` included. The two host modules the tools need at runtime —
  `typebox` (tool schemas) and `@earendil-works/pi-tui` (result renderers, imported in
  `src/tools.ts`) — are declared as peer and dev dependencies, so `bun install` satisfies
  them locally and pi provides them in a session.
- One function per CLI command in `src/operations.ts`. Resolution, slicing and rendering stay
  in Rust; the adapter only builds argv and maps JSON.
- `--json --quiet` is mandatory on every call — the adapter parses the JSON surface and never
  reads the human summary.
- `src/args.ts` builds `string[]`, never a shell string, so nothing the model types can be
  quoted, split or interpolated.
- Bumping `schema_version` in `src/schema.rs` means bumping `SUPPORTED_SCHEMA_VERSION` in
  `src/schema.ts` in the same change.
- `SOURCE_EXTENSIONS` in `src/source.ts` mirrors `Language::from_path` in `src/lang.rs`; a new
  language needs the extension listed there too, or the model gets no nudge for its files and
  a directory seed skips them.
- Steering never blocks a tool: `read` always succeeds and the nudge is appended, so a wrong
  guess costs one extra sentence, not a failed turn.

CI runs the same commands (`.github/workflows/ci.yml`, job `extension`, all with
`working-directory: pi-extension`): `cargo build --release --locked`, `bun install
--frozen-lockfile`, `bun run typecheck`, then `bun test`.
