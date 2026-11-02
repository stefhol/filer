# Plan: explicit bundle scopes (file / class / method)

Status: **implemented** (see `a9b81a1`). Supersedes the implicit `path::symbol` + "method seed
forces the whole file" behaviour that was in `main.rs`/`state.rs` before v3.

Follow-up shipped: the member tier is no longer Java-only. Kotlin, Groovy, C#, Scala and
TypeScript/JavaScript all populate members, fields, supertypes and typed call receivers, so
they expose the same `[file]`/`[class]`/`[method]` rows and call-graph closure as Java. Vue
stays whole-file; `.fe` stays function-level. The "other languages stay at symbol tier" notes
below are the original rollout plan, not the current behaviour.

## Problem

The bundle currently infers granularity from which fields happen to be set:

- `path` only → whole file
- `path` + `symbol` → class/type excerpt
- `path` + `symbol` + `member` → method seed, but the renderer forces the **file** whole
  (added as a stop-gap), and the state stores it as a plain file entry.

Consequences:

1. **The picker has no file level.** File / class / method cannot be chosen cleanly.
2. **The state cannot say what was included in which way.** `path` alone does not distinguish
   "whole file" from "method seed whose file happened to be rendered whole".
3. **The origin is lost.** A method seed is stored as a whole-file entry, so a later run
   cannot tell which method drove the closure.
4. Depth 1+ is minimal (class shell + used methods), but that rule is not written anywhere —
   it is an accident of the renderer.

## Granularity tiers (Ausbaustufen)

The scope model is shared, but each language exposes only the tiers it actually has. Nothing
below the Java tier changes in this plan.

| Tier | Languages | Picker rows | Unit | Depth closure | Change |
| --- | --- | --- | --- | --- | --- |
| File | Vue, config files, files with no symbols | `[file]` | whole file | file-level | none |
| Symbol | Scala, Kotlin, C#, Groovy, TS/JS | `[file]` + `[type]` / `[function]` | top-level symbol excerpt | file-level (today's rules) | picker gains `[file]`; state gains `scope = "symbol"` |
| Function (frozen) | Asset Control FE | `[function]` only | function excerpt | function calls in the global namespace | **none** |
| Member | Java | `[file]` + `[class]` + `[method]` | file / class / class shell + methods | call graph, minimal at depth 1+ | this plan |

Rules that follow from the tiers:

- `scope = "methods"` is produced and accepted only by member-capable languages (Java now).
  The picker/search never emits `[method]` rows for a language that cannot honour them.
- **FE is frozen.** No `[file]` row, no class/method scopes, and no change to its closure,
  renderer, cache or state output. The refactor must be a no-op for `.fe`; the existing
  function-level tests are the regression guard.
- The other parsed languages stay at their current level: top-level type excerpts with the
  existing file-level closure. Only the *state representation* becomes explicit
  (`scope = "symbol"` or `"file"`); their bundles do not change.
- Adding a tier later is a language capability change, not a resolver rewrite: the scope
  types, state schema and renderer already cover file / symbol / methods.

> Decision: "still only file based" means *no method-level work for the other languages*
> They keep today's type excerpts and file-level closure, and gain an explicit `[file]` row so
> the whole file is selectable. No `[method]` scopes, no member pruning. FE stays frozen.
> This can change in the future but it should be regarded as backlog items.


## Model

Every bundle entry is a **selection** with an explicit `scope`:

| scope | renders | seed closure origin |
| --- | --- | --- |
| `file` | whole file | Java: every method of every class in the file; other languages: the existing file-level closure |
| `symbol` | one top-level symbol (Java class, `.fe` function, TS type, …) | Java: every method of the class; otherwise the existing symbol closure |
| `methods` | class shell + listed methods; everything else a signature stub | the listed seed method(s) |

Picker rows map 1:1 to scopes:

```
[file]   app/Service.java
[class]  app.Service
[method] app.Service.used
[method] app.Service.omitted
```

- `[file]` → whole file.
- `[class]` → class excerpt, all members (existing behaviour).
- `[method]` → class shell + call path; **no forced whole file**. Whole file is the explicit
  `[file]` row.

For languages: `[file]` for every file plus `[type]` / `[function]` for top-level symbols.
The type rows keep today's behaviour; the file row is new (whole-file selection). Java is the
only language with `[method]` in this cut.

## bundle.toml v3

One entry per **unit**, dependency methods grouped per class:

```toml
version = 3
root = "/home/me/code/demo"

[[bundles]]
name = "main"
created = 1793630718
depth = 2

# the seed: a method. Class shell keeps the seed and its intra-class callees.
[[bundles.entries]]
path = "app/Service.java"
scope = "methods"
symbol = "Service"
methods = ["used", "internal"]
depth = 0
reason = "seed"

# a dependency reached by a call
[[bundles.entries]]
path = "app/Repo.java"
scope = "methods"
symbol = "Repo"
methods = ["size", "staticCall"]
depth = 1
reason = "call"

# whole class (e.g. method not found in it, or a class seed)
[[bundles.entries]]
path = "app/Base.java"
scope = "symbol"
symbol = "Base"
depth = 2
reason = "call"

# a whole file (file seed, config file, or a language without symbols)
[[bundles.entries]]
path = "build.gradle"
scope = "file"
depth = 0
reason = "config"
```

