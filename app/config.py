"""配置：%APPDATA%/com.user.saydo/config.json（和 Tauri 版同一个目录，语音模型共用）。键名与前端 api.ts 的 Config 一致(camelCase)。"""
import copy
import json
import os
import threading

APP_ID = "com.user.saydo"


def config_dir() -> str:
    d = os.path.join(os.environ.get("APPDATA") or os.path.expanduser("~"), APP_ID)
    os.makedirs(d, exist_ok=True)
    return d


DEFAULT = {
    "hotkey": "Ctrl+Alt+Space",
    "backend": "claude",
    "lang": "auto",
    "sttEngine": "local",
    "tapOpensInput": True,
    "autoCloseSec": 20,
    "approval": "balanced",
    "fullAccess": True,
    "keepHooks": False,
    "warmClaude": True,
    "extraPrompt": "",
    "claude": {"path": "", "model": "", "extraArgs": ""},
    "codex": {"path": "", "model": "", "extraArgs": ""},
    "agy": {"path": "", "model": "", "extraArgs": ""},
    "openai": {"baseUrl": "https://api.openai.com/v1", "apiKey": "", "model": "gpt-5-mini"},
    "cloudStt": {"baseUrl": "https://api.openai.com/v1", "apiKey": "", "model": "gpt-4o-mini-transcribe"},
    "recentFolders": [],
}

_lock = threading.RLock()
_cfg = copy.deepcopy(DEFAULT)


def _merge(base: dict, over: dict) -> dict:
    out = copy.deepcopy(base)
    for k, v in over.items():
        if k in out and isinstance(out[k], dict) and isinstance(v, dict):
            out[k] = _merge(out[k], v)
        elif k in out:
            out[k] = v
    return out


def path() -> str:
    return os.path.join(config_dir(), "config.json")


def load() -> bool:
    """返回 True 表示这是首次运行（没有配置文件）"""
    global _cfg
    first = not os.path.exists(path())
    if not first:
        try:
            with open(path(), encoding="utf-8-sig") as f:
                with _lock:
                    _cfg = _merge(DEFAULT, json.load(f))
        except Exception:
            pass
    else:
        save(_cfg)
    return first


def get() -> dict:
    with _lock:
        return copy.deepcopy(_cfg)


def save(c: dict) -> None:
    global _cfg
    with _lock:
        _cfg = _merge(DEFAULT, c)
        tmp = path() + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(_cfg, f, ensure_ascii=False, indent=2)
        os.replace(tmp, path())


def remember_folder(folder: str) -> None:
    c = get()
    rf = [f for f in c["recentFolders"] if f.lower() != folder.lower()]
    rf.insert(0, folder)
    c["recentFolders"] = rf[:8]
    save(c)
