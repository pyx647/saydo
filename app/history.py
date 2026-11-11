"""历史记录：最近 100 次（指令、结果摘要、后端、结果文件）。"""
import json
import os
import threading
import time

from . import config

_lock = threading.Lock()


def _path() -> str:
    return os.path.join(config.config_dir(), "history.json")


def _load() -> list:
    try:
        with open(_path(), encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return []


def push(instruction: str, result: str, backend: str, context: str, files: list, ok: bool) -> None:
    with _lock:
        v = _load()
        v.insert(0, {"ts": int(time.time()), "instruction": instruction, "result": result[:1500], "backend": backend,
                     "context": context, "files": files, "ok": ok})
        with open(_path(), "w", encoding="utf-8") as f:
            json.dump(v[:100], f, ensure_ascii=False)


def list_all() -> list:
    with _lock:
        return _load()


def clear() -> None:
    with _lock:
        try:
            os.remove(_path())
        except OSError:
            pass
