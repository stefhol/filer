# filer

Select Java, Scala, Kotlin, Groovy, C#, TypeScript, JavaScript, Vue or Asset Control
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
│   com.example.repo.UserRepository        <Tab>       │
│   com.example.model.User                 <Tab>       │
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
  `.ts` / `.tsx`, `.js` / `.jsx` / `.mjs` / `.cjs`, `.vue` and `.fe` files under the project
  root (tree-sitter based except `.fe`, which has a small hand-written parser; no LSP, no
  classpath, no compiler).
- Resolves package-style imports against the files that actually exist in your repo:
  Java `import a.b.C;` / `import static ...` / `import a.b.*;`, Scala 2 and 3 (selector
  lists, renames, `given`, comma-separated imports, nested or braced packages, package
  objects), Kotlin wildcards and aliases, Groovy wildcards, static imports and aliases, and
  C# `using` namespaces, `using static` and aliases.
- Resolves TypeScript/JavaScript/Vue module specifiers: relative imports and re-exports
  (`./x`, `../y.js`), extensionless imports, directory `index` files, `.js` specifiers that
  point at a `.ts` source, dynamic `import()` and CommonJS `require()`. Vue `<script>` and
  `<script setup>` blocks are extracted and parsed as TypeScript/JavaScript.
- Follows type *usages* too, so a class that references a sibling in its own package — which
  Java, Scala, Kotlin, Groovy or C# needs no import for, e.g. `indexOps(DemoDocument.class)`,
  `DemoDocument.create()` or `new DemoDocument()` — still pulls that file in. Module
  languages rely on their explicit imports instead of repo-wide name guessing.
- Extracts **symbols** (top-level functions, classes, objects, global declarations, Java
  methods) with their byte spans, so a bundle can carry one function out of a huge file.
  `.fe` functions are the primary use case: they call each other across a project-wide
  namespace, exactly like the language's IntelliJ plugin resolves them. Java methods add a
  second one: class shell for context, call-path bodies for content.
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
`*.csproj` / `package.json` dependencies, or pretend to resolve libraries. JDK, Scala stdlib,
npm packages, NuGet packages and other third-party imports are simply reported as unresolved
(`tsconfig` path aliases are not read). Imports only resolve within the same language family
(JVM-ish languages with each other, TS/JS/Vue with each other, FE with itself), never
across.

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
| `:FilerSearch` | Telescope picker of symbols: `.fe` functions, Java classes **and methods**, types elsewhere. `<Tab>` multi-selects, `<CR>` bundles. |
| `:FilerHere` | Bundles the current buffer with its imports. In a `.fe` file the function under the cursor is the seed; in Java, the method under the cursor (or its class). |
| `:FilerBundle {files}` | Bundles the given files (relative to root or absolute) or excerpts (`path::Symbol`). |
| `:FilerShow [branch]` | Re-renders the saved bundle (current file contents) without re-resolving. |
| `:FilerReset` | Forgets this branch's bundle; the next bundle starts empty. |
| `:FilerResetAll` | Forgets every branch's bundle. |

In the bundle buffer: `y` yanks, `q` closes. The buffer is `nofile` and never written to
disk. Bundling again — picker or `:FilerHere` — refills that same window and buffer instead
of opening another split; `q` closes it, and the next bundle opens a fresh one.

Root detection uses `vim.fs.root` with markers `.git`, `pom.xml`, `build.gradle(.kts)`,
`settings.gradle(.kts)`, `build.sbt`, `gradlew`, `mvnw`, `tsconfig.json`, `jsconfig.json`,
`package.json`, `*.csproj`, `*.sln`. Override with `opts.root` (string or function).

## Excerpts, not whole files

Every language now records its top-level declarations as *symbols* with byte spans
(`src/fe.rs` for `.fe`, plus the tree-sitter parsers). A bundle entry is either a whole file
or one symbol of it:

