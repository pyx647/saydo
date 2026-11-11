"""端到端：真实启动 SayDo → 合成语音代替麦克风 → 在我自己的测试窗口里选中文字 → 模拟按住热键 → 观察事件流直到出结果。
只对"我自己开的测试窗口"发按键（先校验前台就是它），不会碰你正在用的窗口。
用法：python tests/e2e.py [指令文字] [选中文字]   环境变量 E2E_BACKEND=claude|codex|agy 选择后端"""
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import tkinter as tk
import urllib.request
import wave

import win32com.client

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
from app import config, winapi as w  # noqa: E402

w.set_dpi_aware()
SAY = sys.argv[1] if len(sys.argv) > 1 else "把这段话翻译成英文"
SELECTED = sys.argv[2] if len(sys.argv) > 2 else "今天的实验结果非常好，细菌在三天内长满了平板。"
BACKEND = os.environ.get("E2E_BACKEND", "claude")
LOG = os.path.join(tempfile.gettempdir(), "saydo_e2e_app.log")

# 1) 合成语音（16kHz 单声道）
v = win32com.client.Dispatch("SAPI.SpVoice")
vs = v.GetVoices()
for i in range(vs.Count):
    if "Chinese" in vs.Item(i).GetDescription() or "Huihui" in vs.Item(i).GetDescription():
        v.Voice = vs.Item(i)
wav_path = os.path.join(tempfile.gettempdir(), "saydo_e2e.wav")
fs = win32com.client.Dispatch("SAPI.SpFileStream")
fmt = win32com.client.Dispatch("SAPI.SpAudioFormat")
fmt.Type = 18  # SAFT16kHz16BitMono
fs.Format = fmt
fs.Open(wav_path, 3)
v.AudioOutputStream = fs
v.Speak(SAY)
fs.Close()
with wave.open(wav_path) as f:
    print(f"合成语音：{f.getnframes() / f.getframerate():.1f}s @ {f.getframerate()}Hz  「{SAY}」", flush=True)

# 2) 备份并临时改配置（后端），结束后还原
orig_cfg = config.get()
cfg = dict(orig_cfg, backend=BACKEND)
config.save(cfg)

# 3) 启动 SayDo
env = dict(os.environ, SAYDO_TEST="1", SAYDO_TEST_WAV=wav_path, SAYDO_CONSOLE="1", PYTHONIOENCODING="utf-8")
sj = os.path.join(config.config_dir(), "server.json")
if os.path.exists(sj):
    os.remove(sj)
logf = open(LOG, "w", encoding="utf-8")
app = subprocess.Popen([sys.executable, "-m", "app.main"], cwd=ROOT, env=env, stdout=logf, stderr=subprocess.STDOUT)
info = None
for _ in range(80):
    time.sleep(0.25)
    try:
        info = json.load(open(sj, encoding="utf-8"))
        if info.get("pid") == app.pid:
            break
    except Exception:
        pass
assert info and info.get("pid") == app.pid, "SayDo 没有启动：" + open(LOG, encoding="utf-8").read()[-800:]
base = f"http://127.0.0.1:{info['port']}"
print("SayDo 已启动，端口", info["port"], flush=True)

# 4) 订阅事件流
events = []


def sse():
    r = urllib.request.urlopen(f"{base}/events?t={info['ui_token']}", timeout=400)
    for raw in r:
        line = raw.decode("utf-8", "ignore").strip()
        if line.startswith("data:"):
            e = json.loads(line[5:])
            events.append((time.time(), e))


threading.Thread(target=sse, daemon=True).start()
# 等弹窗窗口(Edge)就绪
from app import ui  # noqa: E402

for _ in range(120):
    if ui.find_window(ui.PANEL_TITLE):
        break
    time.sleep(0.25)
print("弹窗窗口就绪:", bool(ui.find_window(ui.PANEL_TITLE)), flush=True)
time.sleep(1.0)

# 5) 测试窗口
result = {}