Rules:

- `scope = "file"`: `path` only.
- `scope = "symbol"`: `path` + `symbol`.
- `scope = "methods"`: `path` + `symbol` + non-empty `methods`.
- `depth` for `methods` is the minimum depth of its methods.
- `reason` is `seed` if any of the methods is a seed, otherwise the reason of the
  minimum-depth method (`call`, `import`, `usage`, …).
- No `member` field anymore; the list replaces it.

### Migration from v2

| v2 entry | v3 entry |
| --- | --- |
| `path` only | `scope = "file"` |
| `path` + `symbol` | `scope = "symbol"` |
| `path` + `symbol` + `member`, `reason = "seed"` | `scope = "methods"`, `methods = [member]` |
| `path` + `symbol` + `member`, other reason | grouped by `(path, symbol)` into one `scope = "methods"` with the union of members; `depth` = min; `reason` = seed if any, else min-depth reason |

v1 (single branch) goes through the existing migration, then v2→v3. Unknown/invalid entries are
dropped with the current warning style.

Note: a v2 **method seed** was stored as a whole-file entry plus per-member dependency
entries (that was the stop-gap). The origin method cannot be recovered, so such a path
migrates to `scope = "file"`; the dependency `methods` entries are recovered normally. The
new v3 state records `scope = "methods"` from the start, so this loss only affects old files.

Field rename: v3 uses `[[bundles.entries]]`; v2 used `[[bundles.files]]`. The loader reads
`files` for `version <= 2` and `entries` for `version = 3`.

## Picker contract

`filer search` JSON gains `scope`, and file rows:

```json
{"path":"app/Service.java","scope":"file","symbol":null,"member":null,"kind":"file","fqn":"app/Service.java","line":null,...}
{"path":"app/Service.java","scope":"symbol","symbol":"Service","member":null,"kind":"class","fqn":"app.Service","line":5,...}
{"path":"app/Service.java","scope":"methods","symbol":"Service","member":"used","kind":"method","fqn":"app.Service.used","line":15,...}
```

- `scope = "methods"` rows represent the **seed method** (`member`); the bundle will keep the
  seed plus intra-class callees.
- `scope = "file"` rows exist for every file. `.fe` files keep only `scope = "symbol"` rows in
  the picker (no `[file]`), but the CLI still accepts a file path.
- `kind` is `file` / `class` / `method` / `type` / `function`.
- Pick target:
  - `file` → `path`
  - `symbol` → `path::Symbol`
  - `methods` → `path::Class.method`
- Label prefix in the Lua picker: `[file]`, `[class]`, `[method]`, `[type]`, `[function]`,
  then the existing fqn/label.
- `included` marking, from the state entries:
  - `[method]` row included if a `methods` entry lists it, or a `symbol` entry for that class,
    or a `file` entry for that path.
  - `[class]` row included if a `symbol` entry for the class, or a `file` entry.
  - `[file]` row included only if a `file` entry.
  - FE `[function]` row included if the `symbol` entry matches.

CLI input mapping:

| Input | Scope |
| --- | --- |
| `path` | `file` |
| `path::Symbol` | `symbol` |
| `path::Class.method` | `methods` |
| `--at-line` inside a method | `methods` (one method) |
| `--at-line` inside a class (field/constructor/header) | `symbol` |
| `--at-line` on package/import/blank before the class | `file` |
| `.fe` `--at-line` inside a function | `symbol` |

`:FilerHere` keeps `--at-line` for `.fe` and `.java`; other languages stay whole-file.

## Closure

Internal BFS keeps per-method units (`Unit::Member`) for resolution; grouping into
`scope = "methods"` happens at merge/render/state time.

Seed origins:

- `file` (Java): all methods of all classes in the file. Their calls are resolved at depth 1.
  Dependencies are `methods` units. Files without symbols (config, unsupported languages) keep
  the old file-level closure.
- `symbol` (Java class): all methods of the class. Dependencies are `methods` units.
- `methods`: the listed seed method(s). Intra-class callees join the same class unit at
  depth 0; cross-class targets become `methods` units at depth 1.
- `symbol` (FE, other languages): unchanged (`Source.uses`/`calls`).
- `file`/`symbol` for non-Java: unchanged.

Dependency rules (unchanged from the current Java work):

- bare call → class, then supertypes/interfaces, then `import static`
- `this` / `super`
- local/parameter/field receiver → declared type
- `Type.m()` and `new Type().m()`
- unknown receiver → no edge
- receiver class known but method not found → whole class (`scope = "symbol"`)
- overloads resolved by name (all same-named methods kept)
- type-only dependencies (fields, imports, `new X()` with no method call) are **not** bundled
  in minimal mode; the seed file/shell still shows the names.

