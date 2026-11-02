# filer

Select code in Neovim and get one pastable markdown blob for your chat LLM or coding agent —
with the files your selection imports, and the files those import, already included.

`filer` covers the boring part: finding the three or four files that belong together and
formatting them so the model sees the whole picture instead of a fragment. The same
selection is available as versioned JSON, so an agent can pull its own context instead of
being pasted to.

```
┌─ Telescope: filer: symbols (demo) ───────────────────┐
│ > user                                               │
│   [file]   com/example/model/User.java   <Tab>       │
│   [class]  com.example.model.User        <Tab>       │
│   [method] com.example.model.User.name   <Tab>       │
└──────────────────────────────────────────────────────┘
                        ↓  <CR>
┌─ filer://bundle-1 ───────────────────────────────────┐
│ # Code bundle: demo                                  │
│ - files: 4                                           │
│ ## Contents                                          │
│ ### `src/main/java/com/example/model/User.java`      │
│ ```java ... ```                                      │
└──────────────────────────────────────────────────────┘
```

## What it does

Supported languages: Java, Scala, Kotlin, Groovy, C#, Rust, TypeScript, JavaScript, Vue and
`.fe`. Parsing is tree-sitter based (no LSP, no classpath, no compiler). Three tiers of
support:

| Tier | Languages | What you get |
| --- | --- | --- |
| Full | Java, Scala, Kotlin, Groovy, C#, Rust, TS/JS | import/`use`/module resolution, type-usage edges, call graph, `[file]`/`[class]`/`[method]` scopes |
| Whole-file | Vue | `<script>` blocks parsed as TS/JS; never sliced |
| Function-level | `.fe` | one project-wide namespace; calls resolved by name; function seeds |

- Resolves package-style imports against the files that actually exist in your repo:
  Java `import a.b.C;` / `import static ...` / `import a.b.*;`, Scala 2 and 3 (selector
  lists, renames, `given`, comma-separated imports, nested or braced packages, package
  objects), Kotlin wildcards and aliases, Groovy wildcards, static imports and aliases, and
  C# `using` namespaces, `using static` and aliases.
- Resolves Rust module paths: `use crate::...`, `self::...`, `super::...`, grouped imports
  (`use crate::app::{Service, model::User}`), wildcards and aliases, plus `mod foo;`
  declarations. Rust modules resolve by Cargo directory layout (`foo.rs` or `foo/mod.rs`),
  not by package name.
- Resolves TypeScript/JavaScript/Vue module specifiers: relative imports and re-exports
  (`./x`, `../y.js`), extensionless imports, directory `index` files, `.js` specifiers that
  point at a `.ts` source, dynamic `import()` and CommonJS `require()`. Vue `<script>` and
  `<script setup>` blocks are extracted and parsed as TypeScript/JavaScript.
