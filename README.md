# filer

Select Java code in Neovim, get one pastable markdown blob for your chat LLM — with the
files your selection imports, and the files those import, already included.

You have a chat interface but no agentic workflow. `filer` covers the boring part: finding
the three or four files that belong together and formatting them so the model sees the
whole picture instead of a fragment.

```
┌─ Telescope: filer: Java types (demo) ────────────────┐
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

- Indexes every `.java` file under the project root (tree-sitter based, no LSP, no classpath).
- Resolves `import a.b.C;`, `import a.b.C.member;` (static) and `import a.b.*;` against the
  files that actually exist in your repo, then walks the import graph to a configurable depth.
- Renders the selection as markdown: header, file list with import depths, then one fenced
  block per file. Fences grow automatically if a file already contains backticks.
- Caches the index in `~/.cache/filer/`, keyed by root path and invalidated per file by
  mtime + size. Re-indexing a 2000-file project takes ~40 ms, so the picker re-queries on
  every keystroke.

Things it deliberately does not do: compile your project, read `pom.xml` dependencies, or
pretend to resolve libraries. JDK and third-party imports are simply reported as unresolved.

## Build

```sh
cargo build --release     # binary at target/release/filer
# optional, to use it outside Neovim too:
cargo install --path .
```

Requires a C compiler for the tree-sitter grammar (`gcc`/`clang`).

## Neovim setup

The full, copy-paste spec lives in [`examples/lazyvim.lua`](examples/lazyvim.lua) — it
already carries the defaults below. Drop it into your config and fix `dir`:

```sh
# LazyVim / lazy.nvim import dir:
cp examples/lazyvim.lua ~/.config/nvim/lua/plugins/filer.lua
# kickstart.nvim (import = "custom.plugins"):
cp examples/lazyvim.lua ~/.config/nvim/lua/custom/plugins/filer.lua
```

```lua
{
  dir = "~/Work/filer",
  name = "filer",
  cmd = { "FilerSearch", "FilerHere", "FilerBundle" },
  keys = {
    { "<leader>fc", function() require("filer").search() end, desc = "filer: bundle for chat" },
    { "<leader>fh", function() require("filer").here() end, desc = "filer: bundle current file" },
    { "<leader>fC", function() require("filer").bundle({ vim.api.nvim_buf_get_name(0) }, { depth = 0 }) end, desc = "filer: bundle current file only" },
  },
  opts = {
    bin = nil,            -- $PATH, then this repo's target/{release,debug}/filer
    root = nil,           -- detect via .git / pom.xml / build.gradle(.kts) / gradlew / mvnw
    depth = 2,            -- import hops to follow
    same_package = false, -- also pull in sibling files of the same package
    with_config = false,  -- also add pom.xml / build.gradle
    split = "vsplit",     -- or "split", "tabnew"
    copy = false,         -- true = also copy the bundle to the system clipboard
  },
}
```

The plugin needs [telescope.nvim](https://github.com/nvim-telescope/telescope.nvim) for
`:FilerSearch`. `:FilerHere` and `:FilerBundle` have no dependencies.

## Commands

| Command | Effect |
| --- | --- |
| `:FilerSearch` | Telescope picker. `<Tab>` multi-selects, `<CR>` bundles. |
| `:FilerHere` | Bundles the current buffer's file with its imports. |
| `:FilerBundle {files}` | Bundles the given files (relative to root or absolute). |

In the bundle buffer: `y` yanks, `q` closes. The buffer is `nofile`, not written to disk,
and not added to `:ls` history beyond its split.

Root detection uses `vim.fs.root` with markers `.git`, `pom.xml`, `build.gradle(.kts)`,
`settings.gradle(.kts)`, `gradlew`, `mvnw`. Override with `opts.root` (string or function).

## CLI

```sh
filer --root ~/code/demo search --query user           # JSON lines, one per type
filer --root ~/code/demo search --query user --plain   # fqn<TAB>path
filer --root ~/code/demo deps src/main/java/com/example/app/Application.java
filer --root ~/code/demo bundle --depth 2 --with-config \
      src/main/java/com/example/app/Application.java > bundle.md
filer --root ~/code/demo index                         # cache stats
```

`bundle` flags: `--depth N` (0 = only the given files), `--with-same-package`,
`--with-config`, `--title`, `--out FILE`. Default depth is 2.

## How resolution works

1. Walk the tree, skipping `.git`, `target`, `build`, `out`, `bin`, `dist`, `.gradle`,
   `.idea`, `node_modules`.
2. Parse each file with tree-sitter-java: package declaration, top-level type declarations
   (class/interface/enum/record/annotation), import declarations.
3. Build a `fqn -> file` map and a `package -> files` map.
4. For an import, try the full name, then trim trailing segments one at a time
   (`a.b.C.member` → `a.b.C`), which also covers nested types like `a.b.C.Inner`.
   Wildcard imports expand to the whole package. Anything unresolved is JDK/third-party.
5. Breadth-first walk from the seeds up to `--depth`, preserving discovery order so the
   bundle reads top-down from your entry point.

## Adding another language

`src/java.rs` owns everything grammar-specific: it returns `package`, `types`, `imports`
with an `ImportKind`, and the rest of the pipeline is language-agnostic. For Kotlin or
TypeScript, add a sibling module that produces the same `JavaSource`-shaped struct, dispatch
on file extension in `src/index.rs`, and teach `src/bundle.rs` the right fence language.

## Layout

```
src/main.rs     CLI (search / bundle / deps / index)
src/java.rs     tree-sitter-java parsing + tests
src/index.rs    tree walk, parallelism, disk cache
src/resolve.rs  import resolution, closure, tests
src/bundle.rs   markdown rendering, tests
lua/filer/      Neovim plugin: picker, bundling, bundle buffer
plugin/filer.lua user commands
examples/lazyvim.lua  copy-paste LazyVim spec with the defaults above
```

`cargo test` covers parsing, static/wildcard imports, closure depth, same-package expansion
and fence selection.
