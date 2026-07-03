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
- Keyword and semantic symbol completion
- Hover for current-document and local project module declarations, including signatures and doc comments
- Document symbols for current-document declarations and methods
- Workspace symbols for project and workspace declarations
- Go-to-definition for current-document and local project module declarations
- Find references for current-document and local project module declarations
- Rename for current-document and local project module identifier occurrences
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
API. The LSP server only adapts compiler symbol ranges and signatures into LSP
types.

## Requirements

- A recent stable Rust toolchain (the crate uses edition 2024).
- The [`nomo`](https://github.com/nomo-lang/nomo) crate, expected as a sibling
  checkout at `../nomo` (referenced as a `path` dependency).

## Build and install

```bash
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
single-file `nomoc` behavior and only accept built-in `std.*` imports.

Formatting uses the same AST-based formatter as `nomo fmt`, applied as a single
full-document edit against the editor's current open buffer. If the current text
does not parse, the server returns no formatting edit and leaves diagnostics to
the normal compiler diagnostic flow.

Completion always includes v0.1 keywords. When the current document parses, it
also includes top-level declarations and methods from the current document or,
inside a project, local `src/**/*.nomo` modules. Open editor buffers are used as
overlays so unsaved module declarations can appear in completion.

Hover indexes the open document plus local project `src/**/*.nomo` modules when
a nearest `nomo.toml` is available, and shows the parsed signature plus any
`///` or `/** */` item doc comment. Open editor buffers are used as overlays so
unsaved module edits can participate in hover results.

Document symbols use the same parsed declaration index to power editor outline
views for top-level structs, enums, constants, functions, and methods.

Workspace symbols index configured LSP workspace roots. A root that contains a
Nomo workspace indexes every workspace member; otherwise the nearest project is
indexed. Results include current open-buffer overlays and are filtered by the
client query.

Go-to-definition resolves references to declarations in the same document or in
local project modules under `src/**/*.nomo`. Dependency package and whole
workspace definition lookup remain future semantic graph slices.

Find references returns lexical identifier occurrences in the same document and
local project modules for the selected declaration name. Precise shadowing-aware,
dependency-aware, and workspace-wide references will come from the shared
semantic graph.

Rename reuses the same reference locations to return a workspace edit across the
current document and local project modules. The new name must be a valid Nomo
identifier; dependency-aware and shadowing-aware rename remain future semantic
graph work.

## Development

```bash
cargo run     # start the server (communicates over stdio)
cargo test
```

## License

MIT. See [LICENSE](LICENSE).