- Resolves `tsconfig.json` / `jsconfig.json` path aliases and `baseUrl` imports, so
  `import { User } from "@/model/user"` pulls `src/model/user.ts` in when the config says
  `"paths": { "@/*": ["src/*"] }`. See [TypeScript config aliases](#typescript-config-aliases).
- Follows type *usages* too, so a class that references a sibling in its own package — which
  Java, Scala, Kotlin, Groovy or C# needs no import for, e.g. `indexOps(DemoDocument.class)`,
  `DemoDocument.create()` or `new DemoDocument()` — still pulls that file in. Rust type
  positions and `Type::method()` receivers work the same way. Module languages rely on
  their explicit imports instead of repo-wide name guessing.
- Extracts **symbols** (top-level functions, classes, objects, global declarations, Rust
  structs/enums/traits/functions, Java methods) with their byte spans, so a bundle can carry
  one declaration out of a huge file. Class methods add a second granularity: class shell for
  context, call-path bodies for content.
- Every entry has an explicit **scope** — `file`, `symbol` or `methods` — so the picker,
  the state file and the renderer agree on what was included and why.
- Walks the resulting graph to a configurable depth, then sorts what it found into a
  documented canonical order (seeds, `file` > `symbol` > `methods`, depth, reason, path,
  span) so the same selection always renders the same bytes, no matter how the imports moved.
- Applies an optional byte/token budget (`--max-bytes`, `--max-tokens`) by cutting the tail of
  that order, and lists every dropped unit in `omitted` instead of silently shortening the
  bundle. See [Order and budget](#order-and-budget).
- Remembers what is already bundled in a TOML state file, marks those entries with `✓` in
  the picker, and grows the bundle instead of replacing it. Switching git branch kills the
  old state automatically.
- Renders the selection as markdown: header, file list with import depths, then one fenced
  block per file or excerpt. Fences grow automatically if a file already contains backticks.
- Speaks JSON as well as markdown: spans, tokens, a budget report, diagnostics, coverage and
  the provenance chain, with stable exit codes and `--quiet` for clean tool output. That is
  the agent-facing contract; markdown is one consumer of it.
- Caches the index in `~/.cache/filer/`, keyed by root path and invalidated per file by
  mtime + size. Re-indexing a 2000-file project takes ~40 ms, so the picker re-queries on
  every keystroke.
- Writes the bundle state and the index cache atomically: hidden temp file in the same
  directory, `fsync`, then `rename`, serialized by an advisory lock on a `.name.lock`
  sidecar. A crash or two concurrent `filer` runs leave the old or the new file, never a
  torn one; a corrupt file is reported and rebuilt instead of aborting.

Things it deliberately does not do: compile your project, read `pom.xml` / `build.sbt` /
`*.csproj` / `Cargo.toml` / `package.json` dependencies, or pretend to resolve libraries. JDK,
Scala stdlib, npm packages, NuGet packages, Rust crates and other third-party imports are
simply reported as unresolved; `node_modules`, package `exports`/`imports` and
`moduleResolution` variants are not consulted. Imports only resolve
within the same language family (JVM-ish languages with each other, TS/JS/Vue with each
other, Rust with itself), never across.

## Build

```sh
cargo build --release     # binary at target/release/filer
# optional, to use it outside Neovim too:
cargo install --path .
```

Requires Rust 1.89 or newer (the state and cache writes use std's file locks) and a
C compiler for the tree-sitter grammars (`gcc`/`clang`).

## Install

Prebuilt binaries live on the [releases page](https://github.com/stefhol/filer/releases).
Linux (x86_64, aarch64):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/stefhol/filer/releases/latest/download/filer-installer.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/stefhol/filer/releases/latest/download/filer-installer.ps1 | iex
```

Targets built per release: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`. macOS is not built yet — use `cargo install --path .` there.

`filer` needs the [tree-sitter CLI](https://tree-sitter.github.io/tree-sitter/creating-parsers#installation)
at runtime — it is not bundled with the binary, so install it on every machine that runs
`filer`: `cargo install tree-sitter-cli` (or your package manager, e.g.
`pacman -S tree-sitter-cli`).

## Releases

Packaging is generated by [cargo-dist](https://axodotdev.github.io/cargo-dist) and configured
in `dist-workspace.toml`. After editing that file, refresh the generated workflow with
`dist generate` (install the CLI however you like — `cargo install cargo-dist --locked`).

**Cutting a release takes one click:** Actions → *Cut release* → *Run workflow*, pick
`patch`/`minor`/`major` or type an explicit version, and run. `.github/workflows/cut-release.yml`
then:

1. bumps `version` in `Cargo.toml` (and `Cargo.lock`), commits `Release vX.Y.Z` to `master`;
2. creates and pushes the annotated tag `vX.Y.Z` (it refuses to reuse an existing tag);
3. dispatches `.github/workflows/release.yml`, which builds the archives and installers for
   every target and publishes the GitHub Release with checksums.

Tick *dry_run* to only compute and print the next version — nothing is committed, tagged or
released. (`dry_run` checks out the ref you dispatched on, so the button can be tested from a
branch.)

You can still release by hand: bump `version` in `Cargo.toml`/`Cargo.lock`, commit, tag
(`git tag vX.Y.Z`), push both, then dispatch the build with
`gh workflow run release.yml -f tag=vX.Y.Z` (the workflow no longer fires on tag pushes).

Every push to `master` also runs `.github/workflows/ci.yml` (fmt, clippy, tests) and uploads a
`filer-x86_64-unknown-linux-gnu.tar.xz` workflow artifact kept for 90 days.

## Neovim setup

Install the binary first with the [release installer](#install) (it lands on `$PATH`).
The full, copy-paste spec lives in [`examples/lazyvim.lua`](examples/lazyvim.lua) — it
already carries the defaults below and pulls the plugin straight from GitHub `master`, so no
local checkout is needed. Drop it into your config:

```sh
# LazyVim / lazy.nvim import dir:
cp examples/lazyvim.lua ~/.config/nvim/lua/plugins/filer.lua
# kickstart.nvim (import = "custom.plugins"):
cp examples/lazyvim.lua ~/.config/nvim/lua/custom/plugins/filer.lua
```

```lua
{
  "stefhol/filer",
  name = "filer",
  cmd = { "FilerSearch", "FilerHere", "FilerBundle", "FilerDepth" },
  keys = {
    { "<leader>fd", "<cmd>FilerDepth<cr>", desc = "filer: show depth" },
    { "<leader>fc", function() require("filer").search() end, desc = "filer: bundle for chat" },
    { "<leader>fh", function() require("filer").here() end, desc = "filer: bundle current file" },
    { "<leader>fC", function() require("filer").bundle({ vim.api.nvim_buf_get_name(0) }, { depth = 0 }) end, desc = "filer: bundle current file only" },
  },
  opts = {
    bin = nil,            -- $PATH (the release installer); set a path if not installed
    root = nil,           -- detect via .git / pom.xml / build.gradle(.kts) / build.sbt / ...
    depth = 2,            -- import hops to follow; change live with :FilerDepth
    same_package = false, -- also pull in sibling files of the same package
    with_config = false,  -- also add pom.xml / gradle / build.sbt / tsconfig / csproj
    split = "vsplit",     -- or "split", "tabnew"
    copy = false,         -- true = also copy the bundle to the system clipboard
    state = true,         -- remember the bundle and mark it in the picker
    state_file = nil,     -- nil = <root>/.filer/bundle.toml
  },
}
```

The plugin needs [telescope.nvim](https://github.com/nvim-telescope/telescope.nvim) for
`:FilerSearch`. `:FilerHere` and `:FilerBundle` have no dependencies.

## Commands

| Command | Effect |
| --- | --- |
| `:FilerSearch` | Telescope picker of bundle scopes: `[file]` for every file, `[class]`/`[method]` for class-based languages, `[type]`/`[function]` elsewhere. `<Tab>` multi-selects, `<CR>` bundles. |
| `:FilerHere` | Bundles the current buffer with its closure. The declaration under the cursor is the seed — the enclosing method or class, or the whole file when nothing more specific applies. `:FilerHere!` asks for a depth first. |
| `:FilerBundle {files}` | Bundles the given files (relative to root or absolute) or excerpts (`path::Symbol`). `:FilerBundle! …` asks for a depth first. |
| `:FilerDepth [n]` | With no argument, shows the current depth; with `n`, uses it for every following bundle; `:FilerDepth reset` restores the `setup` value. |
| `:FilerShow [branch]` | Re-renders the saved bundle (current file contents) without re-resolving. |
| `:FilerReset` | Forgets this branch's bundle; the next bundle starts empty. |
| `:FilerResetAll` | Forgets every branch's bundle. |

### Depth

`depth` is how many import/symbol hops to follow — `0` bundles only the seeds. Set the
starting value with `opts.depth` (default `2`), change it at runtime with `:FilerDepth 3`,
and revert with `:FilerDepth reset`. Per-call overrides work from Lua and never touch the
config:

```lua
require("filer").here({ depth = 0 })                     -- this file only
require("filer").bundle({ "src/Service.java" }, { depth = 1 })
require("filer").search({ depth = 4 })                   -- depth of the next pick
```

In the bundle buffer: `y` yanks, `q` closes. The buffer is `nofile` and never written to
disk. Bundling again — picker or `:FilerHere` — refills that same window and buffer instead
of opening another split; `q` closes it, and the next bundle opens a fresh one.

Root detection uses `vim.fs.root` with markers `.git`, `pom.xml`, `build.gradle(.kts)`,
`settings.gradle(.kts)`, `build.sbt`, `Cargo.toml`, `gradlew`, `mvnw`, `tsconfig.json`,
`jsconfig.json`, `package.json`, `*.csproj`, `*.sln`. Override with `opts.root` (string or
function).

## Bundle scopes: file, class, method

Every picker row and every bundle entry carries an explicit **scope**. The tiers are shared;
each language exposes only the ones it has.

| Scope | Languages | Rendered | Closure |
| --- | --- | --- | --- |
| `file` | all but function-level languages | the whole file | Java/Rust: the calls of every method or function in the file; others: the import/type closure |
| `symbol` | all parsed languages (Vue stays whole-file) | one top-level declaration plus the file header | class/type: the calls of its methods; other symbols: the symbol closure |
| `methods` | Java, Kotlin, Groovy, C#, Scala, Rust, TS/JS | class/type shell plus the listed methods; the rest as signature stubs | the seed method(s) and their calls |

Picker rows map 1:1 to scopes, and the CLI accepts the same targets:

```
[file]   app/Service.java              whole file
[class]  app.Service                   class excerpt, all members
[method] app.Service.used              class shell + call path

filer --root ~/code/demo bundle app/Service.java                       # file
filer --root ~/code/demo bundle 'app/Service.java::Service'            # class
filer --root ~/code/demo bundle 'app/Service.java::Service.used'       # method
filer --root ~/code/demo bundle --at-line 120 app/Service.java         # scope of line 120
```

`--at-line` picks the scope of the line: a method inside a method, the class on a field,
constructor or header, and the whole file on a package/import/blank line before the class.

Function-level languages deliberately have **no `[file]` row**: one file can hold thousands
of functions, so a whole-file row would drown the picker (the CLI still accepts a file path).
Vue files and files without parsed symbols are whole-file. Everything else gains a `[file]`
row, so the whole file is always selectable explicitly.

### Member scopes: class shell + call path

A method seed (`app.Service.used`, `filer bundle 'app/Service.java::Service.used'`, or
`:FilerHere` inside a method) renders, in every language with classes or Rust `impl` blocks:

- the seeded class as a **shell** — package, imports, fields, constructors, annotations —
  with only the methods on the call path in full,
- every other method of that class as a signature stub: `int unused();   // body omitted`,
- the classes it calls at depth 1+ the same way.

It does **not** force the whole file. The `[file]` row / a plain path does that.

The file list names the unit and its kept methods; each block carries the line of the class:

```
- `app/Service.java` :: `Service` [used]
- `app/Repo.java` :: `Repo` [size] (depth 1, call)

### `app/Service.java` :: `Service` (line 5)

public class Service {
    int used() { return repo.size(); }
    int internal();   // body omitted
}

### `app/Repo.java` :: `Repo` (line 3)

public class Repo {
    public int size() { return 0; }
    public int unused();   // body omitted
}
```

Calls are resolved through:

- bare `helper()` — the class, then superclasses/interfaces, then `import static` targets
- `this.helper()` / `super.helper()`
- `local.helper()`, `param.helper()`, `field.helper()` — using the declared type of the receiver
- `Type.helper()` — static calls (including Rust `Self::helper()` and `Type::helper()`)
- `new Type().helper()`
- Rust bare `helper()` / `crate::module::helper()` — unique free functions in the repo
- Scala nullary calls (`repo.size`, no parentheses) count as calls
- Kotlin/C#/TS property access (`repo.Size`) is not treated as a method call, so getters are
  not followed (the property is visible in the shell)

Everything the resolver cannot type (`a.b().c()`, interface dispatch, third-party types) adds
no edge. If the receiver's class is known but the method is not found, the **whole class** is
kept rather than stubbing a method that might be needed. Type-only dependencies (a field of
type `Repo` that is never called) are not bundled; the import line and the field signature
still give the reader the name. Nested classes are rendered inside their enclosing class.

`--depth` counts method hops for method scopes, and unit hops for file/class seeds. A
`[file]` or `[class]` seed uses the **same call graph** for Java, Kotlin, Groovy, C#, Scala,
Rust, TS/JS — not the import/type closure:
you get the files that are actually called, nothing else. `--with-same-package` is a
whole-class expansion and is not applied to that minimal path. A Java file without parsed
symbols (and every other language) keeps the import/type closure.

### Language notes

- TS/JS: type annotations on fields, parameters and locals type receivers, and module imports
  (`./Repo`) resolve classes. JavaScript has no annotations, so only `this.x()` and bare calls
  create edges.
- TS/JS/Vue imports that are not relative go through the nearest `tsconfig.json` /
  `jsconfig.json`: `paths` aliases first, then `baseUrl`. Whatever the config does not map
  stays third-party. See [TypeScript config aliases](#typescript-config-aliases).
- Rust: methods from every `impl Type` block attach to the type; `self`, `self.field`,
  parameter and local types resolve receivers; `Self::`/`Type::` resolve associated calls;
  free functions resolve by unique name across the crate.
- Vue is never sliced: its script offsets are relative to the extracted `<script>` block, so
  it is always bundled whole.

### TypeScript config aliases

`tsconfig.json` and `jsconfig.json` are read as JSONC (comments and trailing commas are
fine), and only what module resolution needs: `compilerOptions.baseUrl`,
`compilerOptions.paths` and relative `extends` chains.

- The **nearest** config for the importing file wins: the one in its directory, else the one
  in the closest ancestor. That is what makes a monorepo work — `web/tsconfig.json` can map
  `@/*` somewhere else than the root config.
- Aliases feed the same probes as relative imports: exact file, `.ts`, `.tsx`, `.js`, `.jsx`,
  `.mjs`, `.cjs`, `.vue`, and `<dir>/index.*`. A `.js` alias target prefers a `.ts` source
  next to it.
- Keys: an exact key wins over a wildcard key, wildcard keys are matched by longest prefix
  (TypeScript precedence), and multiple targets are tried in declaration order.
- Without `paths`, a non-relative import is tried against `baseUrl`
  (`import { x } from "src/lib/x"`).
- Candidates that escape the project root are dropped, and an alias only ever resolves into
  the Web family: pointing one at a `.java` file does not pull a JVM file in.
- Not supported: package-name `extends` (`@tsconfig/node20`, which lives in `node_modules`),
  project `references`, `rootDirs`, and `moduleResolution` variants. Bare specifiers that no
  alias matches (`react`, `@scope/pkg`, `#imports`) stay unresolved, as before.

Configs are re-read on every run, so editing one takes effect without clearing the index
cache.

Excerpts are rendered as `### path :: name (line N)` and contain the declaration plus the
file header (package/imports/`using`s, one time) so they stand alone. Braces left open by
a block namespace are closed at the end.

The seed is the declaration under the cursor: the enclosing method or class, or the whole file
when nothing more specific applies.

## Bundle state

With `state = true` (the default) the binary writes `.filer/bundle.toml` next to the project,
keeping **one bundle per git branch** (`version = 3`). Every entry states its scope:

```toml
# filer bundle state — `:FilerReset` clears the current branch, `:FilerResetAll` everything
root = "/home/me/code/demo"
version = 3

[[bundles]]
name = "feature/checkout"
created = 1793618434
depth = 2

# whole file
[[bundles.entries]]
path = "demo/src/main/java/com/example/demo/RunService.java"
scope = "file"
depth = 0
reason = "seed"

# class excerpt
[[bundles.entries]]
path = "demo/src/main/java/com/example/demo/DemoApplication.java"
scope = "symbol"
symbol = "DemoApplication"
depth = 1
reason = "same-package"

# class shell + kept methods
[[bundles.entries]]
path = "demo/src/main/java/com/example/demo/Service.java"
scope = "methods"
symbol = "Service"
methods = ["used", "internal"]
depth = 0
reason = "seed"
```

Rules:

- `file`: `path` only.
- `symbol`: `path` + `symbol`.
- `methods`: `path` + `symbol` + a non-empty `methods` list. One entry per class, even when
  methods arrive at different depths; `depth` is the minimum and `reason` is `seed` if any
  method is a seed, otherwise the reason of the minimum-depth method.

Version 2 files (`[[bundles.files]]`, implicit scope, one entry per `member`) and version 1
files (one bundle at the top level) are migrated on load: `path` → `file`,
`path` + `symbol` → `symbol`, and `member` entries are grouped per class into one `methods`
entry with the union of methods. A v2 method **seed** was stored as a whole-file entry
(that was the old stop-gap), so it cannot be recovered and migrates to `file`; the new v3
state records `methods` from the start.

What that buys you:

- The picker shows the **current branch's** bundled files as
  `✓ [class] com.example.demo.DemoDocument  (depth 1, same-package)`. Selecting one is a
  no-op instead of a duplicate.
- The next bundle is a **superset**: this branch's previous files are kept, new picks are
  added, depths are the minimum over both runs.
- Switching branches keeps everything: each branch picks up its own bundle where it left
  off, instead of losing it.
- The bundle ends with a `## Bundle state` section holding that branch's TOML verbatim —
  data only, no filer instructions or prose — so the model can see what came in and why.
  (The `.filer/bundle.toml` sidecar keeps its one-line comment; that file never goes to
  the chat.)
- `reason` is one of `seed`, `import`, `static-import`, `wildcard-import`, `module-import`,
  `same-package`, `usage`, `package`, `config`, `external`.

Cleaning up:

| Situation | What to do |
| --- | --- |
| New ticket, fresh bundle | `:FilerReset` — clears the current branch only |
| Nuke every branch | `:FilerResetAll` (or `filer state --clear --all`) |
| Clear a different branch | `filer state --clear --branch main` |
| Want a one-off bundle, no state | `opts.state = false`, or `filer bundle` without `--state` |
| See what is stored | `filer state` (add `--branch NAME` for another branch) |
| Reopen yesterday's bundle | `:FilerShow` / `:FilerShow main` / `filer show --branch main` |

`.filer/.gitignore` containing `*` is created automatically, so the state never shows up in
`git status` and your own `.gitignore` is left alone. Next to it, `.bundle.toml.lock` is the
advisory write lock (kept on purpose; the kernel releases the lock when the process exits). Branch names come straight from
`.git/HEAD` (no git subprocess); detached HEADs are stored as `detached@<sha>`, and repos
without git use a single `(no branch)` bundle. Older single-branch state files are migrated
on load.

## Deltas: only what is new

An agent that already pasted half the closure into its context does not need it again. Two
flags describe what the caller already holds, at the same granularity the bundle uses:

```sh
# the units already in context: path, path::Symbol or path::Class.method, repeatable
filer --root ~/code/demo bundle --skip 'app/Repo.java::Repo.size' app/Service.java

# a manifest instead: `-` for stdin, `@FILE` or a bare path
filer --root ~/code/demo bundle --only-new @manifest.txt app/Service.java
filer --root ~/code/demo --json bundle --only-new - app/Service.java < manifest.txt
```

The manifest accepts two formats, picked by the first character:

- JSON (`{`/`[`): `bundle --json` output, or just its `entries` array. `scope` decides the
  granularity, so a `methods` entry covers exactly the listed methods.
- Anything else: one `path`, `path::Symbol` or `path::Class.method` per line. Blank lines and
  `#` comments are skipped, whitespace and CRLF are trimmed (`--seeds-from` rules).

```sh
# pipe a bundle straight back in; with everything covered the answer is "nothing new"
filer --root ~/code/demo --json bundle --only-new @manifest.json app/Service.java
```

Rules:

- `--skip` and `--only-new` filter the **output**, not the resolution: a skipped unit stays in
  the graph, so what it pulls in can still be new.
- Coverage is coarse on purpose. A bare path covers everything in that file, a symbol covers
  its methods too, and a method covers exactly that method.
- An empty manifest is a plain bundle. A manifest that covers everything is `nothing new`
  (exit `4`, `empty_result`, with a hint), never an error.
- The saved state file keeps the **full** bundle: a delta run with `--state` records what
  filer resolved, not just the part that was new.

## Order and budget

The bundle has one documented order, and it is the same order the budget truncates:

1. **Seeds** first, in the order you gave them on the command line.
2. **Scope**: whole files (`file`) before class/declaration excerpts (`symbol`) before
   method scopes (`methods`). Complete files are the most useful thing in a slice, so they
   survive a tight budget longest.
3. **Ascending depth** within a scope, so a shallower dependency beats a deeper one.
4. **Reason**: `seed` before `call` before the import reasons (`import`, `static-import`,
   `wildcard-import`, `module-import`), then `same-package`/`usage`, then `package`,
   `config`, `external`.
5. **Path**, then **span** start, as the final tie-breakers.

Nothing depends on discovery order, so reordering imports in a seed file cannot reorder the
bundle, and the same seed with the same budget renders the same bytes twice in a row. The
state file is written in this order too.

### Budgets

```sh
filer --root ~/code/demo bundle --max-tokens 2000 app/Service.java
filer --root ~/code/demo bundle --max-bytes 8000 app/Service.java   # the primitive
filer --root ~/code/demo show --max-tokens 2000                     # re-render under a budget
```

- `--max-bytes` is the primitive: total **source** bytes of the bundled spans.
- `--max-tokens` is a convenience for `--max-bytes`, derived as `tokens * 4` (one token is
  four source bytes, documented in `bundle::BYTES_PER_TOKEN`). **There is no tokenizer
  dependency**, so the number is an estimate; `--max-bytes` is exact.
- Both are unlimited by default; the state file always keeps the **full** selection, so the
  budget only shapes what is rendered now and raising it later brings entries back.
- Truncation is strict: the last unit that still fits survives, and everything after it in
  the canonical order is dropped. A smaller budget can only remove entries from the tail of
  the order above — it never reshuffles or reorders the rest.
- Dropped units are not rendered and not listed in `## Files`; the JSON envelope lists every
  one in `omitted` with `"reason": "budget"` and reports `budget.truncated: true`. In text
  mode a one-line warning on stderr says how many units were cut.

A budget does not fight the per-file safety valve: a single file above 1 MiB is still
reported as `too_large` instead of being cut.

### Sessions: one state file per agent

The default state is per repo and per branch (`.filer/bundle.toml`). Two agents — or one
agent and your editor — sharing a checkout would then clobber each other's context, so each
can take a session instead:

```sh
filer --root ~/code/demo bundle --session s1 --depth 2 'app/Service.java::Service.used'
filer --root ~/code/demo show   --session s1
filer --root ~/code/demo state  --session s1
FILER_SESSION=s1 filer --root ~/code/demo bundle --depth 2 app/Service.java   # same thing
```

- A session lives in `.filer/sessions/<id>.toml` — **repo-local**, so it travels with the
  checkout and dies with it, but never shows up in `git status`: the state writer drops the
  same `*` `.gitignore` into `.filer/` that keeps `bundle.toml` out of git.
- `--session` (global) switches the state file for `bundle`, `show`, `state`, `why` and
  `search --state`; an explicit `--state` still wins. `$FILER_SESSION` is used when the flag
  is absent, so a harness can export `PI_SESSION_ID` (or any id) once instead of threading a
  flag through every call.
- Ids are file names: letters, digits, `.`, `-`, `_` only, no `/`, no `.`/`..`. Anything else
  exits `1` with a hint, so a session id can never escape `.filer/sessions/`.
- Branches still work inside a session: the file holds one bundle per git branch, like the
  default state. `--from-state` refreshes a session's seeds the same way.
- `filer state` keeps working unchanged for the default file — sessions are additive.

### `context list` / `context clear`

What is saved on disk, without reading TOML by hand:

```sh
filer --root ~/code/demo context list
# default           2 entries on main  .filer/bundle.toml
# s1                7 entries on main  .filer/sessions/s1.toml

filer --root ~/code/demo --json context list
filer --root ~/code/demo context clear s1      # forget one session
filer --root ~/code/demo context clear --all   # forget every session and the default state
```

`--json` returns one object with `schema_version`, the current `branch`, a `default` entry and
a `sessions` array; each carries `path`, `exists`, `entries` (for the current branch),
`modified` (unix seconds) and a `branches` array with per-branch `entries`/`depth`/`created`.
`context clear` prints what it removed and is a no-op (exit `0`) when there was nothing.

### Refreshing after an edit

A stateful bundle is a **union** of every run, which is what a human picker wants and what an
agent must *not* get after editing a file: the old entries would never leave. `--from-state`
re-resolves the saved bundle's seeds (`reason = "seed"`) and rebuilds the rest of the closure
from the current sources:

```sh
filer --root ~/code/demo bundle --state .filer/s1.toml --depth 2 'app/Service.java::Service.used'
# ... edit Service.java: call another method, delete one ...
filer --root ~/code/demo bundle --from-state --state .filer/s1.toml --depth 2
```

- Derived entries are recomputed, never merged in: a new call shows up as a new callee, a
deleted method drops out.
- Seeds are the only input, so no file arguments are needed (`--from-state` implies the
default `.filer/bundle.toml` when `--state` is omitted). Extra files on the command line are
added to the recovered seeds.
- A seed that no longer exists is reported on stderr and dropped; if no seed survives, the
run exits `4` and leaves the state file untouched instead of writing an empty bundle.
- `--from-state` conflicts with `--reset`, which already means "start over".
## CLI

```sh
filer --root ~/code/demo search --query user           # JSON lines, one per symbol
filer --root ~/code/demo search --query user --plain   # fqn<TAB>path[::symbol]
filer --root ~/code/demo search --query 'discount rounding' --json  # empty -> hint
filer --root ~/code/demo deps src/main/java/com/example/app/Application.java
filer --root ~/code/demo deps 'app/Service.java::Service.used' --unit
filer --root ~/code/demo bundle --depth 2 --with-config \
      src/main/java/com/example/app/Application.java > bundle.md
filer --root ~/code/demo bundle 'src/main/java/app/Service.java::Service.usedEverywhere'
filer --root ~/code/demo bundle --at-line 120 src/main/java/app/Service.java
filer --root ~/code/demo index                         # cache stats
filer --root ~/code/demo index --update app/Service.java  # refresh one file's cache entry
filer --root ~/code/demo show                          # re-render this branch's bundle
filer --root ~/code/demo show --branch main            # ... another branch's
filer --root ~/code/demo show --out bundle.md           # ... into a file
filer --root ~/code/demo callers 'app/Service.java::Service.used' --depth 2 --json
filer --root ~/code/demo why 'app/Repo.java::Repo.size' --json
filer --root ~/code/demo refs TaxRate --json
filer --root ~/code/demo state                         # what is stored per branch?
filer --root ~/code/demo state --clear                 # forget this branch
filer --root ~/code/demo state --clear --all           # forget everything

# stateful bundling: bundle twice, get the union
filer --root ~/code/demo bundle --state ~/code/demo/.filer/bundle.toml A.java > a.md
filer --root ~/code/demo bundle --state ~/code/demo/.filer/bundle.toml B.java > ab.md
filer --root ~/code/demo bundle --state ~/code/demo/.filer/bundle.toml --reset C.java > c.md

# seeds from a list instead of argv: one path per line, `#` comments and blank
# lines skipped, so long lists never hit the argv limit
rg -l 'discount' | filer --root ~/code/demo bundle --seeds-from - --depth 1 > found.md
filer --root ~/code/demo bundle --seeds-from @seeds.txt --depth 2 > seeds.md

# after an edit: re-resolve the seeds instead of unioning (files are optional)
filer --root ~/code/demo bundle --from-state --state ~/code/demo/.filer/s1.toml --depth 2
```

`bundle` flags: `--depth N` (0 = only the given units), `--with-same-package`,
`--with-config`, `--title`, `--out FILE`, `--state FILE`, `--reset`, `--from-state`,
`--seeds-from -|@FILE` (repeatable), `--at-line N`, `--max-bytes N`, `--max-tokens N`
(see [Order and budget](#order-and-budget)), `--skip UNIT`, `--only-new -|@FILE`
(see [Deltas: only what is new](#deltas-only-what-is-new)), `--max-file-bytes N`
(see [Config file](#config-file)). `show` takes the same budget and per-file flags.
With `--seeds-from` the positional files become optional and both sources are
combined; reading that list is a plain file read — filer never runs `git`, so
`git diff --name-only | filer bundle --seeds-from -` is your own pipeline.
Default depth is 2. Global flags: `--json`, `--quiet`, `--include GLOB`, `--exclude GLOB`,
`--no-ignore`, `--session ID` (see below and [Sessions](#sessions-one-state-file-per-agent)),
`--roots DIR[,DIR]` (see below).
Add `::Symbol` for one declaration and `::Class.method` for a Java or Rust method scope:
`filer --root ~/code/demo bundle 'app/Service.java::Service.used'`,
`filer --root ~/code/demo bundle 'src/app/service.rs::Service.used'`.
`--with-config` adds, when they exist: `pom.xml`, the Gradle files, `build.sbt` /
`project/*.sbt`, `Cargo.toml`, `tsconfig.json` / `jsconfig.json` / `package.json`,
`Directory.Build.props` and root-level `*.csproj` / `*.sln`.

Every indexed language works the same:

```sh
filer --root ~/code/demo bundle --depth 2 src/main/scala/com/example/app/Application.scala
filer --root ~/code/demo bundle --depth 2 src/main/kotlin/com/example/App.kt
filer --root ~/code/demo bundle --depth 2 src/app/service.rs
filer --root ~/code/demo bundle --depth 2 src/web/app.ts
```

`show` takes no files: it reads the state file (default `<root>/.filer/bundle.toml`), picks the
current branch's bundle (`--branch` overrides), re-reads the files from disk and renders it
again — useful after a restart, or to pipe a bundle into another tool. Files or excerpts
that vanished since are dropped with a notice; asking for a branch that has no bundle lists
the ones that do.

### Callers, why, and refs

The three reverse queries work on the same index and call graph as `bundle`; none of them
searches file text.

```sh
# Who calls this unit? Depth 1 is direct callers, depth 2 follows callers of callers.
$ filer --root ~/code/demo callers 'app/Repo.java::Repo.size' --depth 2 --json
[{"caller":"app/Service.java::Service.used","line":5,"depth":1},
 {"caller":"app/Controller.java::Controller.handle","line":5,"depth":2}]

# How did this unit get into this branch's saved bundle? The chain starts at the seed;
# each hop's line is the call site in the previous hop.
$ filer --root ~/code/demo why 'app/Repo.java::Repo.size' --json
{"target":"app/Repo.java::Repo.size","reason":"call","depth":2,
 "chain":[{"unit":"app/Controller.java::Controller.handle","line":null,"reason":"seed",...},
          {"unit":"app/Service.java::Service.used","line":5,"reason":"call",...},
          {"unit":"app/Repo.java::Repo.size","line":5,"reason":"call",...}]}

# Invert the recorded name uses and calls. `kind` is `use` or `call`.
$ filer --root ~/code/demo refs TaxRate --json
[{"path":"app/Service.java","symbol":"Service","member":null,"line":2,"kind":"use"}]
```

`deps` lists the import/call closure of one seed. By default every row is a file, even
when the seed is a method; add `--unit` to keep the units (`path::Symbol`,
`path::Class.method`) the traversal actually discovered. Both modes run the same
breadth-first walk, so the unit list is the file list differ-by-unit, not a second
resolver. `deps --json` also carries the materialized `edges` and the `diagnostics` for
the files it touched.

`callers` follows call edges only (`Reason::Call`); an imported type is not a caller.
`why` reads the state file (`--state`, default `<root>/.filer/bundle.toml`) and the current
branch (`--branch`), walks `graph.inc` back to a `reason = "seed"` entry, and returns the
forward chain. If the unit sits inside a whole-file or whole-class seed, the chain stops at
that seed.

`refs` is an **edge inversion**, not grep. It reports what the resolver already recorded in
`Symbol.uses`, `Symbol.calls` and `Member.calls`, so body text, comments and string literals
are deliberately out of scope. A `call` row carries the call-site line; a `use` row carries
the enclosing declaration's line because uses are stored per symbol, not per occurrence.

Empty results print `[]` with `--json` and exit `4`, like `search`.

### JSON surface

The JSON form is the stable product and the surface an agent drives; markdown is one
consumer of the same data. Every JSON output covered here (`search`, `deps`, `bundle`,
`show`, `index`) carries `schema_version: 1`:

```sh
filer --root ~/code/demo --json search --query Service   # JSON lines, one row per unit
filer --root ~/code/demo --json deps app/Service.java    # {"schema_version":1,"seed":...,"deps":[...],"edges":[...],"diagnostics":[...]}
filer --root ~/code/demo --json deps 'app/Service.java::Service.used' --unit
filer --root ~/code/demo --json bundle --depth 2 app/Service.java
filer --root ~/code/demo --json show
filer --root ~/code/demo --json index
```

`bundle --json` and `show --json` emit one envelope per run:

```json
{
  "schema_version": 1,
  "root": "/repo",
  "title": "demo",
  "branch": "main",
  "depth": 2,
  "files": 2,
  "excerpts": 2,
  "methods": 1,
  "budget": {
    "max_file_bytes": 1048576,
    "max_bytes": 8000,
    "max_tokens": 2000,
    "used_bytes": 4096,
    "used_tokens": 1024,
    "truncated": false
  },
  "entries": [
    {
      "path": "app/Service.java",
      "scope": "methods",
      "symbol": "Service",
      "methods": ["used"],
      "depth": 0,
      "reason": "seed",
      "start_line": 3,
      "end_line": 14,
      "start_byte": 14,
      "end_byte": 186,
      "tokens": 43,
      "rendered": true,
      "label": "`app/Service.java` :: `Service` [used]",
      "markdown": "\n### `app/Service.java` :: `Service` (line 3)\n\n```java\n...\n```\n"
    }
  ],
  "omitted": [],
  "diagnostics": [
    {"kind": "unresolved-import", "path": "app/Service.java", "import": "com.acme.Tax"}
  ],
  "coverage": {"files_indexed": 812, "files_skipped": 34, "imports_unresolved": 19},
  "state_toml": "version = 3\n..."
}
```

- `entries` names exactly the units the markdown contains, in order; `label` is the
  `## Files` line and `markdown` is the `## Contents` section, so a consumer can rebuild
  the bundle from the JSON. `omitted` lists the units that are not in it: either selected
  but unrenderable (`too_large`, `unreadable`, `symbol_missing`, `stale_span`, `not_indexed`)
  or cut by the budget (`budget`). Budget cuts are not in `entries`, so `entries[].tokens`
  always sums to `budget.used_tokens`.
- `budget.max_bytes` / `budget.max_tokens` are `null` when no budget was given;
  `used_tokens` is the sum of `entries[].tokens`, and `truncated` is true when the budget
  cut units or a file hit `max_file_bytes`. See [Order and budget](#order-and-budget).
- Spans are 1-based lines and 0-based byte offsets. `tokens` is `bytes / 4` for the unit
  span; there is no tokenizer dependency.
- `state_toml` preserves the bundle-state appendix as TOML inside the JSON envelope, so
  the semantic state stays readable in the format the sidecar uses.
- `diagnostics` lists what the traversal declined to include for **this** bundle (see
  [Diagnostics and coverage](#diagnostics-and-coverage)); `show` does not resolve, so its
  array is empty.
- `coverage` is index-wide, so it describes how complete the tool's view of the repo is:
  `files_indexed`, `files_skipped` (supported-language files that could not be read or
  parsed) and `imports_unresolved` (import statements no indexed file provides). The
  per-unit version of the last number is `diagnostics`.
- `deps --json` deliberately **wraps** its array: it used to print a bare `[...]`, now it
  prints `{"schema_version":1,"deps":[...]}` so the version is a top-level key. Update
  consumers with `jq '.deps'`. `search` stays JSON lines and adds `schema_version` to each
  row, which is additive for the Neovim picker.
- `deps --json` additionally carries `seed` (canonical unit label), `granularity`
  (`file` or `unit`, matching `--unit`), `edges` (`{from, to, reason, line}` with canonical
  unit labels; `line` is `null` when the edge has no single call site) and `diagnostics`.
  Each `deps` row keeps `path`, `fqn`, `depth`, `reason` and adds `unit`, `scope`,
  `symbol`, `member`. The fields are additive, so `schema_version` stays `1`.

```json
{
  "schema_version": 1,
  "seed": "app/Pricing.java::Pricing.round",
  "granularity": "unit",
  "deps": [
    {"path": "app/Repo.java", "fqn": "app.Repo", "depth": 1, "reason": "call",
     "unit": "app/Repo.java::Repo.size", "scope": "methods", "symbol": "Repo", "member": "size"}
  ],
  "edges": [
    {"from": "app/Pricing.java::Pricing.round", "to": "app/Repo.java::Repo.size",
     "reason": "call", "line": 10}
  ],
  "diagnostics": [
    {"kind": "unresolved-import", "path": "app/Pricing.java", "import": "com.acme.Tax"}
  ]
}
```

### Diagnostics and coverage

Everything filer declines to include used to disappear. A precise tool that silently drops
half a slice is worse than a coarse one, so the JSON surface reports the gaps: markdown
`bundle` warns with a diagnostic count, `deps` prints each diagnostic, and `bundle --json` /
`deps --json` carry the full list. `--quiet` silences the warnings, not the JSON.

`kind` is a **closed enum**. Each kind carries a fixed payload field in addition to `kind`
and `path`:

| `kind` | Payload | Meaning |
| --- | --- | --- |
| `unresolved-import` | `import` | No indexed file provides the import (JDK, stdlib, npm, NuGet, ...). |
| `unknown-receiver` | `line`, `call` | A call through an arbitrary expression (`a.b().c()`), whose receiver type is not known. The parsers record the call name, not the expression. |
| `unresolved-call` | `line`, `name` | The receiver/type name resolved to nothing. |
| `method-not-found` | `line`, `name` | The class is known but declares no such method; the whole class is kept instead. |
| `type-only-skipped` | `symbol` | A type in scope that a call-path bundle deliberately skips. |
| `ambiguous` | `line`, `name` | Several same-named candidates; filer refuses to guess. |

New kinds are a wire-format change. Diagnostics are sorted (path, line, kind, name) and
deduplicated so repeated seeds and expansions do not multiply them.

Rebuilding the markdown from the envelope:

```sh
jq -jr '"# Code bundle: \(.title)\n\n- root: `\(.root)`\n- files: \(.files)\n"
  + (if .excerpts > 0 then "- excerpts: \(.excerpts)\n" else "" end)
  + (if .methods > 0 then "- methods: \(.methods)\n" else "" end)
  + "- import depth: \(.depth)\n\n## Files\n\n"
  + ([.entries[] | "- " + .label] | join("\n"))
  + "\n\n## Contents\n"
  + ([.entries[] | .markdown] | join(""))
  + (if .state_toml then "\n## Bundle state\n\n```toml\n" + (.state_toml | rtrimstr("\n")) + "\n```\n" else "" end)' bundle.json
```

### Machine-readable errors and exit codes

`--json` (global) prints a fatal error as one envelope on stderr instead of prose:

```json
{"error":{"code":"symbol_not_found","message":"app/Service.java has no symbol Nope","hint":"pass a path alone to bundle the whole file"}}
```

Soft notices (dropped entries, skipped inputs, unresolved imports) become
`{"warning":"..."}` lines on stderr, so an agent never has to parse English.

Exit codes are stable:

| Code | Meaning | Examples |
| --- | --- | --- |
| `0` | success | rows printed, bundle rendered |
| `1` | internal | unreadable/unwritable file, usage error |
| `2` | not found | `symbol_not_found`, `file_not_found`, `branch_not_found`, `state_not_found` |
| `3` | not indexed | `not_indexed` (binary, skipped directory or unsupported language) |
| `4` | empty result | no search hits, nothing to bundle, empty saved bundle |

An empty `search` result exits `4`. With `--json` it also prints a hint line that names
what the index does *not* look at:

```sh
$ filer --root ~/code/demo search --query 'discount rounding' --json
{"hint":"symbol/path index only; use text search for file contents","results":0,"schema_version":1}
```

The picker scores FQN, relative path and symbol/member names only — never fields,
parameters, locals, imports, comments or bodies. File contents are not searched.

`--quiet` (global) suppresses the trailing `filer: N units, M new on this run` summary,
the `dropping ...` notices and the diagnostic notes (unresolved imports, unknown
receivers, skipped type-only dependencies, ...). Hard errors still print. A successful
`bundle --quiet` writes nothing to stderr; the same information stays in `bundle --json`.

### What gets indexed: ignore files and globs

Every command builds the same index, so the filter is global:

- `.gitignore`, `.git/info/exclude`, the global `core.excludesFile` and `.ignore` files are
  honored by default — a plain directory works too, `git` is not required and nothing is
  shelled out. `--no-ignore` turns that layer off for one run. The build/cache directories
  below stay skipped either way.
- `--include GLOB` / `--exclude GLOB` (repeatable) narrow the view further:
  - a glob containing `/` is matched against the project-relative path and is anchored at
    the root: `--include 'src/**'`, `--exclude '**/legacy/**'`;
  - a glob without `/` is matched against the file name at any depth, like the same line in a
    `.gitignore`: `--include '*.kt'`, `--exclude '*.gen.java'`;
  - `*` stays inside one path segment (`src/*.java` does not reach `src/app/Service.java`),
    `**` crosses segments, and `--exclude` wins over `--include`;
  - globs filter files, never directories: nothing is pruned behind your back.
- The filter is part of the index, not a post-filter. `search` shows the filtered view,
  `deps`/`callers`/`why` resolve only inside it, and a hidden path is `not_indexed` (exit 3).
  `bundle` still bundles an explicitly named filtered-out file — argv is the caller's
  literal input — and warns that the file is outside the index.
- The cache follows: a path leaves the index as soon as it is ignored or excluded, while its
  parse memo survives if the file still exists, so un-ignoring it costs no re-parse.


### Config file

`.filer/config.toml` holds the project's defaults, so an agent and a human stop
disagreeing about what "context" means. Every key is optional and the precedence is
always **flag > config file > built-in**:

```toml
[bundle]
depth = 2                 # --depth; built-in 2
max_file_bytes = 1048576  # --max-file-bytes / show --max-file-bytes; built-in 1 MiB

[index]
roots = ["src", "lib"]    # --roots DIR[,DIR]; built-in: the whole root
include = ["**/*.java"]   # --include; built-in: every supported file
exclude = ["**/generated/**"]  # --exclude; built-in: nothing
ignore = true             # --no-ignore still wins; built-in true
```

- A flag **replaces** the config list instead of appending to it: `--exclude 'x'` with
  `exclude = ["y"]` in the file leaves only `x` excluded. That keeps "what you asked for"
  and "what the file asks for" separable, and makes the precedence testable.
- `roots` must exist inside the project root; a missing or escaping root is an error, not a
  silently empty index.
- Unknown keys are an **error** (`depht = 2` fails with the file path and the known keys),
  because a typo that silently changes the bundle is exactly what this file prevents.
- The file is read from `<root>/.filer/config.toml` — the same directory as the state — so a
  session in any worktree picks up its own defaults.
- `filer show` re-renders with the same `[bundle] max_file_bytes`, so a reopened bundle looks
  like the one that was written.

### Incremental index refresh

Every command builds the index by walking the tree; the mtime+size cache makes that cheap,
but an agent that just edited five files does not need to walk anything at all:

```sh
filer --root ~/code/demo index --update src/app/Service.java src/app/Repo.java
git diff --name-only | filer --root ~/code/demo index --update -    # one path per line
```

`--update` re-parses only the named paths (or reuses their memo when nothing changed), drops
the entries of named paths that are gone, and writes the cache atomically. Everything else in
the cache is left alone, so the next full walk reuses the result. It prints
`parsed/reused/removed/skipped` counts plus the cache size — as JSON with `--json`, e.g.
`{"updated":{"parsed":1,"reused":0,"removed":0,"skipped":0},"entries":42}`. Paths outside the
root, non-source files and unreadable files are skipped with a warning instead of failing the
run. `filer index --update` never resolves, bundles or touches the bundle state.

## How resolution works

1. Walk the tree, skipping `.git`, `.gradle`, `.idea`, `.filer`, `.venv`, `.next`, `.nuxt`,
   `.output`, `.turbo`, `.svelte-kit`, `target`, `build`, `out`, `bin`, `dist`, `coverage`
   and `node_modules`, plus whatever `.gitignore`/`.ignore` hides (see above; `--no-ignore`
   opts out).
2. Parse each file with the tree-sitter grammar for its extension (`src/lang.rs`; Vue uses
   the TypeScript grammar on its `<script>` blocks). Java types are
   classes/interfaces/enums/records/annotations; Scala adds objects, traits and top-level
   type aliases; Kotlin adds objects and type aliases; Groovy adds traits; C# adds structs,
   records and delegates; Rust adds structs, enums, unions, traits, type aliases and free
   functions, with `impl` methods attached to their type; TS/JS add interfaces, type aliases
   and enums. Sequential and braced package/namespace clauses compose; `package object foo`
   counts as package `a.foo`; C# file-scoped namespaces apply to the rest of the file.
3. Build `fqn -> files`, `package -> files`, simple-name and `symbol -> (file, index)` maps.
   A name can exist in several languages, so lookups are filtered by language family:
   JVM-ish (Java, Scala, Kotlin, Groovy, C#) only resolve against each other, TS/JS/Vue only
   against each other, and Rust only against Rust.
4. For a package-style import, try the full name, then trim trailing segments one at a time
   (`a.b.C.member` → `a.b.C`), which also covers nested types like `a.b.C.Inner` and Scala
   selectors / `given` imports. Wildcards expand to the package, or to the object/type they
   name (`import a.b.C._`); C# `using Some.Namespace;` is a package wildcard. Anything
   unresolved is JDK, stdlib, npm, NuGet or another third party.
5. For a TypeScript/JavaScript module import, join the specifier onto the importing file's
   directory, reject paths that escape the root, then probe in order: the exact path, TS
   extensions (`.ts`, `.tsx`, then JS `.js`, `.jsx`, `.mjs`, `.cjs`, then `.vue`), and
   `<dir>/index.*`. A `.js` specifier prefers the matching `.ts`/`.tsx` source. A
   non-relative specifier goes through the nearest `tsconfig.json`/`jsconfig.json` first
   (`paths` aliases, then `baseUrl`; see
   [TypeScript config aliases](#typescript-config-aliases)); whatever the config does not map
   (`react`) is treated as third-party.
   For a Rust import, expand the `use` tree, then resolve the longest module prefix against
   the Cargo layout: `crate::` starts at the nearest `src` directory, `self::` at the current
   module directory, `super::` one level up, and `mod foo;` uses the same relative lookup.
   Each module is `foo.rs` or `foo/mod.rs`; `lib.rs`/`main.rs`/`mod.rs` are the module root.
   Unresolved prefixed paths (`std::...`, third-party crates) stay external.
6. For every type name used in the file (Java/Scala type positions, Kotlin user types,
   Groovy/C# type positions, Rust type positions and `Type::method` paths, object receivers
   like `DemoDocument.create`) look the name up in
   the index. Names that are explicitly imported are left to step 4; a same-package match
   wins; otherwise the name is only used when it is unique in the repo. Ambiguous names are
   dropped rather than guessed.
7. For a symbol seed, do the same with the names used *inside that symbol*, and map each hit
   to the symbol that declares it (a sibling in the same file first). A whole-file unit that
   ends up in the same bundle supersedes its symbol units.
8. Breadth-first walk from the seeds up to `--depth`. The walk records depth and reason per
   unit; the bundle then sorts them into the [canonical order](#order-and-budget) (seeds,
   scope, depth, reason, path, span) instead of rendering discovery order, which is what
   makes the output reproducible. Materialized edges are kept in the graph, so `deps --json`
   can expose them and `callers`/`why` can walk them backwards.
9. While resolving, record what was declined (unresolved imports, unknown receivers,
   method-not-found fallbacks, skipped type-only deps, ambiguities) as diagnostics. They
   travel with the closure, not with the rendered markdown, so `bundle --json` can report
   gaps in the slice it just produced.

## Adding another language

`src/lang.rs` holds the language-neutral `Source` struct (`package`, `types`, `imports` with
an `ImportKind`, `uses`, `symbols` with byte spans, `preamble_end`), the `Language` →
extension map, and the `Language::family` grouping that keeps resolution between compatible
languages only. Each per-language module (`java`, `scala`, `kotlin`, `groovy`, `csharp`,
`rust`, `web`) only translates its grammar into that struct.

To add another package+import language:

1. Add a parser module that returns `Source`.
2. Add an arm to `Language::from_path` and to the parser dispatch.
3. Pick the markdown fence string.

Indexing, closure, state and rendering need no changes. A language with a different import
model (TS/JS module paths, Rust's `crate`/`self`/`super` paths) needs its own resolver branch
in `src/resolve.rs` — that is where `ImportKind::Module` and `Family::Rust` are handled.

Populating `symbols` is optional: use `symbol_from_node` for a tree-sitter declaration (it
extends the span over the doc comment and records the line) and `finish_symbols` to assign
per-symbol uses. Return no symbols and the language keeps whole-file bundling.
