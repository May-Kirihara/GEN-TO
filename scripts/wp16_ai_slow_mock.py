#!/usr/bin/env python3
"""WP-16 S6用: 遅延付きOpenAI互換ローカルmock AI endpoint。

各chat/completions応答を 3〜10秒遅延させて「AIジョブが詰まっている」状態を
作り、その間のUI応答性 (グリッド/検索/比較) を計測するためのもの。
Python標準ライブラのみ (依存なし)。

- GET  /v1/models          → {"data":[{"id":"mock-vision"}]} (遅延なし)
- POST /v1/chat/completions → 200 + 有効なstructured応答 (3-10s遅延。
  response_formatを含む場合はJSON文字列content)
- それ以外 → 404

使い方: python3 scripts/wp16_ai_slow_mock.py [port] (既定 8765)
ログ: 要求ごとに1行 (時刻・path・遅延ms) をstdoutへ。
"""
from __future__ import annotations

import json
import random
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

rng = random.Random(20260903)


def make_handler():
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, fmt, *args):  # 既定ログを静音化
            pass

        def _send(self, status: int, body: bytes, ctype: str = "application/json"):
            self.send_response(status)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):  # noqa: N802
            if self.path.rstrip("/").endswith("/models"):
                body = json.dumps({
                    "object": "list",
                    "data": [{"id": "mock-vision", "object": "model", "owned_by": "wp16"}],
                }).encode()
                self._send(200, body)
                print(f"{time.strftime('%H:%M:%S')} GET {self.path} -> 200", flush=True)
                return
            self._send(404, b'{"error":"not found"}')

        def do_POST(self):  # noqa: N802
            length = int(self.headers.get("Content-Length", 0))
            req_body = self.rfile.read(length) if length else b""
            delay_ms = rng.randint(3000, 10000)
            structured = b'"response_format"' in req_body
            print(f"{time.strftime('%H:%M:%S')} POST {self.path} "
                  f"delay={delay_ms}ms structured={structured} bytes={length}", flush=True)
            time.sleep(delay_ms / 1000.0)
            if "chat/completions" not in self.path:
                self._send(404, b'{"error":"not found"}')
                return
            if structured:
                content = json.dumps({
                    "tags": ["wp16", "mock"],
                    "caption": "遅延モック応答 (WP-16 S6)",
                }, ensure_ascii=False)
            else:
                # response_format非対応endpoint向け: 本文自体をJSON文字列にする
                # (プロンプトでJSONを要求する運用 — GEN-TOは寛容に抽出する)
                content = json.dumps({
                    "tags": ["wp16", "mock"],
                    "caption": "遅延モック応答 (WP-16 S6)",
                }, ensure_ascii=False)
            resp = {
                "id": "chatcmpl-wp16",
                "object": "chat.completion",
                "created": int(time.time()),
                "model": "mock-vision",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20},
            }
            self._send(200, json.dumps(resp, ensure_ascii=False).encode())

    return Handler


def main() -> None:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    server = ThreadingHTTPServer(("127.0.0.1", port), make_handler())
    print(f"slow mock listening on 127.0.0.1:{port} (chat 3-10s delay)", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