```sh
filer --root ~/ac bundle --depth 2 main.fe::riskValue     # one function + what it calls
filer --root ~/ac bundle --at-line 120 main.fe            # the function at line 120
filer --root ~/ac bundle main.fe                          # still the whole file
```

What that means in practice:

- `.fe` has no imports and no packages. Functions and top-level `global` declarations live
  in one project-wide namespace, so a call `clamp(...)` pulls in the file that defines
  `clamp`; `local` names, parameters, loop variables and built-ins (`len`, `dict_get`, `ts`,
  …) are ignored. Ambiguous names are dropped rather than guessed.
- An FE function seed pulls in the functions it calls and the top-level declarations it
  references, to `--depth`. Top-level declarations are selectable excerpts too.
- Excerpts are rendered as `### path :: name (line N)` and contain the declaration plus the
  file header (package/imports/`using`s, one time) so they stand alone. Braces left open by
  a block namespace are closed at the end.
- Other languages keep whole-file bundling when you pass a plain path. Java/Scala/Kotlin/
  C#/Groovy/TS/JS types are selectable as excerpts (`Application.java::Application`), and
  `:FilerSearch` lists one entry per symbol instead of one per file.
- Vue symbols are not sliced out (script offsets are relative to the extracted `<script>`
  block), so `.vue` stays whole-file.

The seed under the cursor is what `:FilerHere` uses in a `.fe` buffer: put the cursor in a
function, the bundle contains that function and its calls.

### Java methods: seed file complete, dependencies pruned

A Java *class* row in the picker keeps its old meaning: that class as an excerpt. A *method*
row (`app.Service.usedEverywhere`), `filer bundle 'app/Service.java::Service.usedEverywhere'`
and `:FilerHere` inside a method all seed a call graph:

- the **file you seeded is rendered complete** (all classes and methods in it),
- every file at depth 1+ is rendered as a class shell — package, imports, fields,
  constructors, annotations — with only the methods on the call path,
- their other methods become signature stubs:
  `int unrelatedHelper();   // body omitted`

```
### `app/Service.java`                    (depth 0, complete)

### `app/Repo.java` :: `Repo` (line 3)    (depth 1)
package app;

public class Repo {
    public int size() { return 0; }

    public int unused();   // body omitted

    public static void staticCall() { }
}
```

Calls are resolved through:

- bare `helper()` — the class, then superclasses/interfaces, then `import static` targets
- `this.helper()` / `super.helper()`
- `local.helper()`, `param.helper()`, `field.helper()` — using the declared type of the receiver
- `Type.helper()` — static calls
- `new Type().helper()`

Everything the resolver cannot type (`a.b().c()`, interface dispatch, third-party types) adds
no edge. If the receiver's class is known but the method is not found, the **whole class** is
kept rather than stubbing a method that might be needed. Type-only dependencies (a field of
type `Repo` that is never called) are not bundled; the import line and the field signature
still give the reader the name. Nested classes are rendered inside their enclosing class.

`--depth` counts method hops. In a `.java` buffer `:FilerHere` seeds the method under the
cursor (or its class when the cursor is on a field, constructor or blank line); the picker and
`filer bundle 'path/Service.java::Service.usedEverywhere'` do the same explicitly. A bare
`filer bundle path/Service.java` still uses the file-level import closure, not the call graph.

## Bundle state

With `state = true` (the default) the binary writes `.filer/bundle.toml` next to the project,
keeping **one bundle per git branch** (`version = 2`):

```toml
# filer bundle state — `:FilerReset` clears the current branch, `:FilerResetAll` everything
root = "/home/me/code/demo"
version = 2

[[bundles]]
name = "feature/checkout"
created = 1793618434
depth = 2

[[bundles.files]]
path = "demo/src/main/java/com/example/demo/RunService.java"
depth = 0
reason = "seed"

[[bundles.files]]
path = "demo/src/main/java/com/example/demo/DemoDocument.java"
depth = 1
reason = "same-package"

[[bundles]]
name = "main"
created = 1793617000
depth = 2

[[bundles.files]]
path = "demo/src/main/java/com/example/demo/DemoApplication.java"
depth = 0
reason = "seed"
```

