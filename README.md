# filer

Select Java, Scala, Kotlin, Groovy, C#, Rust, TypeScript, JavaScript, Vue or Asset Control
Formula Engine (`.fe`) code in Neovim, get one pastable markdown blob for your chat LLM —
with the files your selection imports, and the files those import, already included.

For `.fe` files the unit is the **function** you picked: `main.fe::riskValue` pulls in that
function and the functions it calls, not the whole 20 000-line file. For Java a method can be
the entry point too: the **file you seed stays complete**, and every file pulled in at
depth 1+ keeps its class shell (package, imports, fields, constructors) with only the methods
on the call path; everything else becomes a signature stub.

You have a chat interface but no agentic workflow. `filer` covers the boring part: finding
the three or four files that belong together and formatting them so the model sees the
whole picture instead of a fragment.

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

- Indexes `.java`, `.scala` / `.sbt` / `.sc`, `.kt` / `.kts`, `.groovy` / `.gradle`, `.cs`,
  `.rs`, `.ts` / `.tsx`, `.js` / `.jsx` / `.mjs` / `.cjs`, `.vue` and `.fe` files under the
  project root (tree-sitter based except `.fe`, which has a small hand-written parser; no LSP,
  no classpath, no compiler).
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
- Follows type *usages* too, so a class that references a sibling in its own package — which
  Java, Scala, Kotlin, Groovy or C# needs no import for, e.g. `indexOps(DemoDocument.class)`,
  `DemoDocument.create()` or `new DemoDocument()` — still pulls that file in. Rust type
  positions and `Type::method()` receivers work the same way. Module languages rely on
  their explicit imports instead of repo-wide name guessing.
- Extracts **symbols** (top-level functions, classes, objects, global declarations, Rust
  structs/enums/traits/functions, Java methods) with their byte spans, so a bundle can carry
  one function out of a huge file.
  `.fe` functions are the primary use case: they call each other across a project-wide
  namespace, exactly like the language's IntelliJ plugin resolves them. Class methods add a second one: class shell for context, call-path bodies for content.
- Every entry has an explicit **scope** — `file`, `symbol` or `methods` — so the picker,
  the state file and the renderer agree on what was included and why.
- Walks the resulting graph to a configurable depth.
- Remembers what is already bundled in a TOML state file, marks those entries with `✓` in
  the picker, and grows the bundle instead of replacing it. Switching git branch kills the
  old state automatically.
- Renders the selection as markdown: header, file list with import depths, then one fenced
  block per file or excerpt. Fences grow automatically if a file already contains backticks.
- Caches the index in `~/.cache/filer/`, keyed by root path and invalidated per file by
  mtime + size. Re-indexing a 2000-file project takes ~40 ms, so the picker re-queries on
  every keystroke.

Things it deliberately does not do: compile your project, read `pom.xml` / `build.sbt` /
`*.csproj` / `Cargo.toml` / `package.json` dependencies, or pretend to resolve libraries. JDK,
Scala stdlib, npm packages, NuGet packages, Rust crates and other third-party imports are
simply reported as unresolved (`tsconfig` path aliases are not read). Imports only resolve
within the same language family (JVM-ish languages with each other, TS/JS/Vue with each
other, Rust with itself, FE with itself), never across.

## Build

```sh
cargo build --release     # binary at target/release/filer
# optional, to use it outside Neovim too:
cargo install --path .
```