def snap_panel(tag):
    """只抓弹窗窗口的"可见区域"（SetWindowRgn 裁剪后的包围盒），用来检查真实 Edge 窗口里的样子"""
    import ctypes
    import ctypes.wintypes as wt
    from PIL import ImageGrab
    import win32gui
    h = ui.find_window(ui.PANEL_TITLE)
    if not h or not win32gui.IsWindowVisible(h):
        print(f"[snap {tag}] 弹窗不可见", flush=True)
        return
    l, t, r, b = win32gui.GetWindowRect(h)
    box = wt.RECT()
    ctypes.windll.gdi32.GetRgnBox  # noqa
    kind = ctypes.windll.user32.GetWindowRgnBox(h, ctypes.byref(box))
    if kind and box.right > box.left:
        l, t, r, b = l + box.left, t + box.top, l + box.right, t + box.bottom
    img = ImageGrab.grab(bbox=(l - 6, t - 6, r + 6, b + 6), all_screens=True)
    out = os.path.join(tempfile.gettempdir(), f"saydo_panel_{tag}.png")
    img.save(out)
    print(f"[snap {tag}] {r - l}x{b - t} -> {out}", flush=True)


def driver(root):
    try:
        time.sleep(0.5)
        hwnd = w.user32.GetParent(int(root.winfo_id())) or int(root.winfo_id())
        w.force_foreground(hwnd)
        fg = w.foreground()
        if fg["pid"] != os.getpid():
            result["abort"] = f"前台不是测试窗口：{fg['exe']} {fg['title']!r}"
            return
        t0 = time.time()
        # 模拟按下 Ctrl+Alt+Space（SayDo 以 allow_injected 接收）
        for vk in (0x11, 0x12, 0x20):
            w.send_keys([(vk, False)], tag=False)
            time.sleep(0.04)
        time.sleep(1.6)
        snap_panel("listening")
        time.sleep(0.8)  # 按住说话
        for vk in (0x20, 0x12, 0x11):
            w.send_keys([(vk, True)], tag=False)
            time.sleep(0.04)
        result["released_at"] = time.time() - t0
        # 等结果
        for _ in range(900):
            time.sleep(0.2)
            if any(e["payload"].get("phase") in ("result", "error") for _, e in events if e["event"] == "panel-state"):
                break
        time.sleep(0.9)
        snap_panel("result")
    finally:
        root.after(0, root.destroy)


root = tk.Tk()
root.title("SayDo e2e target")
root.geometry("460x140+400+300")
t = tk.Text(root)
t.pack(fill="both", expand=True)
t.insert("1.0", SELECTED)
t.tag_add("sel", "1.0", "end-1c")
t.focus_set()
threading.Thread(target=driver, args=(root,), daemon=True).start()
root.mainloop()

# 6) 报告
print("\n=== 事件时间线 ===")
t_first = events[0][0] if events else 0
last_sig = None
for ts, e in events:
    if e["event"] != "panel-state":
        continue
    p = e["payload"]
    ph = p.get("phase")
    sig = (ph, p.get("text", "")[:40], tuple(p.get("steps", []))[-1:] if ph == "working" else None)
    if sig == last_sig:
        continue
    last_sig = sig
    extra = ""
    if ph in ("listening", "ctx"):
        c = p.get("ctx", {})
        extra = f"ctx={c.get('kind')} {c.get('label')!r}"
    elif ph == "caption":
        extra = f"字幕={p.get('text')!r}{' (final)' if p.get('final') else ''}"
    elif ph == "heard":
        extra = f"识别={p.get('text')!r}"
    elif ph == "working":
        extra = f"step={(p.get('steps') or [''])[-1]!r} text={p.get('text', '')[:50]!r}"
    elif ph == "result":
        extra = f"text={p.get('text')!r} files={[f['name'] for f in p.get('files', [])]} backend={p.get('backend')} {p.get('elapsed')}s"
    elif ph == "error":
        extra = p.get("message", "")[:300]
    elif ph == "ask":
        extra = f"{p.get('kind')} {p.get('title')!r}"
    print(f"+{ts - t_first:5.1f}s  {ph:12s} {extra}")
if "abort" in result:
    print("ABORT:", result["abort"])
print("\n=== 应用日志（尾部）===")
logf.flush()
print(open(LOG, encoding="utf-8").read()[-1200:])

# 7) 清理：关应用、还原配置
config.save(orig_cfg)
try:
    urllib.request.urlopen(urllib.request.Request(f"{base}/api/panel_hide", data=b"{}", headers={"X-Token": info["ui_token"]}), timeout=3).read()
except Exception:
    pass
ui.WIN.close_all()
app.terminate()
try:
    app.wait(5)
except subprocess.TimeoutExpired:
    app.kill()
subprocess.run(["taskkill", "/F", "/FI", f"PID eq {app.pid}"], capture_output=True)
print("已清理。")
