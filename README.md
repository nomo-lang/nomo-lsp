# nomo-lsp

`nomo-lsp` is the shared Language Server Protocol implementation for the
[Nomo programming language](https://www.nomo-lang.org). VS Code, Zed, and
IntelliJ integrations use this server instead of maintaining separate language
models.

## Status and compatibility

Nomo and this server are in **Preview**. They are suitable for language
evaluation and controlled development workflows, not a stable production
toolchain. Breaking changes may ship between timestamped snapshots.

The latest packaged server is the prerelease
[`v0.0.0-20260721120555`](https://github.com/nomo-lang/nomo-lsp/releases/tag/v0.0.0-20260721120555).
Current `main` is newer and pins both `nomo` and `nomo-lsp-bridge` to compiler
commit
[`085da51`](https://github.com/nomo-lang/nomo/commit/085da513ff6c042bd00571c49a6eb061722acf6f).
Use matching server and editor snapshots when evaluating unreleased syntax.

There is no stable `v0.1.0` release. Read the
[release gate](https://github.com/nomo-lang/rfcs/blob/main/RELEASE-GATE.md)
before making maturity or platform claims.

## What this repository owns

This repository owns:

- LSP transport over standard input/output;
- compiler-backed diagnostics and quick fixes;
- completion, hover, signature help, symbols, navigation, references, and
  rename;
- semantic tokens and conservative inlay hints;
- full-document formatting through the shared formatter;
- open-buffer overlays and dependency-aware analysis caches;
- editor-facing release smoke tests and packaged server archives.

The compiler repository owns parsing, type checking, manifests, module graphs,
canonical signatures, formatting rules, C99/WASM backends, Runtime, and the
standard library. Syntax changes must land there first and then update the
pinned revisions in this repository.

## Install and run

Download a timestamped archive from
[GitHub Releases](https://github.com/nomo-lang/nomo-lsp/releases), verify it
against `SHA256SUMS`, and put `nomo-lsp` (or `nomo-lsp.exe`) on `PATH`. Release
archives also contain the matching `std/src` tree used for standard-library
navigation.

To build the current source:

```sh
git clone https://github.com/nomo-lang/nomo-lsp.git
cd nomo-lsp
cargo build --locked --release
./target/release/nomo-lsp
```

The last command starts an LSP server over stdio. Normally an editor extension
starts it and handles the protocol.

## Canonical source model

Project analysis walks upward to the nearest `nomo.toml` and delegates package
and module identity to the pinned compiler:

```toml
[package]
name = "hello-world"
```

```nomo
package hello_world

import std.io

fn main() {
    io.println("Hello, Nomo")
}
```

The manifest name determines the lower-snake-case module root. `src/main.nomo`
declares that root directly; it no longer appends `.main`. Ordinary void-return
declarations omit `-> void`, while callable types such as
`task fn(string) -> void` keep a complete return type.

Dependency aliases are import names, not package declarations. Open buffers,
workspace members, path dependencies, and toolchain standard-library sources
are resolved through the compiler project context.

## Verified capabilities

The repository test and release gates currently exercise:

- versioned diagnostics from the shared compiler, including documentation URLs
  and compiler suggestions;
- manifest-aware local modules, workspace members, dependency aliases, and
  source overlays;
- keyword, import-path, top-level symbol, and method completion;
- hover and signature rendering for functions, methods, interfaces, externs,
  fields, variants, and standard-library declarations;
- document and workspace symbols;
- go-to-definition, declaration-aware references, and checked rename;
- quick fixes for compiler suggestions and missing imports;
- semantic tokens and inferred-binding or parameter-name inlay hints;
- full-document and range formatting through `nomo-fmt`;
- initialize, diagnostics, completion, shutdown, and bounded latency in a real
  server process.

These checks demonstrate the tested Preview surface. They do not establish
production readiness, exhaustive IDE compatibility, or stable protocol
extensions.

## Important boundaries

- Standalone files use the compiler's single-file behavior; project-aware
  dependency resolution requires a nearby `nomo.toml`.
- Dependency packages can be definition targets, but sources outside the
  current workspace are not editable rename/reference targets.
- Inlay hints intentionally cover conservative syntax-level cases, not every
  inferred compiler type.
- Formatting returns no edit for invalid syntax; diagnostics remain the source
  of parse errors.
- The cache execute commands `nomo.cache.stats` and `nomo.cache.clear` are
  Preview extensions and may change with snapshots.
- Editor UI, marketplace packaging, and extension-specific fallback lexers
  belong to their editor repositories.

## Editor integrations

- [VS Code](https://github.com/nomo-lang/vscode-nomo)
- [Zed](https://github.com/nomo-lang/zed-nomo)
- [IntelliJ Platform](https://github.com/nomo-lang/intellij-nomo)

Each client must document the server/compiler commit it embeds or expects.
Highlighting from a fallback grammar is not evidence that compiler-backed
diagnostics, navigation, or formatting are available.

## Development checks

Use a recent stable Rust toolchain with Rust 2024 edition support:

```sh
cargo fmt --check
python3 scripts/check_syntax_governance.py
cargo clippy --locked -- -D warnings
cargo test --locked
cargo build --locked --release
python3 scripts/lsp_release_gate.py \
  --lsp target/release/nomo-lsp \
  --output performance/results/lsp-release-gate.json
```

The release gate launches the built server and verifies initialize,
diagnostics, completion, shutdown, and latency bounds. A compile-only result is
not editor-integration evidence.

When updating the compiler pin:

1. merge the RFC-first compiler change and wait for its required CI;
2. set both Git dependencies in `Cargo.toml` to the same immutable commit;
3. update `Cargo.lock`;
4. review every surface listed in [`AGENTS.md`](AGENTS.md);
5. run the complete checks above on a signed feature branch.

## Releases

The release workflow builds Linux x86-64, macOS Intel and Apple silicon, and
Windows x86-64 archives. It runs the real protocol gate, packages matching
standard-library sources, emits checksums, and attests artifacts. Timestamped
`v0.0.0-*` tags are prereleases.

## Authoritative documentation

- [Nomo specification](https://github.com/nomo-lang/rfcs/blob/main/en/SPEC.md)
- [中文语言规范](https://github.com/nomo-lang/rfcs/blob/main/zh-CN/SPEC.md)
- [RFC index](https://github.com/nomo-lang/rfcs)
- [Roadmap](https://github.com/nomo-lang/rfcs/blob/main/ROADMAP.md)
- [Compiler and CLI](https://github.com/nomo-lang/nomo)
- [Shared contribution guide](https://github.com/nomo-lang/.github/blob/main/CONTRIBUTING.md)

Repository-specific compiler-pin and verification rules are in
[`AGENTS.md`](AGENTS.md).

## License

MIT. See [LICENSE](LICENSE).
