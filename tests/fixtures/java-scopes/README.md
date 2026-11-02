# java-scopes fixture corpus

Small, self-contained Java projects with golden output for the scope model
(`file` / `symbol` / `methods`), the call graph and the resolver fallbacks.
The corpus is deliberately **source-only** — it ships no Rust test harness.
`run.sh` is the only executable part; it drives the real CLI and compares or
regenerates `expected/`.

Use it to pin the resolver/renderer/state contract while refactoring the
internals (e.g. materializing `Graph`/`Edge`, adding spans, or changing the
renderer): a change that alters closure or scope assignment must change these
goldens.

## Layout

```
cases.txt                        case -> fixture -> CLI seed (one line each)
run.sh                           generate / --check the goldens
file-seed/                       two classes in one file
class-seed/                      every receiver form of one class
method-seed/                     intra-class calls, overloads, abstract, enum
fallback/                        unknown receiver, method-not-found, chained
<fixture>/src/main/java/...      Java sources (packages are real)
expected/<case>.md               `filer bundle` markdown (root normalized)
expected/<case>.search.jsonl     `filer search --state` rows (picker contract)
expected/<case>.state.toml       `bundle.toml` v3 written by the bundle run
```

The fixture directory is its own `--root`, so paths inside the goldens are
repository-relative (`src/main/java/com/example/...`) even though the fixture
itself sits inside this repo. Nothing here needs Maven or Gradle.

## Running

```sh
cargo build --release                  # once, in the repo root
tests/fixtures/java-scopes/run.sh --check   # verify
tests/fixtures/java-scopes/run.sh           # regenerate expected/
```

`run.sh` picks `$FILER`, then `target/{release,debug}/filer` of the checkout,
then `filer` on `$PATH`. Set `FILER=/path/to/filer` to test another build.

Each case runs `filer bundle` with `--depth 2 --state <tmp>` and then
`filer search --state <tmp>`; the state file is kept per case, so cases never
influence each other (state *merging* is intentionally not covered here — that
belongs to the state-migration tests). The goldens are normalized to stay
machine-independent:

| Value | Normalized to |
| --- | --- |
| absolute fixture directory | `<root>` |
| branch name in the TOML | `<branch>` |
| `created = <unix>` in the TOML | `created = <timestamp>` |

Everything else is byte-exact, including line numbers in `### ... (line N)`.

## Cases

| Case | Fixture | Seed | Covers |
| --- | --- | --- | --- |
| `file-seed` | `file-seed` | `.../Service.java` | a file seed keeps both classes whole and follows the calls of both |
| `class-seed` | `class-seed` | `...::Service` | field, parameter, local, static, static-import, inherited and `super` receivers |
| `method-seed.intra` | `method-seed` | `...::Pipeline.run` | intra-class calls, overloads by name, stub of an unvisited method |
| `method-seed.abstract` | `method-seed` | `...::Job.runOnce` | abstract callee kept as a signature, concrete siblings whole |
| `method-seed.enum` | `method-seed` | `...::Level.scaled` | enum members are methods; enum constants stay in the shell |
| `fallback` | `fallback` | `...::Controller` | unknown receiver, chained call, method-not-found fallback, `new Type().m()` |

## Expected units

The tables below are the contract the goldens encode. `path` is relative to the
fixture root; `d`/`reason` are the depth and origin written to `bundle.toml`;
`methods` is the kept method list of a `methods` entry (sorted, as in the TOML).

### `file-seed` — one file, two classes

| Scope | Path | Symbol | Methods | d | Reason |
| --- | --- | --- | --- | --- | --- |
| `file` | `.../file/Service.java` | — | — | 0 | `seed` |
| `methods` | `.../file/Store.java` | `Store` | `raw` | 1 | `call` |
| `methods` | `.../file/util/Helper.java` | `Helper` | `twice` | 1 | `call` |

`Repo` is a second top-level class of the seed file, so it is part of the
`file` entry, not its own unit. `Store.raw()` and `Helper.twice()` call each
other, so depth 2 adds nothing. `Service.unused()` and `Repo.spare()` are kept
because the whole seed file is; only the unused methods of the depth-1 classes
(`Helper.unused`, `Store.unused`) must become `signature;   // body omitted`
stubs.

### `class-seed` — receiver forms