Requires a C compiler for the tree-sitter grammars (`gcc`/`clang`).

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
  cmd = { "FilerSearch", "FilerHere", "FilerBundle" },
  keys = {
    { "<leader>fc", function() require("filer").search() end, desc = "filer: bundle for chat" },
    { "<leader>fh", function() require("filer").here() end, desc = "filer: bundle current file" },
    { "<leader>fC", function() require("filer").bundle({ vim.api.nvim_buf_get_name(0) }, { depth = 0 }) end, desc = "filer: bundle current file only" },
  },
  opts = {
    bin = nil,            -- $PATH (the release installer); set a path if not installed
    root = nil,           -- detect via .git / pom.xml / build.gradle(.kts) / build.sbt / ...
    depth = 2,            -- import hops to follow
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
| `:FilerHere` | Bundles the current buffer with its closure. In a `.fe` file the function under the cursor is the seed; in Java, Kotlin, Scala, Groovy, C#, Rust and TS/JS, the method under the cursor (or its class); elsewhere the whole file. |
| `:FilerBundle {files}` | Bundles the given files (relative to root or absolute) or excerpts (`path::Symbol`). |
| `:FilerShow [branch]` | Re-renders the saved bundle (current file contents) without re-resolving. |
| `:FilerReset` | Forgets this branch's bundle; the next bundle starts empty. |
| `:FilerResetAll` | Forgets every branch's bundle. |

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
| `file` | all but `.fe` | the whole file | Java/Rust: the calls of every method or function in the file; others: the import/type closure |
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

`.fe` is function-granular and deliberately has **no `[file]` row**: one `.fe` file can hold
thousands of functions, so a whole-file row would drown the picker (the CLI still accepts a
file path). Vue files and files without parsed symbols are whole-file. Everything else gains
a `[file]` row, so the whole file is always selectable explicitly.

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

### Other languages

- `.fe` has no imports and no packages. Functions and top-level `global` declarations live in
  one project-wide namespace, so a call `clamp(...)` pulls in the file that defines `clamp`;
  `local` names, parameters, loop variables and built-ins (`len`, `dict_get`, `ts`, …) are
  ignored. Ambiguous names are dropped rather than guessed. Top-level declarations are
  selectable excerpts too.
- An FE function seed pulls in the functions it calls and the top-level declarations it
  references, to `--depth`.
- TypeScript/JavaScript classes get the same `[class]`/`[method]` tiers. Type annotations on
  fields, parameters and locals type the receivers, and module imports (`./Repo`) resolve the
  classes. JavaScript has no annotations, so only `this.x()` and bare calls create edges.
- Rust structs/enums/traits get the same `[class]`/`[method]` tiers. Methods from every
  `impl Type` block in a file are attached to the type; `self`, `self.field`, parameter and
  local types resolve receivers, `Self::`/`Type::` resolve associated calls, and free
  functions resolve by unique name across the crate. `use crate/self/super` paths and
  `mod` declarations follow the Cargo module layout.
- Vue stays whole-file: its script offsets are relative to the extracted `<script>` block, so
  `.vue` is never sliced.

Excerpts are rendered as `### path :: name (line N)` and contain the declaration plus the
file header (package/imports/`using`s, one time) so they stand alone. Braces left open by
a block namespace are closed at the end.

The seed under the cursor is what `:FilerHere` uses: in a `.fe` buffer the function under the
cursor, in a class-based language the method or class, elsewhere the whole file.

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
`git status` and your own `.gitignore` is left alone. Branch names come straight from
`.git/HEAD` (no git subprocess); detached HEADs are stored as `detached@<sha>`, and repos
without git use a single `(no branch)` bundle. Older single-branch state files are migrated
on load.

## CLI

```sh
filer --root ~/code/demo search --query user           # JSON lines, one per symbol
filer --root ~/code/demo search --query user --plain   # fqn<TAB>path[::symbol]
filer --root ~/code/demo deps src/main/java/com/example/app/Application.java
filer --root ~/code/demo bundle --depth 2 --with-config \
      src/main/java/com/example/app/Application.java > bundle.md
filer --root ~/code/demo bundle 'src/main/java/app/Service.java::Service.usedEverywhere'
filer --root ~/code/demo bundle --at-line 120 src/main/java/app/Service.java
filer --root ~/code/demo index                         # cache stats
filer --root ~/code/demo show                          # re-render this branch's bundle
filer --root ~/code/demo show --branch main            # ... another branch's
filer --root ~/code/demo show --out bundle.md           # ... into a file
filer --root ~/code/demo state                         # what is stored per branch?
filer --root ~/code/demo state --clear                 # forget this branch
filer --root ~/code/demo state --clear --all           # forget everything

# stateful bundling: bundle twice, get the union
filer --root ~/code/demo bundle --state ~/code/demo/.filer/bundle.toml A.java > a.md
filer --root ~/code/demo bundle --state ~/code/demo/.filer/bundle.toml B.java > ab.md
filer --root ~/code/demo bundle --state ~/code/demo/.filer/bundle.toml --reset C.java > c.md
```

`bundle` flags: `--depth N` (0 = only the given units), `--with-same-package`,
`--with-config`, `--title`, `--out FILE`, `--state FILE`, `--reset`, `--at-line N`.
Default depth is 2. Add `::Symbol` for one declaration and `::Class.method` for a Java or
Rust method scope: `filer --root ~/ac bundle 'main.fe::riskValue'`,
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

## How resolution works

1. Walk the tree, skipping `.git`, `.gradle`, `.idea`, `.filer`, `.venv`, `.next`, `.nuxt`,
   `.output`, `.turbo`, `.svelte-kit`, `target`, `build`, `out`, `bin`, `dist`, `coverage`
   and `node_modules`.
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
   against each other, Rust only against Rust, and Asset Control Formula Engine is its own
   family with a single global namespace: calls and top-level declarations resolve by unique
   name across `.fe` files (the IDE plugin's stub indexes do the same, and index a top-level
   declaration whether it says `local` or `global`).
4. For a package-style import, try the full name, then trim trailing segments one at a time
   (`a.b.C.member` → `a.b.C`), which also covers nested types like `a.b.C.Inner` and Scala
   selectors / `given` imports. Wildcards expand to the package, or to the object/type they
   name (`import a.b.C._`); C# `using Some.Namespace;` is a package wildcard. Anything
   unresolved is JDK, stdlib, npm, NuGet or another third party.
5. For a TypeScript/JavaScript module import, join the specifier onto the importing file's
   directory, reject paths that escape the root, then probe in order: the exact path, TS
   extensions (`.ts`, `.tsx`, then JS `.js`, `.jsx`, `.mjs`, `.cjs`, then `.vue`), and
   `<dir>/index.*`. A `.js` specifier prefers the matching `.ts`/`.tsx` source. Bare
   specifiers (`react`) are treated as third-party.
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
8. Breadth-first walk from the seeds up to `--depth`, preserving discovery order so the
   bundle reads top-down from your entry point.

## Adding another language

`src/lang.rs` holds the language-neutral [`Source`](src/lang.rs) struct (`package`, `types`,
`imports` with an `ImportKind`, `uses`, `symbols` with byte spans, `preamble_end`), the
`Language` -> extension map and the `Language::family` grouping that keeps resolution
between compatible languages only. The per-language modules (`java`, `scala`, `kotlin`,
`groovy`, `csharp`, `rust`, `web`, `fe`) only translate their grammar into that struct. To add
another package+import language: add a parser module returning `Source`, add an arm to
`Language::from_path` and `SourceParser::parse`, and pick a fence string. Indexing, closure,
state and rendering need no changes. A language with a different import model (like TS/JS
module paths or Rust's `crate`/`self`/`super` paths) needs its own resolver branch in
`src/resolve.rs` — that is where `ImportKind::Module` and `Family::Rust` are handled.

Populating `symbols` is optional: use `symbol_from_node` for a tree-sitter declaration (it
extends the span over the doc comment and records the line) and `finish_symbols` to assign
per-symbol uses. Return no symbols and the language keeps whole-file bundling. `.fe` is the
opposite extreme: a hand-written lexer in `src/fe.rs` because no tree-sitter grammar exists
and filer only needs function boundaries, not statements.

## Layout

```
src/main.rs     CLI (search / bundle / deps / index)
src/lang.rs     language detection, families, Source/Import model, parser dispatch
src/java.rs     tree-sitter-java parsing, members/calls for pruning + tests
src/scala.rs    tree-sitter-scala parsing, import normalization + tests
src/kotlin.rs   tree-sitter-kotlin parsing + tests
src/groovy.rs   tree-sitter-groovy parsing + tests
src/csharp.rs   tree-sitter-c-sharp parsing (namespaces, usings) + tests
src/rust.rs     tree-sitter-rust parsing (use/mod, items, impl methods) + tests
src/web.rs      TypeScript/JavaScript parsing, Vue SFC extraction + tests
src/fe.rs       Asset Control Formula Engine lexer/parser + tests
src/index.rs    tree walk, parallelism, disk cache
src/resolve.rs  import resolution, closure, tests
src/bundle.rs   markdown rendering, class-shell pruning + tests
src/state.rs    TOML bundle state, branch detection, tests
lua/filer/      Neovim plugin: picker, bundling, bundle buffer
plugin/filer.lua user commands
examples/lazyvim.lua  copy-paste LazyVim spec with the defaults above
```

`cargo test` covers parsing for every language (packages/namespaces, Scala import forms,
Kotlin/Groovy/C# imports, Rust use/mod and impl methods, TS/JS module specifiers, Vue
scripts, FE functions/declarations), static/wildcard/module resolution, family isolation,
name inference without imports, closure
depth, same-package expansion, symbol extraction and symbol closure (including depth bounds
and whole-file supersession), preamble brace balancing, fence selection, TOML round-trips
with `symbol` entries, and branch staleness.
