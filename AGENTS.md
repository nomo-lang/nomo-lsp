# Nomo LSP repository guide

## Scope

This repository owns the standalone language server: LSP transport, editor-facing
diagnostics, completion, navigation, hover, symbols, formatting, rename, and
release-gate latency checks. Compiler parsing, type checking, canonical signature
rendering, project manifests, module graphs, and the standard library belong in
the `nomo` repository.

## Compiler revision

- `nomo` and `nomo-lsp-bridge` must use the same immutable Git revision.
- Syntax or semantic work starts in `nomo` under an RFC-first change. Update the
  pinned revision only after the compiler PR is merged and its required CI passes.
- Commit `Cargo.toml` and `Cargo.lock` together. Do not point either dependency at
  an unmerged branch.

## Syntax-change synchronization

For every syntax or signature change, review diagnostics, formatting, hover,
  signature help, document/workspace symbols, completion snippets, navigation,
  code actions, semantic tokens, inlay hints, and embedded test fixtures.
- Derive project module identity from `nomo.toml` through the compiler project
  context. A dependency alias is an import concern and must not change the
  dependency's source package declaration.
- Preserve explicitly documented migration compatibility, but make canonical
  output match the compiler formatter and shared LSP bridge.

## Required verification

Run from the repository root:

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

The release gate must launch the built server and verify initialize, diagnostics,
completion, shutdown, and the recorded latency bounds. Never treat a compile-only
check as editor integration evidence.

## Delivery

Use a feature branch, signed commits, a PR, protected CI, and a merge. Restore a
clean `main` with `HEAD == origin/main` before handing the repository to another
task.
