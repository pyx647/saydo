"""本地服务（只监听 127.0.0.1）：静态界面 + /api/<cmd>(命令) + /events(SSE) + /call(内置 MCP 的桥)。
安全：所有 API 都要令牌（UI 令牌放在 Edge 窗口的 URL 里；MCP 桥用另一个令牌，只写在 server.json 里）；
校验 Host（防 DNS 重绑定）和 Origin（防网页跨站请求）。否则任何网页都能向 127.0.0.1 发请求来驱动你的 Agent。"""
import json
import mimetypes
import os
import queue
import secrets
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlparse

from . import config, tools, ui

DIST = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "dist")
COMMANDS = {}
UI_TOKEN = secrets.token_urlsafe(24)
BRIDGE_TOKEN = secrets.token_urlsafe(24)
PORT = 0


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "SayDo"

    def log_message(self, *a):
        pass

    # —— 安全校验 ——
    def _guard(self) -> bool:
        host = (self.headers.get("Host") or "").lower()
        if host not in (f"127.0.0.1:{PORT}", f"localhost:{PORT}"):
            self._send(403, b"bad host", "text/plain")
            return False
        origin = self.headers.get("Origin")
        if origin and origin not in (f"http://127.0.0.1:{PORT}", f"http://localhost:{PORT}"):
            self._send(403, b"bad origin", "text/plain")
            return False
        return True

    def _send(self, code, body: bytes, ctype="application/json; charset=utf-8", extra=None):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def _json(self, obj, code=200):
        self._send(code, json.dumps(obj, ensure_ascii=False).encode("utf-8"))

    # —— GET：静态文件 + SSE ——
    def do_GET(self):
        if not self._guard():
            return
        u = urlparse(self.path)
        if u.path == "/events":
            if parse_qs(u.query).get("t", [""])[0] != UI_TOKEN:
                return self._send(403, b"forbidden", "text/plain")
            return self._sse()
        rel = unquote(u.path).lstrip("/") or "index.html"
        full = os.path.normpath(os.path.join(DIST, rel))
        if not full.startswith(DIST) or not os.path.isfile(full):
            full = os.path.join(DIST, "index.html")
        if not os.path.isfile(full):
            return self._send(404, "界面还没构建：在 saydo 目录运行 pnpm build".encode("utf-8"), "text/plain; charset=utf-8")
        with open(full, "rb") as f:
            data = f.read()
        self._send(200, data, mimetypes.guess_type(full)[0] or "application/octet-stream")

    def _sse(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        q = ui.subscribe()
        try:
            self.wfile.write(b": hello\n\n")
            self.wfile.flush()
            while True:
                try:
                    data = q.get(timeout=15)
                    self.wfile.write(b"data: " + data.encode("utf-8") + b"\n\n")
                except queue.Empty:
                    self.wfile.write(b": ping\n\n")
                self.wfile.flush()
        except (BrokenPipeError, ConnectionError, OSError):
            pass
        finally:
            ui.unsubscribe(q)

    # —— POST：命令 / 桥 ——
    def do_POST(self):
        if not self._guard():
            return
        n = int(self.headers.get("Content-Length") or 0)
        try:
            body = json.loads(self.rfile.read(n).decode("utf-8") or "{}")
        except ValueError:
            body = {}
        tok = self.headers.get("X-Token", "")
        if self.path == "/call":
            if not secrets.compare_digest(tok, BRIDGE_TOKEN):
                return self._send(403, b"forbidden", "text/plain")
            try:
                return self._json({"ok": True, "content": tools.dispatch(body.get("tool", ""), body.get("args") or {})})
            except Exception as e:
                return self._json({"ok": False, "error": str(e)})
        if self.path.startswith("/api/"):
            if not secrets.compare_digest(tok, UI_TOKEN):
                return self._send(403, b"forbidden", "text/plain")
            fn = COMMANDS.get(self.path[5:])
            if not fn:
                return self._json({"ok": False, "error": "未知命令"}, 404)
            try:
                return self._json({"ok": True, "result": fn(body)})
            except Exception as e:
                return self._json({"ok": False, "error": str(e)})
        self._send(404, b"not found", "text/plain")


class _Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = False


def start(commands: dict) -> int:
    global PORT
    COMMANDS.update(commands)
    srv = _Server(("127.0.0.1", 0), Handler)
    PORT = srv.server_address[1]
    with open(os.path.join(config.config_dir(), "server.json"), "w", encoding="utf-8") as f:
        json.dump({"port": PORT, "ui_token": UI_TOKEN, "bridge_token": BRIDGE_TOKEN, "pid": os.getpid()}, f)
    ui.WIN.base_url = f"http://127.0.0.1:{PORT}"
    ui.WIN.token = UI_TOKEN
    threading.Thread(target=srv.serve_forever, daemon=True, name="http").start()
    return PORT