`--depth` counts method hops (or unit hops for file/symbol seeds). Depth 0 = the seed scope
only. `--with-same-package` remains a whole-class expansion; not part of the minimal path.

## Rendering

- Group `methods` entries by `(path, symbol)` and render one block per class with the union.
- `symbol` entry for the same class supersedes `methods`; `file` supersedes everything in the
  file (`file > symbol > methods`).
- Preamble (package/imports/using, braces balanced) in front of symbol/methods blocks.
- `methods` block: kept methods full; other methods with a body become
  `signature;   // body omitted`; constructors, fields, initializers, nested classes stay.
- Files list (one line per state unit):

  ```
  - `app/Service.java` :: `Service` [used, internal]
  - `app/Repo.java` :: `Repo` [size, staticCall] (depth 1, call)
  - `app/Base.java` :: `Base`
  - `build.gradle`
  ```

- Headings: `### path :: Class (line N)`; `### path` for files.
- Counts: distinct files, content blocks (`excerpts`), and optionally methods kept.

## State merging

- Previous entries are validated per scope:
  - `file`: keep if the path is indexed or exists.
  - `symbol`: drop iff the symbol is gone.
  - `methods`: keep the intersection with existing methods; drop the entry if empty.
- Merge with the new closure by unit; `methods` union; min depth; seed wins.
- `drop_covered`: file > symbol > methods.
- Save grouped v3 entries.
- `:FilerShow` re-renders from the v3 entries against current file contents; does **not**
  re-resolve new calls (snapshot semantics). Stale members are dropped with a notice.
- Bundle appendix uses the same v3 TOML.

## Implementation order

1. **Types**
   - `enum Selection { File, Symbol, Methods }` (serde kebab-case).
   - `enum Granularity { File, Symbol, Function, Member }` returned by `Language`
     (`Fe` → Function, Java → Member, Vue/unknown → File, other parsed languages → Symbol).
   - `BundleFile { rel, abs, scope, symbol, methods, depth, reason }` (methods = `BTreeSet`).
   - Keep `Unit::File/Symbol/Member` for the BFS; add a grouping step to `BundleFile`.
   - Search/seed parsing asks the language's granularity before emitting or accepting a
     `methods` scope.
2. **`bundle_cmd`**
   - Remove the `seed_files` force-whole-file hack.
   - Parse inputs to a scope (`path`, `path::Symbol`, `path::Class.method`).
   - `--at-line` → scope as in the table above.
   - Merge previous entries by scope; save grouped v3 entries.
3. **Search / picker JSON**
   - Emit file rows; add `scope`; adjust `included`; add `[file]/[class]/[method]` labels;
     target strings per scope.
4. **Resolve**
   - Scope-aware seed origins (file → all methods, class → class methods, method → one).
   - Java class seeds use the call graph, not `symbol.uses`.
   - Keep the existing dependency rules.
5. **State**
   - v3 schema, migration, grouping, validation, `show`.
6. **Bundle renderer**
   - Group by `(path, symbol)`; `methods` list in the Files section; counts.
7. **Lua**
   - `member` vs `scope`/`methods`; targets; `[file]` rows; `:FilerHere` unchanged.
8. **Tests**
   - state v2→v3 migration
   - picker rows/scopes/included
   - file seed → all methods' callees, minimal deps
   - class seed → class whole, minimal deps
   - method seed → class shell, not the file
   - depth 0/1/2; intra-class merge; unknown-method fallback
   - renderer grouping/stubs/counts
   - `show` after a method rename
9. **Docs / install**
   - README section rewrite (scope model, TOML, picker)
   - cache bump only if the parse model changes (scope is bundle-level, so likely not)
   - `cargo install --path . --force`, Neovim headless check

## Non-goals / known limitations

- Overload resolution by arity; interface dispatch; chained receiver typing.
- Inferred types (`val x = Local()`, `var x = ...`, `const x = new X()`): only explicit
  annotations or `val`/`var`/constructor-property declarations type a receiver.
- Type-only dependencies in minimal mode.
- Nested classes are rendered whole inside their enclosing class.
- Enum methods are extracted; annotation-type elements are not.
- `.fe` whole-file rows are intentionally absent from the picker.
- `:FilerShow` is a snapshot, not a re-resolution.

## Decisions recorded

- Method seed renders **class shell + call path**, not the whole file.
- Picker is **one list with explicit `[file]` / `[class]` / `[method]` rows**.
- `[file]` seeds use the **minimal call graph**, not the old import/type closure.
- Callee classes at depth 1+ show **only call-path methods**, rest as signature stubs.
- Seed scope: method → class shell; class → class; file → whole file. `.fe` → function.
- Tiers are explicit: Java is the only member-capable language; the other parsed languages
  keep their type excerpts and file-level closure and gain an explicit `[file]` row; FE is
  frozen; Vue/config are whole-file.
