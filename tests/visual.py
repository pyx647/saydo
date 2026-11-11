"""界面验证：启动 SayDo，用测试入口直接驱动弹窗显示各状态，抓取弹窗可见区域，并通过 Edge 远程调试口查看页面状态。
不发任何按键，不调用 agent。"""
import ctypes
import ctypes.wintypes as wt
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.request

import win32gui
from PIL import ImageGrab

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
from app import config, ui, winapi as w  # noqa: E402

w.set_dpi_aware()
env = dict(os.environ, SAYDO_TEST="1", SAYDO_CONSOLE="1", SAYDO_DEBUG_PORT="9333", PYTHONIOENCODING="utf-8")
sj = os.path.join(config.config_dir(), "server.json")
if os.path.exists(sj):
    os.remove(sj)
log = open(os.path.join(tempfile.gettempdir(), "saydo_visual_app.log"), "w", encoding="utf-8")
app = subprocess.Popen([sys.executable, "-m", "app.main"], cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
info = None
for _ in range(80):
    time.sleep(0.25)
    try:
        info = json.load(open(sj, encoding="utf-8"))
        if info.get("pid") == app.pid:
            break
    except Exception:
        pass
base = f"http://127.0.0.1:{info['port']}"


def api(cmd, **args):
    req = urllib.request.Request(f"{base}/api/{cmd}", data=json.dumps(args).encode(), headers={"X-Token": info["ui_token"], "Content-Type": "application/json"})
    return json.loads(urllib.request.urlopen(req, timeout=10).read())


for _ in range(120):
    if ui.find_window(ui.PANEL_TITLE):
        break
    time.sleep(0.25)
print("弹窗窗口:", bool(ui.find_window(ui.PANEL_TITLE)), flush=True)
time.sleep(2.0)

try:
    targets = json.loads(urllib.request.urlopen("http://127.0.0.1:9333/json", timeout=5).read())
    for t in targets:
        print("CDP target:", t.get("type"), repr(t.get("title")), t.get("url", "")[:70].split("&t=")[0], flush=True)
except Exception as e:
    print("CDP 不可用:", e)


def snap(tag):
    h = ui.find_window(ui.PANEL_TITLE)
    if not h or not win32gui.IsWindowVisible(h):
        print(f"[{tag}] 弹窗不可见")
        return
    l, t, r, b = win32gui.GetWindowRect(h)
    box = wt.RECT()
    if ctypes.windll.user32.GetWindowRgnBox(h, ctypes.byref(box)) and box.right > box.left:
        l, t, r, b = l + box.left, t + box.top, l + box.right, t + box.bottom
    ImageGrab.grab(bbox=(l - 4, t - 4, r + 4, b + 4), all_screens=True).save(os.path.join(tempfile.gettempdir(), f"saydo_vis_{tag}.png"))
    print(f"[{tag}] {r - l}x{b - t}", flush=True)



def cdp_eval(expr):
    """极简 CDP 客户端（只用标准库）：对弹窗页面执行一段 JS 并返回结果"""
    import base64, socket, struct
    targets = json.loads(urllib.request.urlopen("http://127.0.0.1:9333/json", timeout=5).read())
    t = [x for x in targets if x.get("type") == "page" and "w=panel" in x.get("url", "")][0]
    path = t["webSocketDebuggerUrl"].split("9333", 1)[1]
    sock = socket.create_connection(("127.0.0.1", 9333), timeout=5)
    key = base64.b64encode(os.urandom(16)).decode()
    crlf = chr(13) + chr(10)
    req = crlf.join([f"GET {path} HTTP/1.1", "Host: 127.0.0.1:9333", "Upgrade: websocket", "Connection: Upgrade",
                     f"Sec-WebSocket-Key: {key}", "Sec-WebSocket-Version: 13", "", ""])
    sock.sendall(req.encode())
    buf = b""
    while (crlf * 2).encode() not in buf:
        buf += sock.recv(4096)
    msg = json.dumps({"id": 1, "method": "Runtime.evaluate", "params": {"expression": expr, "returnByValue": True}}).encode()
    mask = os.urandom(4)
    hdr = bytes([0x81]) + (bytes([0x80 | len(msg)]) if len(msg) < 126 else bytes([0x80 | 126]) + struct.pack(">H", len(msg)))
    sock.sendall(hdr + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(msg)))
    data = b""
    sock.settimeout(5)
    while True:
        chunk = sock.recv(65536)
        if not chunk:
            break
        data += chunk
        if data.endswith(b"}") or b'"result"' in data:
            break
    sock.close()
    i = data.find(b"{")
    return data[i:].decode("utf-8", "ignore")[:600]

