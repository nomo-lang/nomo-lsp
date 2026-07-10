# nomo-lsp

Language Server Protocol implementation for the [Nomo](https://github.com/nomo-lang)
programming language, built on [tower-lsp](https://github.com/ebkalderon/tower-lsp).

`nomo-lsp` is the single source of language intelligence for every Nomo editor
integration. It links directly against the [`nomo`](https://github.com/nomo-lang/nomo)
compiler crate (as a `path` dependency), so the diagnostics it reports are
exactly the ones the compiler produces.

## Features

- Real-time diagnostics from the Nomo compiler front-end
- Manifest-aware dependency alias diagnostics matching `nomo check`
- Full-document text synchronization (open / change / save / close)
- Keyword, import path and semantic symbol completion
- Hover for current-document and local project module declarations, including signatures and doc comments
- Document symbols for current-document declarations, extern functions, interfaces, methods, fields and enum variants
- Workspace symbols for project and workspace declarations
- Go-to-definition for current-document and local project module declarations
- Find references for current-document and local project module declarations
- Rename for current-document and local project module identifier occurrences
- Quick-fix code actions from compiler suggestions
- Inlay hints for inferred `let` binding types and same-file function call parameter names
- Semantic highlighting tokens
- Full-document formatting through the shared `nomo fmt` formatter

## Role in the Nomo ecosystem

The editor extensions talk to this server rather than re-implementing the
language:

- [`vscode-nomo`](https://github.com/nomo-lang/vscode-nomo)
- [`zed-nomo`](https://github.com/nomo-lang/zed-nomo)
- [`intellij-nomo`](https://github.com/nomo-lang/intellij-nomo)

`nomo-lsp` depends on [`nomo`](https://github.com/nomo-lang/nomo); those editor
clients depend on `nomo-lsp`.

Completion, hover, document symbols, workspace symbols, go-to-definition,
references, and rename are backed by the compiler crate's reusable `semantic`
API. Code actions are backed by compiler diagnostics and suggestions. The LSP
server only adapts compiler ranges, signatures and suggestions into LSP types.

## Requirements

- A recent stable Rust toolchain (the crate uses edition 2024).
- The [`nomo`](https://github.com/nomo-lang/nomo) crate, expected as a sibling
  checkout at `../nomo` (referenced as a `path` dependency).

## Build and install

Tagged releases provide `nomo-lsp` archives for Linux x86-64, macOS x86-64 and
Apple silicon, and Windows x86-64 on the
[GitHub Releases page](https://github.com/nomo-lang/nomo-lsp/releases). Extract
the archive for your platform and place `nomo-lsp` (or `nomo-lsp.exe`) on your
`PATH`. The archive includes a checksum in the release's `SHA256SUMS` file.

To build from source, clone both repositories as siblings:

```bash
git clone https://github.com/nomo-lang/nomo.git
git clone https://github.com/nomo-lang/nomo-lsp.git
cd nomo-lsp
cargo build --release
# or install the binary onto your PATH so editors can find it:
cargo install --path .
```

Most editor extensions look up `nomo-lsp` on the `PATH`. The server speaks LSP
over stdio.

Diagnostics use the same compiler API as project-level `nomo check`: for a file
inside a project, `nomo-lsp` walks up to the nearest `nomo.toml`, reads declared
dependency aliases, and accepts imports such as `import json.parser` only when
`json` is declared in the manifest. Standalone files without a manifest keep the
single-file `nomoc` behavior and only accept built-in `std.*` imports. When a
diagnostic code is registered in the compiler's documented diagnostics registry,
LSP diagnostics include a `codeDescription` link to the matching
`docs/diagnostics/E####.md` reference page.

Formatting uses the same AST-based formatter as `nomo fmt`, applied as a single
full-document edit against the editor's current open buffer. If the current text
does not parse, the server returns no formatting edit and leaves diagnostics to
the normal compiler diagnostic flow.

Completion always includes v0.1 keywords. On `import` lines it adds supported
`std.*` paths, local project modules, and dependency aliases/modules with source
available. When the current document parses, completion also includes top-level
declarations and methods from the current document or, inside a project, local
`src/**/*.nomo` modules. Open editor buffers are used as overlays so unsaved
module declarations can appear in completion. Standard import completion reads
the shared toolchain `nomo-std` registry, including the native-boundary
`std.ffi.CString` and `std.ffi.Opaque` types.

Hover indexes the open document, local project `src/**/*.nomo` modules, and
public symbols from imported dependency modules with source available when a
nearest `nomo.toml` is available. It shows the parsed signature plus any `///`
or `/** */` item doc comment. Extern function declarations participate in the
same hover path. Open editor buffers are used as overlays so unsaved module
edits can participate in hover results.

Document symbols use the same parsed declaration index to power editor outline
views for top-level structs, enums, interfaces, constants, functions, extern
functions, and methods. Struct fields, enum variants, and interface methods are
nested under their parent type so outlines preserve the source model instead of
flattening members into the top level.

Workspace symbols index configured LSP workspace roots. A root that contains a
Nomo workspace indexes every workspace member; otherwise the nearest project is
indexed. Public symbols from dependency packages with source available are
included. Results include current open-buffer overlays and are filtered by the
client query.

Go-to-definition resolves local bindings, declarations in the same document,
local project modules under `src/**/*.nomo`, and public symbols from imported
dependency modules with source available. Cross-package lookup across every
member of a workspace remains a later graph extension.

Find references compares declaration identity rather than raw identifier text.
It follows local bindings and project declarations while excluding shadowed
parameters/variables and unrelated same-name declarations. Dependency package
sources are definition targets but remain outside the editable reference set.
Fields, struct literal labels, and methods use compiler-checked receiver types,
so same-name members on different structs resolve to distinct declarations.
Calls on constrained type parameters resolve to their declaring interface.

Rename reuses those declaration-aware locations across the current document and
local project modules. The new name must be a valid Nomo identifier. When the
original program checks successfully, the proposed in-memory edits are checked
again by the compiler and rejected if they introduce declaration collisions or
other semantic errors.

Code actions expose compiler suggestions as quick fixes, add missing concrete
imports such as `import std.io` or `import std.io.println`, and can either
update a mismatched package declaration or rename the module file so both agree.

Inlay hints show conservative inferred type hints for `let` bindings without an
explicit type annotation, such as `let label = "hi"` rendering `: string`.
Hints are only produced when the type can be determined from syntax-level facts
such as literals, casts, struct literals, and matching `if`/`match` branch
types. Same-file function, extern function, and impl/interface method calls also
receive parameter-name hints when the callee signature is available in the
current parsed file.

## Development

```bash
cargo run     # start the server (communicates over stdio)
cargo test
```

The release workflow checks out the matching `nomo` tag beside `nomo-lsp`, so a
tagged language-server release requires the same `v<version>` tag to exist in
both repositories. Manual workflow runs use the current `main` branches and
build artifacts without publishing a release.

## License

MIT. See [LICENSE](LICENSE).