An excerpt entry also carries the declaration name; whole-file entries omit it. A pruned
Java class adds `member` for each kept method:

```toml
[[bundles.files]]
path = "ac/main.fe"
symbol = "riskValue"
depth = 0
reason = "seed"

[[bundles.files]]
path = "src/main/java/app/Service.java"
symbol = "Service"
member = "usedEverywhere"
depth = 0
reason = "seed"
```

What that buys you:

- The picker shows the **current branch's** bundled files as
  `✓ com.example.demo.DemoDocument  (depth 1, same-package)`. Selecting one is a no-op
  instead of a duplicate.
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
Default depth is 2. Add `::Symbol` to a file to bundle one declaration:
`filer --root ~/ac bundle 'main.fe::riskValue'`.
`--with-config` adds, when they exist: `pom.xml`, the Gradle files, `build.sbt` /
`project/*.sbt`, `tsconfig.json` / `jsconfig.json` / `package.json`, `Directory.Build.props`
and root-level `*.csproj` / `*.sln`.

Every indexed language works the same:

```sh
filer --root ~/code/demo bundle --depth 2 src/main/scala/com/example/app/Application.scala
filer --root ~/code/demo bundle --depth 2 src/main/kotlin/com/example/App.kt
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
   records and delegates; TS/JS add interfaces, type aliases and enums. Sequential and braced
   package/namespace clauses compose; `package object foo` counts as package `a.foo`;
   C# file-scoped namespaces apply to the rest of the file.
3. Build `fqn -> files`, `package -> files`, simple-name and `symbol -> (file, index)` maps.
   A name can exist in several languages, so lookups are filtered by language family:
   JVM-ish (Java, Scala, Kotlin, Groovy, C#) only resolve against each other, TS/JS/Vue only
   against each other, and Asset Control Formula Engine is its own family with a single
   global namespace: calls and top-level declarations resolve by unique name across `.fe`
   files (the IDE plugin's stub indexes do the same, and index a top-level declaration
   whether it says `local` or `global`).
4. For a package-style import, try the full name, then trim trailing segments one at a time
   (`a.b.C.member` → `a.b.C`), which also covers nested types like `a.b.C.Inner` and Scala
   selectors / `given` imports. Wildcards expand to the package, or to the object/type they
   name (`import a.b.C._`); C# `using Some.Namespace;` is a package wildcard. Anything
   unresolved is JDK, stdlib, npm, NuGet or another third party.
5. For a module import, join the specifier onto the importing file's directory, reject paths
   that escape the root, then probe in order: the exact path, TS extensions (`.ts`, `.tsx`,
   then JS `.js`, `.jsx`, `.mjs`, `.cjs`, then `.vue`), and `<dir>/index.*`. A `.js`
   specifier prefers the matching `.ts`/`.tsx` source. Bare specifiers (`react`) are treated
   as third-party.
6. For every type name used in the file (Java/Scala type positions, Kotlin user types,
   Groovy/C# type positions, object receivers like `DemoDocument.create`) look the name up in
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
`groovy`, `csharp`, `web`, `fe`) only translate their grammar into that struct. To add
another package+import language: add a parser module returning `Source`, add an arm to
`Language::from_path` and `SourceParser::parse`, and pick a fence string. Indexing, closure,
state and rendering need no changes. A language with a different import model (like TS/JS
module paths) needs its own resolver branch in `src/resolve.rs` — that is where
`ImportKind::Module` is handled.

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
Kotlin/Groovy/C# imports, TS/JS module specifiers, Vue scripts, FE functions/declarations),
static/wildcard/module resolution, family isolation, name inference without imports, closure
depth, same-package expansion, symbol extraction and symbol closure (including depth bounds
and whole-file supersession), preamble brace balancing, fence selection, TOML round-trips
with `symbol` entries, and branch staleness.