def drive(root=None):
    print(api("debug_show", x=700, y=300))
    time.sleep(0.6)
    api("debug_emit", event="panel-state", payload={"phase": "listening", "ctx": {"kind": "text", "label": "已选中 23 字", "preview": "", "files": [], "app": "x.exe", "title": ""}})
    for i in range(30):
        api("debug_emit", event="audio-level", payload=0.2 + 0.6 * abs((i % 7) - 3) / 3)
        time.sleep(0.04)
    api("debug_emit", event="panel-state", payload={"phase": "caption", "text": "把这段话翻译成英文"})
    time.sleep(1.2)
    try:
        print("CDP:", cdp_eval("JSON.stringify({vis: document.visibilityState, focus: document.hasFocus(), inner: [innerWidth, innerHeight], bodyBg: getComputedStyle(document.body).backgroundColor, text: document.body.innerText.slice(0,60), cls: document.documentElement.className})"), flush=True)
    except Exception as e:
        print("CDP 失败:", e)
    hh = ui.find_window(ui.PANEL_TITLE)
    import win32con
    print("WIN32:", dict(visible=bool(win32gui.IsWindowVisible(hh)), iconic=bool(w.user32.IsIconic(hh)), rect=win32gui.GetWindowRect(hh),
                         topmost=bool(win32gui.GetWindowLong(hh, win32con.GWL_EXSTYLE) & win32con.WS_EX_TOPMOST), fg=w.foreground()["title"][:30],
                         layout=ui.WIN.layout if False else None), flush=True)
    snap("listening")
    api("debug_emit", event="panel-state", payload={"phase": "heard", "text": "把这段话翻译成英文"})
    api("debug_emit", event="panel-state", payload={"phase": "working", "steps": ["交给 Claude Code…"], "stepCount": 1, "text": "", "elapsed": 3})
    time.sleep(1.2)
    snap("working")
    api("debug_emit", event="panel-state", payload={"phase": "result", "text": "Today's experiment results were excellent, with the bacteria completely covering the plate within three days.", "files": [], "canReplace": True, "backend": "claude", "elapsed": 5, "instruction": "把这段话翻译成英文"})
    time.sleep(1.2)
    snap("result")

    # 轻点热键后的输入对话窗（聊天形态）：额外抓整个窗口矩形（不按区域裁剪），便于看标题栏是否漏出
    api("debug_emit", event="panel-state", payload={"phase": "input", "ctx": {"kind": "none", "label": "", "preview": "", "files": [], "app": "x.exe", "title": ""}, "hasPrev": False})
    time.sleep(1.0)
    api("panel_focus")  # 输入窗需要键盘焦点 → 窗口被激活（Chromium 此时可能重设窗口区域）
    hh = ui.find_window(ui.PANEL_TITLE)
    for dt in (0.05, 0.15, 0.4, 0.8, 1.5, 2.0):
        time.sleep(dt)
        bx = wt.RECT()
        k = ctypes.windll.user32.GetWindowRgnBox(hh, ctypes.byref(bx))
        _l, _t, _r, _b = win32gui.GetWindowRect(hh)
        _im = ImageGrab.grab(bbox=(_l, _t, _r, _b), all_screens=True).convert("RGB")
        print(f"  focus+{dt}s rgnbox kind={k}", (bx.left, bx.top, bx.right, bx.bottom), "rect", (_l, _t, _r, _b),
              "strip-pixels", _im.getpixel((220, 20)), _im.getpixel((20, 20)), "card-pixel", _im.getpixel((220, 120)), flush=True)
    import win32con as _wc
    _st = win32gui.GetWindowLong(hh, _wc.GWL_STYLE)
    print("INPUT style: caption=%s thickframe=%s sysmenu=%s popup=%s" % (bool(_st & _wc.WS_CAPTION == _wc.WS_CAPTION), bool(_st & _wc.WS_THICKFRAME), bool(_st & _wc.WS_SYSMENU), bool(_st & _wc.WS_POPUP)), flush=True)
    box = wt.RECT()
    ctypes.windll.user32.GetWindowRgnBox(hh, ctypes.byref(box))
    print("INPUT rect:", win32gui.GetWindowRect(hh), "rgnbox:", (box.left, box.top, box.right, box.bottom), "layout:", ui.WIN.layout, flush=True)
    try:
        print("INPUT CDP:", cdp_eval("JSON.stringify({outer:[outerWidth,outerHeight],inner:[innerWidth,innerHeight],dpr:devicePixelRatio,card:(()=>{const r=document.querySelector('[data-card]')||document.body.firstElementChild;const b=r.getBoundingClientRect();return [b.x,b.y,b.width,b.height]})()})"), flush=True)
    except Exception as e:
        print("CDP 失败:", e)
    snap("input")
    l, t, r, b = win32gui.GetWindowRect(hh)
    ImageGrab.grab(bbox=(l - 4, t - 4, r + 4, b + 4), all_screens=True).save(os.path.join(tempfile.gettempdir(), "saydo_vis_input_full.png"))
    # 诊断：隐藏弹窗后抓同一块屏幕，比较"标题栏那一条"——若一样，说明那条灰色是背后的窗口而不是弹窗自己
    with_panel = ImageGrab.grab(bbox=(l, t, r, b), all_screens=True).convert("RGB")
    api("panel_hide")
    time.sleep(0.6)
    without = ImageGrab.grab(bbox=(l, t, r, b), all_screens=True).convert("RGB")
    without.save(os.path.join(tempfile.gettempdir(), "saydo_vis_input_bg.png"))
    from PIL import ImageChops
    strip = (box.left, 0, box.right, box.top)  # 区域上沿以上、窗口内的那一条
    d = ImageChops.difference(with_panel.crop(strip), without.crop(strip)).getbbox()
    print("STRIP 与背景的差异范围(None=完全一样=没画东西):", d, flush=True)
    api("debug_show", x=700, y=300)
    time.sleep(0.8)
    if os.environ.get("VIS_EXP") == "1":
        # 实验：从外部把窗口区域改成一个 120x120 的小方块，看屏幕上到底会不会被裁（区域是否真的生效）
        hh = ui.find_window(ui.PANEL_TITLE)
        l, t, r, b = win32gui.GetWindowRect(hh)
        time.sleep(0.5)
        base = ImageGrab.grab(bbox=(l, t, r, b), all_screens=True).convert("RGB")
        for top in (39, 50, 60, 80):
            rg = ctypes.windll.gdi32.CreateRectRgn(9, top, 459, 310)
            ctypes.windll.user32.SetWindowRgn(hh, rg, True)
            time.sleep(0.6)
            im = ImageGrab.grab(bbox=(l, t, r, b), all_screens=True).convert("RGB")
            dd = ImageChops.difference(im.crop((9, 0, 459, top)), without.crop((9, 0, 459, top))).getbbox()
            print(f"EXP 区域上沿={top}: 上沿以上与背景的差异 =", dd, flush=True)
        rg = ctypes.windll.gdi32.CreateRectRgn(20, 100, 140, 220)
        ctypes.windll.user32.SetWindowRgn(hh, rg, True)
        time.sleep(0.5)
        small = ImageGrab.grab(bbox=(l, t, r, b), all_screens=True).convert("RGB")
        small.save(os.path.join(tempfile.gettempdir(), "saydo_vis_exp_small.png"))
        print("EXP 小区域后画面与原来的差异范围:", ImageChops.difference(base, small).getbbox(), "窗口", (l, t, r, b), flush=True)
    # 诊断：哪些顶层窗口和弹窗矩形重叠（用来确认背后那块灰色矩形到底是谁）
    def _cb(x, _):
        if win32gui.IsWindowVisible(x):
            a = win32gui.GetWindowRect(x)
            if a[0] < r and a[2] > l and a[1] < b and a[3] > t and (a[2] - a[0]) > 5:
                print("  OVERLAP", hex(x), win32gui.GetClassName(x), repr(win32gui.GetWindowText(x))[:30], a, flush=True)
    win32gui.EnumWindows(_cb, None)

    if root is not None:
        root.after(0, root.destroy)


