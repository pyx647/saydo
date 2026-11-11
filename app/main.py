"""SayDo 主入口：热键钩子 + 本地服务 + 弹窗/设置窗口 + 托盘。"""
import json
import os
import subprocess
import sys
import threading
import urllib.request
import winreg

from . import agent, config, history, server, session, stt, ui, winapi as w
from .tray import Tray

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOOK = None
TRAY = None
RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"


def _log_to_file():
    """pythonw 没有控制台：把 stdout/stderr 写进日志文件，方便排错"""
    if sys.stdout is None or os.environ.get("SAYDO_CONSOLE") != "1":
        p = os.path.join(config.config_dir(), "saydo.log")
        f = open(p, "a", encoding="utf-8", buffering=1)
        sys.stdout = sys.stderr = f


# ───────────────────────── 命令表（前端 invoke 的目标） ─────────────────────────

def _cli_info(cid, ov):
    exe = agent.find_exe(cid, ov)
    ver = ""
    if exe:
        try:
            r = subprocess.run([exe, "--version"], capture_output=True, timeout=6, creationflags=w.CREATE_NO_WINDOW)
            ver = (r.stdout.decode("utf-8", "ignore").splitlines() or [""])[0].strip()
        except Exception:
            pass
    return exe, ver


def detect_clis(_):
    cfg = config.get()
    out, threads = {}, []
    names = {"claude": "Claude Code", "codex": "Codex CLI", "agy": "Antigravity (agy)"}

    def one(cid):
        exe, ver = _cli_info(cid, cfg[cid]["path"])
        out[cid] = {"id": cid, "name": names[cid], "found": bool(exe), "path": exe or "", "version": ver}

    for cid in names:
        t = threading.Thread(target=one, args=(cid,))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()
    return [out[c] for c in names]


def stt_download(_):
    import urllib.request as ur
    base = "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main"
    d = stt.model_dir()
    for name in ("tokens.txt", "model.int8.onnx"):
        dest = os.path.join(d, name)
        if os.path.exists(dest):
            continue
        with ur.urlopen(f"{base}/{name}", timeout=60) as r, open(dest + ".tmp", "wb") as f:
            total, done = int(r.headers.get("Content-Length") or 0), 0
            while True:
                chunk = r.read(1 << 16)
                if not chunk:
                    break
                f.write(chunk)
                done += len(chunk)
                ui.emit("stt-download-progress", {"file": name, "done": done, "total": total})
        os.replace(dest + ".tmp", dest)
    ui.emit("stt-download-done", {})
    return None


def _autostart_cmd() -> str:
    pyw = sys.executable.replace("python.exe", "pythonw.exe")
    return f'"{pyw}" "{os.path.join(ROOT, "saydo.pyw")}"'


def autostart_get(_):
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY) as k:
            winreg.QueryValueEx(k, "SayDo")
            return True
    except OSError:
        return False


def autostart_set(a):
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
        if a.get("on"):
            winreg.SetValueEx(k, "SayDo", 0, winreg.REG_SZ, _autostart_cmd())
        else:
            try:
                winreg.DeleteValue(k, "SayDo")
            except OSError:
                pass


def save_config(a):
    config.save(a["config"])
    HOOK.set_spec(config.get()["hotkey"])
    threading.Thread(target=agent.prewarm, daemon=True).start()
    ui.emit("config-changed", {})


def set_paused(a):
    HOOK.paused = bool(a.get("paused"))
    ui.emit("paused-changed", HOOK.paused)


def panel_hide(_):
    session.cancel_current()
    ui.WIN.hide_panel()
    ui.state({"phase": "idle"})


def commands():
    cmds = _commands()
    if os.environ.get("SAYDO_TEST") == "1":  # 仅测试：让外部脚本直接驱动弹窗，便于做界面验证
        cmds["debug_show"] = lambda a: ui.WIN.show_panel_at((a.get("x", 700), a.get("y", 300)))
        cmds["debug_emit"] = lambda a: ui.emit(a["event"], a.get("payload"))
    return cmds


