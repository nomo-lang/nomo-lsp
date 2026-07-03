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
- Keyword completion
- Hover for current-document and local project module declarations, including signatures and doc comments
- Document symbols for current-document declarations and methods
- Go-to-definition for current-document and local project module declarations
- Find references for current-document and local project module declarations
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

Hover, document symbols, go-to-definition, and references are backed by the
compiler crate's reusable `semantic` API. The LSP server only adapts compiler
symbol ranges and signatures into LSP types.

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

Hover indexes the open document plus local project `src/**/*.nomo` modules when
a nearest `nomo.toml` is available, and shows the parsed signature plus any
`///` or `/** */` item doc comment. Open editor buffers are used as overlays so
unsaved module edits can participate in hover results.

Document symbols use the same parsed declaration index to power editor outline
views for top-level structs, enums, constants, functions, and methods.

Go-to-definition resolves references to declarations in the same document or in
local project modules under `src/**/*.nomo`. Dependency package and whole
workspace definition lookup remain future semantic graph slices.

Find references returns lexical identifier occurrences in the same document and
local project modules for the selected declaration name. Precise shadowing-aware,
dependency-aware, and workspace-wide references will come from the shared
semantic graph.

## Development

```bash
cargo run     # start the server (communicates over stdio)
cargo test
```

## License

MIT. See [LICENSE](LICENSE).
