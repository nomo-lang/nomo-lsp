# LSP release-gate baseline

`scripts/lsp_release_gate.py` launches the release binary over stdio, completes
the initialize handshake, opens an invalid Nomo project, requires diagnostics,
requests cold and warm completion, applies a valid full-document edit, verifies
versioned diagnostics are cleared, and requests completion again. It also calls
`nomo.cache.stats` and requires both a warm-query hit and edit invalidation. CI
uploads the measured latency JSON for every run and fails when a latency exceeds
`release-gate-thresholds.json`.

Cold thresholds remain broad preview regression guards. Warm completion and
post-edit thresholds are the first RFC 0016 incremental budgets; representative
large-workspace traces and persistent-cache budgets remain future slices.