def _commands():
    return {
        "get_config": lambda a: config.get(),
        "save_config": save_config,
        "detect_clis": detect_clis,
        "stt_status": lambda a: {"ready": stt.model_ready(), "dir": stt.model_dir()},
        "stt_download_model": stt_download,
        "app_info": lambda a: {"exe": os.path.join(ROOT, "saydo.pyw"), "configDir": config.config_dir(), "version": "0.1.0", "paused": HOOK.paused},
        "set_paused": set_paused,
        "show_settings_cmd": lambda a: threading.Thread(target=ui.WIN.show_settings, daemon=True).start(),
        "record_hotkey": lambda a: HOOK.record(),
        "history_list": lambda a: history.list_all(),
        "history_clear": lambda a: history.clear(),
        "panel_submit": lambda a: session.submit_text(a.get("text", "")),
        "panel_replace": lambda a: session.replace_original(a.get("text", "")),
        "test_backend": lambda a: session.test_backend(a.get("backend")),
        "panel_layout": lambda a: ui.WIN.on_layout(a),
        "panel_hide": panel_hide,
        "panel_drag": lambda a: ui.WIN.drag(),
        "panel_focus": lambda a: ui.WIN.focus_panel(),
        "ask_reply": lambda a: ui.ask_reply(a["id"], a.get("value")),
        "folder_suggestions": lambda a: ui.folder_suggestions(),
        "browse_folder": lambda a: ui.browse_folder(a.get("start") or "") or None,
        "open_path": lambda a: ui.open_path(a["path"]),
        "reveal_path": lambda a: ui.reveal_path(a["path"]),
        "autostart_get": autostart_get,
        "autostart_set": autostart_set,
    }


# ───────────────────────── 启动 ─────────────────────────

def _already_running() -> bool:
    import win32api, win32event, winerror
    global _MUTEX
    _MUTEX = win32event.CreateMutex(None, False, "SayDo_single_instance")
    return win32api.GetLastError() == winerror.ERROR_ALREADY_EXISTS


def _poke_existing():
    try:
        with open(os.path.join(config.config_dir(), "server.json"), encoding="utf-8") as f:
            info = json.load(f)
        req = urllib.request.Request(f"http://127.0.0.1:{info['port']}/api/show_settings_cmd", data=b"{}",
                                     headers={"X-Token": info["ui_token"], "Content-Type": "application/json"})
        urllib.request.urlopen(req, timeout=3).read()
    except Exception:
        pass


def main():
    global HOOK, TRAY
    w.set_dpi_aware()
    _log_to_file()
    if _already_running():
        _poke_existing()
        return
    first = config.load()
    HOOK = w.HotkeyHook(session.on_down, session.on_up, allow_injected=os.environ.get("SAYDO_TEST") == "1")
    HOOK.set_spec(config.get()["hotkey"])
    server.start(commands())
    if not HOOK.start():
        print("[main] 全局热键钩子安装失败", flush=True)

    def warm():
        try:
            ui.WIN.ensure_panel()
        except Exception as e:
            print("[main] 弹窗窗口启动失败:", e, flush=True)
        stt.warm()
        agent.prewarm()
        if first:
            ui.WIN.show_settings()

    threading.Thread(target=warm, daemon=True, name="warm").start()

    def toggle_pause():
        HOOK.paused = not HOOK.paused
        ui.emit("paused-changed", HOOK.paused)

    def on_quit():
        agent.WARM.kill()
        session.cancel_current()
        ui.WIN.close_all()
        HOOK.stop()

    icon = os.path.join(ROOT, "assets", "icon.ico")   # SayDo 自己的图标（assets/make_icon.py 生成）
    TRAY = Tray(icon, lambda: threading.Thread(target=ui.WIN.show_settings, daemon=True).start(), toggle_pause, lambda: HOOK.paused, on_quit)
    TRAY.run()
    os._exit(0)


if __name__ == "__main__":
    main()
