#!/usr/bin/env python3
"""Launch nomo-lsp over stdio and record protocol latency evidence."""

from __future__ import annotations

import argparse
import json
import platform
import queue
import subprocess
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable


class LspSession:
    def __init__(self, executable: Path) -> None:
        self.process = subprocess.Popen(
            [str(executable)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.messages: queue.Queue[dict[str, Any] | BaseException] = queue.Queue()
        self.reader = threading.Thread(target=self._read_messages, daemon=True)
        self.reader.start()

    def _read_messages(self) -> None:
        assert self.process.stdout is not None
        try:
            while True:
                headers: dict[str, str] = {}
                while True:
                    line = self.process.stdout.readline()
                    if not line:
                        return
                    if line == b"\r\n":
                        break
                    name, value = line.decode("ascii").split(":", 1)
                    headers[name.lower()] = value.strip()
                length = int(headers["content-length"])
                payload = self.process.stdout.read(length)
                if len(payload) != length:
                    raise RuntimeError("nomo-lsp closed stdout during a protocol message")
                self.messages.put(json.loads(payload))
        except BaseException as error:  # Propagate reader failures to the main thread.
            self.messages.put(error)

    def send(self, message: dict[str, Any]) -> None:
        assert self.process.stdin is not None
        payload = json.dumps(message, separators=(",", ":")).encode("utf-8")
        self.process.stdin.write(f"Content-Length: {len(payload)}\r\n\r\n".encode("ascii"))
        self.process.stdin.write(payload)
        self.process.stdin.flush()

    def wait_for(
        self,
        predicate: Callable[[dict[str, Any]], bool],
        timeout_seconds: float,
    ) -> dict[str, Any]:
        deadline = time.monotonic() + timeout_seconds
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("timed out waiting for a nomo-lsp protocol response")
            item = self.messages.get(timeout=remaining)
            if isinstance(item, BaseException):
                raise item
            if predicate(item):
                return item

    def close(self) -> None:
        if self.process.poll() is not None:
            return
        try:
            self.send({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": None})
            self.wait_for(lambda message: message.get("id") == 3, 2.0)
            self.send({"jsonrpc": "2.0", "method": "exit", "params": None})
            self.process.wait(timeout=2.0)
        except (BrokenPipeError, TimeoutError, subprocess.TimeoutExpired):
            self.process.terminate()
            self.process.wait(timeout=2.0)


def elapsed_ms(start: float) -> float:
    return round((time.perf_counter() - start) * 1000.0, 3)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--lsp", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--thresholds",
        type=Path,
        default=Path(__file__).resolve().parents[1]
        / "performance"
        / "release-gate-thresholds.json",
    )
    args = parser.parse_args()
    thresholds = json.loads(args.thresholds.read_text(encoding="utf-8"))

    with tempfile.TemporaryDirectory(prefix="nomo-lsp-release-gate-") as temporary:
        root = Path(temporary)
        source = root / "src" / "main.nomo"
        source.parent.mkdir()
        (root / "nomo.toml").write_text(
            '[package]\nnamespace = "release-gate"\nname = "lsp"\n'
            'version = "0.0.0-20260713145859"\nedition = "2026"\n',
            encoding="utf-8",
        )
        text = (
            "package app.main\n\n"
            "fn main() -> void {\n"
            '    let value: i64 = "not-an-integer"\n'
            "}\n"
        )
        source.write_text(text, encoding="utf-8")
        root_uri = root.resolve().as_uri()
        source_uri = source.resolve().as_uri()
        session = LspSession(args.lsp.resolve())
        try:
            started = time.perf_counter()
            session.send(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "processId": None,
                        "rootUri": root_uri,
                        "capabilities": {},
                        "workspaceFolders": [{"uri": root_uri, "name": "release-gate"}],
                    },
                }
            )
            initialize = session.wait_for(lambda message: message.get("id") == 1, 10.0)
            initialize_ms = elapsed_ms(started)
            if "error" in initialize:
                raise RuntimeError(f"initialize failed: {initialize['error']}")
            session.send({"jsonrpc": "2.0", "method": "initialized", "params": {}})

            started = time.perf_counter()
            session.send(
                {
                    "jsonrpc": "2.0",
                    "method": "textDocument/didOpen",
                    "params": {
                        "textDocument": {
                            "uri": source_uri,
                            "languageId": "nomo",
                            "version": 1,
                            "text": text,
                        }
                    },
                }
            )
            diagnostics = session.wait_for(
                lambda message: message.get("method") == "textDocument/publishDiagnostics"
                and message.get("params", {}).get("uri") == source_uri,
                10.0,
            )
            diagnostics_ms = elapsed_ms(started)
            diagnostic_items = diagnostics.get("params", {}).get("diagnostics", [])
            if not diagnostic_items:
                raise RuntimeError("nomo-lsp did not publish the expected invalid-program diagnostic")

            started = time.perf_counter()
            session.send(
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "textDocument/completion",
                    "params": {
                        "textDocument": {"uri": source_uri},
                        "position": {"line": 3, "character": 8},
                    },
                }
            )
            completion = session.wait_for(lambda message: message.get("id") == 2, 10.0)
            completion_ms = elapsed_ms(started)
            completion_items = completion.get("result") or []
            if isinstance(completion_items, dict):
                completion_items = completion_items.get("items", [])
            if not completion_items:
                raise RuntimeError("nomo-lsp returned no completion items")

            started = time.perf_counter()
            session.send(
                {
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "textDocument/completion",
                    "params": {
                        "textDocument": {"uri": source_uri},
                        "position": {"line": 3, "character": 8},
                    },
                }
            )
            warm_completion = session.wait_for(
                lambda message: message.get("id") == 4, 10.0
            )
            warm_completion_ms = elapsed_ms(started)
            if not (warm_completion.get("result") or []):
                raise RuntimeError("nomo-lsp returned no warm completion items")

            valid_text = (
                "package app.main\n\n"
                "fn main() -> void {\n"
                "    let value: i64 = 1\n"
                "}\n"
            )
            started = time.perf_counter()
            session.send(
                {
                    "jsonrpc": "2.0",
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": {"uri": source_uri, "version": 2},
                        "contentChanges": [{"text": valid_text}],
                    },
                }
            )
            edited_diagnostics = session.wait_for(
                lambda message: message.get("method")
                == "textDocument/publishDiagnostics"
                and message.get("params", {}).get("uri") == source_uri
                and message.get("params", {}).get("version") == 2,
                10.0,
            )
            incremental_edit_diagnostics_ms = elapsed_ms(started)
            if edited_diagnostics.get("params", {}).get("diagnostics"):
                raise RuntimeError("nomo-lsp retained stale diagnostics after a valid edit")

            started = time.perf_counter()
            session.send(
                {
                    "jsonrpc": "2.0",
                    "id": 5,
                    "method": "textDocument/completion",
                    "params": {
                        "textDocument": {"uri": source_uri},
                        "position": {"line": 3, "character": 8},
                    },
                }
            )
            post_edit_completion = session.wait_for(
                lambda message: message.get("id") == 5, 10.0
            )
            post_edit_completion_ms = elapsed_ms(started)
            if not (post_edit_completion.get("result") or []):
                raise RuntimeError("nomo-lsp returned no completion items after an edit")

            session.send(
                {
                    "jsonrpc": "2.0",
                    "id": 6,
                    "method": "workspace/executeCommand",
                    "params": {"command": "nomo.cache.stats", "arguments": []},
                }
            )
            cache_stats_response = session.wait_for(
                lambda message: message.get("id") == 6, 10.0
            )
            cache_stats = cache_stats_response.get("result") or {}
            if cache_stats.get("hits", 0) < 1:
                raise RuntimeError("nomo-lsp incremental cache recorded no warm-query hit")
            if cache_stats.get("invalidations", 0) < 1:
                raise RuntimeError("nomo-lsp incremental cache recorded no edit invalidation")

            result = {
                "schema": 2,
                "platform": platform.platform(),
                "server": initialize.get("result", {}).get("serverInfo", {}),
                "measurements_ms": {
                    "initialize": initialize_ms,
                    "publish_diagnostics": diagnostics_ms,
                    "completion": completion_ms,
                    "warm_completion": warm_completion_ms,
                    "incremental_edit_diagnostics": incremental_edit_diagnostics_ms,
                    "post_edit_completion": post_edit_completion_ms,
                },
                "diagnostic_count": len(diagnostic_items),
                "completion_count": len(completion_items),
                "cache": cache_stats,
                "thresholds_ms": thresholds,
            }
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
            print(json.dumps(result, indent=2))

            failures = [
                f"{name} took {value}ms (limit {thresholds[name]}ms)"
                for name, value in result["measurements_ms"].items()
                if value > thresholds[name]
            ]
            if failures:
                raise RuntimeError("; ".join(failures))
        finally:
            session.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
