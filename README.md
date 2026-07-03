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
- Hover for current-document declarations, including signatures and doc comments
- Document symbols for current-document declarations and methods
- Go-to-definition for current-document declarations
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

Hover currently indexes the open document's declarations and shows the parsed
signature plus any `///` or `/** */` item doc comment. Cross-module hover and
workspace-wide definition/reference queries are planned as the next semantic API
slices.

Document symbols use the same parsed declaration index to power editor outline
views for top-level structs, enums, constants, functions, and methods.

Go-to-definition currently resolves references to declarations in the same open
document. Cross-module and workspace-wide definition lookup are planned as the
next semantic graph slices.

## Development

```bash
cargo run     # start the server (communicates over stdio)
cargo test
```

## License

MIT. See [LICENSE](LICENSE).