| Scope | Path | Symbol | Methods | d | Reason |
| --- | --- | --- | --- | --- | --- |
| `symbol` | `.../shop/Service.java` | `Service` | — | 0 | `seed` |
| `methods` | `.../shop/Cart.java` | `Cart` | `cartCount` | 1 | `call` |
| `methods` | `.../shop/Ledger.java` | `Ledger` | `ledgerTotal` | 1 | `call` |
| `methods` | `.../shop/Repo.java` | `Repo` | `repoSize` | 1 | `call` |
| `methods` | `.../shop/base/BaseService.java` | `BaseService` | `audit`, `base` | 1 | `call` |
| `methods` | `.../shop/util/Formats.java` | `Formats` | `money`, `zero` | 1 | `call` |

Each receiver in `Service` produces exactly one file: `repo.repoSize()` (field),
`param.ledgerTotal()` (parameter), `local.cartCount()` (local),
`Formats.zero()` (static type), `base()` / `audit()` / `super.base()`
(inherited) and `money(7)` (static import). The unused methods
(`repoUnused`, `ledgerUnused`, `cartUnused`) must not be *kept*: they survive
only as stubs in the class shells of their own entries.

### `method-seed`

| Scope | Path | Symbol | Methods | d | Reason |
| --- | --- | --- | --- | --- | --- |
| `methods` | `.../method/Pipeline.java` | `Pipeline` | `prepare`, `run`, `toResult` | 0 | `seed` |
| `methods` | `.../method/base/Job.java` | `Job` | `labels`, `runOnce`, `step` | 0 | `seed` |
| `methods` | `.../method/Level.java` | `Level` | `scaled`, `weight` | 0 | `seed` |

- Three `prepare` overloads exist, and all three are kept even though only
  `run` is the seed (overloads resolve by name).
- `Pipeline.skipped()` is not on the call path and must be a stub.
- `Job.step()` is abstract; the class shell keeps it as a signature, not a
  body, and it must not drop the class.
- `Level` proves enum constants are part of the rendered shell; `weight()` is
  reached from `scaled()`.

### `fallback`

| Scope | Path | Symbol | Methods | d | Reason |
| --- | --- | --- | --- | --- | --- |
| `symbol` | `.../fallback/Controller.java` | `Controller` | — | 0 | `seed` |
| `methods` | `.../fallback/Bridge.java` | `Bridge` | `lookup` | 1 | `call` |
| `symbol` | `.../fallback/Repo.java` | `Repo` | — | 1 | `call` |

- `bridge.lookup().tick()` and `new Bridge().lookup().tick()` must keep
  `Bridge.lookup` (first hop) and add **no** edge to `Tick` (the chained
  receiver is `Object`).
- `repo.missing()` targets a known class without that method, so the whole
  class is kept as `scope = "symbol"` — `Repo.unused()` must be rendered too.
- `new Repo().size()` is a supported receiver and must produce the `Repo` edge.
- `Tick` and `Dead` must **not** appear in the bundle or the state. They are
  indexed, so they do get search rows — with `included:false`.

## Picker contract (`expected/*.search.jsonl`)

Each line is one `filer search` row (JSON is the default output), in CLI
order. The rows carry
`scope`, `symbol`, `member`, `kind`, `fqn`, `line`, and the state-derived
`included` / `depth` / `reason`. The corpus asserts, among others:

- `[file]` rows exist for every file, `kind = "file"`, `scope = "file"`,
  `line = null`, `fqn` = the path.
- `[class]` rows have `kind = "class"`, `scope = "symbol"`, the package-qualified
  `fqn`, and `line` = the class line.
- `[method]` rows have `kind = "method"`, `scope = "methods"`, and
  `fqn = package.Class.method`.

`included` follows the state-entry coverage rule, so it is coarser than the
unit tables above:

- a `file` entry covers the file row and **every** class and method row of that
  file (in `file-seed`, `Repo` and both `Service` classes are included because
  the whole file is);
- a `symbol` entry covers its class row and all of that class's method rows
  (in `fallback`, `Repo.unused` is marked included because the whole class is);
- a `methods` entry covers only the listed methods (`Store.raw` is included,
  `Store.unused` is not).

If the picker contract changes, edit `expected/*.search.jsonl` and the Lua
picker together — do not silently re-baseline.

## Not covered here

- State v1→v2→v3 migration, merge/union and `show` snapshots: separate tests.
- `--at-line`, `--with-same-package`, `--with-config`, budgets/ordering.
- Non-Java languages.