if os.environ.get("VIS_TK") == "1":
    # 复现 e2e 的条件：前台是我自己的测试窗口（Tk），弹窗在它存在时显示
    import threading
    import tkinter as tk
    root = tk.Tk()
    root.title("SayDo visual target")
    root.geometry(os.environ.get("VIS_TK_SIZE", "460x140") + "+" + os.environ.get("VIS_TK_POS", "400+300"))
    if os.environ.get("VIS_MAG") == "1":
        root.configure(bg="#ff00ff")   # 洋红底：弹窗矩形里凡是没被裁掉的非弹窗内容一眼可见
        tk.Frame(root, bg="#ff00ff").pack(fill="both", expand=True)
        root.attributes("-topmost", True)   # 背后的 Claude 界面一直在变，用置顶的纯色窗口垫底才稳定
    else:
        tk.Text(root).pack(fill="both", expand=True)
    def start():
        time.sleep(0.5)
        hwnd = w.user32.GetParent(int(root.winfo_id())) or int(root.winfo_id())
        w.force_foreground(hwnd)
        print("前台:", w.foreground()["title"], flush=True)
        drive(root)
    threading.Thread(target=start, daemon=True).start()
    root.mainloop()
else:
    drive()

api("panel_hide")
ui.WIN.close_all()
app.terminate()
try:
    app.wait(5)
except subprocess.TimeoutExpired:
    app.kill()
print("done")
