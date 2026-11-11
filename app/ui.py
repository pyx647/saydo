"""界面层：事件广播(SSE)、弹窗/设置窗口（Edge --app）、向用户提问。

弹窗窗口的做法：Edge 的 --app 窗口去不掉 Chromium 自绘的标题栏，所以用 SetWindowRgn 把标题栏区域裁掉，
只露出网页内容，同时裁出圆角/胶囊形状。网页(ResizeObserver)把自己的尺寸、圆角、窗口边框厚度上报，由这里摆放。
焦点策略：听写/执行中带 WS_EX_NOACTIVATE，绝不抢焦点（用户的选区还在原程序里）；需要交互时才去掉。"""
import ctypes
import ctypes.wintypes
import itertools
import json
import os
import queue
import subprocess
import sys
import threading
import time

import win32api
import win32con
import win32gui

from . import config, winapi as w

EDGE_PATHS = [r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe", r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"]
PANEL_TITLE, SETTINGS_TITLE = "SayDoPanel", "SayDo · 设置"
WS_EX_NOACTIVATE = 0x08000000

# ───────────────────────── 事件广播 ─────────────────────────

_subs = set()
_subs_lock = threading.Lock()


def subscribe() -> queue.Queue:
    q = queue.Queue()
    with _subs_lock:
        _subs.add(q)
    return q


def unsubscribe(q):
    with _subs_lock:
        _subs.discard(q)


def subscriber_count() -> int:
    with _subs_lock:
        return len(_subs)


def emit(event: str, payload=None):
    data = json.dumps({"event": event, "payload": payload}, ensure_ascii=False)
    with _subs_lock:
        for q in list(_subs):
            q.put(data)


def state(d: dict):
    emit("panel-state", d)


def error(msg: str):
    interactive(False)
    state({"phase": "error", "message": msg})


# ───────────────────────── 视图模型 ─────────────────────────

def file_view(path: str) -> dict:
    try:
        st = os.stat(path)
        size, is_dir = st.st_size, os.path.isdir(path)
    except OSError:
        size, is_dir = 0, False
    return {"path": path, "name": os.path.basename(path.rstrip("\\/")) or path, "dir": os.path.dirname(path.rstrip("\\/")),
            "ext": os.path.splitext(path)[1].lstrip(".").lower(), "size": size, "isDir": is_dir}


def ctx_view(fg: dict, sel_kind: str, sel_val, screen: bool = False) -> dict:
    v = {"kind": "none", "label": "未选中内容", "preview": "", "files": [], "app": fg.get("exe", ""), "title": fg.get("title", "")}
    if screen:
        v.update(kind="screen", label="当前屏幕 / 页面")
    elif sel_kind == "text":
        v.update(kind="text", label=f"已选中 {len(sel_val)} 字", preview=sel_val[:120])
    elif sel_kind == "files":
        fv = [file_view(f) for f in sel_val]
        v.update(kind="files", files=fv, label=fv[0]["name"] if len(fv) == 1 else f"{len(fv)} 个文件")
    return v


# ───────────────────────── 窗口 ─────────────────────────

def _edge() -> str:
    for p in EDGE_PATHS:
        if os.path.exists(p):
            return p
    raise RuntimeError("找不到 Microsoft Edge（SayDo 的界面用 Edge 的应用窗口显示）")


def find_window(title: str) -> int:
    found = []

    def cb(h, _):
        try:
            if win32gui.GetClassName(h) == "Chrome_WidgetWin_1" and title in win32gui.GetWindowText(h):
                found.append(h)
        except Exception:
            pass

    win32gui.EnumWindows(cb, None)
    return found[0] if found else 0


class Windows:
    def __init__(self):
        self.base_url = ""
        self.token = ""
        self.profile = os.path.join(config.config_dir(), "edge-profile")
        self.anchor = None            # 弹窗可见区域左上角（物理像素）
        # 窗口无边框后，窗口矩形 == 网页区域，所以偏移恒为 0（不再信任网页上报的 outer-inner）
        self.layout = {"w": 380, "h": 70, "dpr": 1.0, "offx": 0, "offy": 0, "radius": 18, "parts": []}
        self.visible = False
        self.interactive = False
        self.lock = threading.RLock()
        self._expect_rgn = None
        self.docked = True
        self.mon_pt = (0, 0)
        threading.Thread(target=self._guard_loop, daemon=True).start()

    # —— 启动 ——
    def _launch(self, query: str, size, pos=None):
        args = [_edge(), f"--app={self.base_url}/index.html?{query}&t={self.token}", f"--user-data-dir={self.profile}",
                "--no-first-run", "--no-default-browser-check", f"--window-size={size[0]},{size[1]}",
                # 弹窗是"不抢焦点"的置顶窗口，Chromium 的原生遮挡检测/后台节流会把没被激活的窗口判成 hidden 而停止渲染（白屏），必须关掉
                "--disable-features=CalculateNativeWinOcclusion", "--disable-backgrounding-occluded-windows",
                "--disable-renderer-backgrounding", "--disable-background-timer-throttling"]
        if pos:
            args.append(f"--window-position={pos[0]},{pos[1]}")
        if os.environ.get("SAYDO_DEBUG_PORT"):  # 仅调试：打开 Edge 远程调试口，用来查看页面状态
            args.append(f"--remote-debugging-port={os.environ['SAYDO_DEBUG_PORT']}")
        subprocess.Popen(args, creationflags=w.CREATE_NO_WINDOW)

    @staticmethod
    def _frameless(h):
        """去掉弹窗窗口的标题栏/边框样式。Edge 的 --app 窗口用的是系统(DWM)画的标题栏和底色，
        SetWindowRgn 只能裁网页内容，裁不掉它们（实测：区域缩成小方块后，整块灰色标题栏仍在）。
        样式去掉后，窗口矩形就等于网页区域，不再有任何系统边框。"""
        # Win11 给 Edge 窗口铺的 Mica/亚克力底色和边框由 DWM 画，且不受窗口区域(SetWindowRgn)裁剪 → 关掉
        try:
            dwm = ctypes.windll.dwmapi

            def attr(a, v):
                val = ctypes.c_int(v)
                dwm.DwmSetWindowAttribute(ctypes.c_void_p(h), a, ctypes.byref(val), 4)
            attr(38, 1)            # DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_NONE
            attr(20, 1)            # 深色模式：就算有边框也是深色
            attr(2, 1)             # DWMWA_NCRENDERING_POLICY = DWMNCRP_DISABLED（不画非客户区）
            attr(33, 1)            # DWMWA_WINDOW_CORNER_PREFERENCE = DONOTROUND（圆角由我们的区域决定）
            attr(34, 0xFFFFFFFE)   # DWMWA_BORDER_COLOR = NONE
            attr(35, 0x00180E0F)   # DWMWA_CAPTION_COLOR = 和卡片同色的深色（BGR）
        except Exception as e:
            print("[ui] DWM 属性设置失败:", e, flush=True)
        st = win32gui.GetWindowLong(h, win32con.GWL_STYLE)
        bad = win32con.WS_CAPTION | win32con.WS_THICKFRAME | win32con.WS_SYSMENU | win32con.WS_MINIMIZEBOX | win32con.WS_MAXIMIZEBOX
        if st & bad:
            win32gui.SetWindowLong(h, win32con.GWL_STYLE, (st & ~bad) | win32con.WS_POPUP)
            win32gui.SetWindowPos(h, 0, 0, 0, 0, 0, win32con.SWP_FRAMECHANGED | win32con.SWP_NOMOVE | win32con.SWP_NOSIZE
                                  | win32con.SWP_NOZORDER | win32con.SWP_NOACTIVATE)

    def ensure_panel(self, timeout=25.0) -> int:
        with self.lock:
            h = find_window(PANEL_TITLE)
            if h:
                self._frameless(h)
                return h
            self._launch("w=panel", (440, 160), (-4000, -4000))
            t0 = time.time()
            while time.time() - t0 < timeout:
                time.sleep(0.2)
                h = find_window(PANEL_TITLE)
                if h:
                    ex = win32gui.GetWindowLong(h, win32con.GWL_EXSTYLE) | win32con.WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE
                    win32gui.SetWindowLong(h, win32con.GWL_EXSTYLE, ex)
                    self._frameless(h)
                    win32gui.ShowWindow(h, win32con.SW_HIDE)
                    self.visible = False
                    return h
            raise RuntimeError("弹窗窗口启动超时")

    def show_settings(self):
        h = find_window(SETTINGS_TITLE)
        if h:
            win32gui.ShowWindow(h, win32con.SW_RESTORE)
            w.force_foreground(h)
            return
        self._launch("w=settings", (940, 720))
        for _ in range(60):
            time.sleep(0.25)
            h = find_window(SETTINGS_TITLE)
            if h:
                w.force_foreground(h)
                return

    def close_all(self):
        for t in (PANEL_TITLE, SETTINGS_TITLE):
            h = find_window(t)
            if h:
                win32gui.PostMessage(h, win32con.WM_CLOSE, 0, 0)

    # —— 弹窗摆放 ——
    def _monitor_work(self, pt):
        try:
            mon = win32api.MonitorFromPoint(pt, win32con.MONITOR_DEFAULTTONEAREST)
            return win32api.GetMonitorInfo(mon)["Work"]
        except Exception:
            return (0, 0, 1920, 1080)

    def show_panel_at(self, cursor):
        """在光标附近显示面板（不抢焦点）"""
        h = self.ensure_panel()
        L = self.layout
        if L["h"] < 10:
            L["h"] = 70
        W, H = int(L["w"] * L["dpr"]), int(L["h"] * L["dpr"])
        # 固定停靠在光标所在屏幕的底部正中（任务栏上方）；用户拖动过就不再强制回位，直到下次重新弹出
        self.docked = True
        self.mon_pt = cursor
        self.anchor = cursor
        self.set_interactive(False)
        self.apply(h)
        win32gui.ShowWindow(h, win32con.SW_SHOWNOACTIVATE)
        self.apply(h)
        self.visible = True

    def apply(self, h=None):
        h = h or find_window(PANEL_TITLE)
        if not h or not self.anchor:
            return
        L = self.layout
        dpr, offx, offy = L["dpr"], L["offx"], L["offy"]
        W, H = int(round(L["w"] * dpr)), int(round(L["h"] * dpr))
        if self.docked:
            l, t, r, b = self._monitor_work(self.mon_pt)
            ax = l + (r - l - W) // 2
            ay = b - int(round(28 * dpr)) - H   # 底边固定，内容变高时向上长
        else:
            l, t, r, b = self._monitor_work(self.anchor)
            ax, ay = self.anchor
        if ay + H > b - 8:
            ay = max(t + 8, b - 8 - H)
        if ax + W > r - 8:
            ax = max(l + 8, r - 8 - W)
        self.anchor = (ax, ay)
        ow, oh = W + 2 * offx, H + offy + offx
        win32gui.SetWindowPos(h, win32con.HWND_TOPMOST, ax - offx, ay - offy, ow, oh, win32con.SWP_NOACTIVATE)
        gdi = ctypes.windll.gdi32
        parts = L.get("parts") or []
        if parts:
            # 气泡 + 胶囊：每块各自一个圆角矩形，并成一个窗口形状（两块之间的空隙是透明的）
            rgn = gdi.CreateRectRgn(0, 0, 0, 0)
            bb = [10 ** 6, 10 ** 6, -10 ** 6, -10 ** 6]
            for p in parts:
                px, py = offx + int(round(p["x"] * dpr)), offy + int(round(p["y"] * dpr))
                pw, ph = int(round(p["w"] * dpr)), int(round(p["h"] * dpr))
                pr = int(round(p["r"] * dpr))
                piece = gdi.CreateRoundRectRgn(px, py, px + pw + 1, py + ph + 1, pr * 2, pr * 2)
                gdi.CombineRgn(rgn, rgn, piece, 2)  # RGN_OR
                gdi.DeleteObject(piece)
                bb = [min(bb[0], px), min(bb[1], py), max(bb[2], px + pw + 1), max(bb[3], py + ph + 1)]
            expect = tuple(bb)
        else:
            rad = int(L["radius"] * dpr)
            rgn = gdi.CreateRoundRectRgn(offx, offy, offx + W + 1, offy + H + 1, rad * 2, rad * 2)
            expect = (offx, offy, offx + W + 1, offy + H + 1)
        ret = ctypes.windll.user32.SetWindowRgn(h, rgn, True)
        if not ret:
            print(f"[ui] SetWindowRgn 失败 err={ctypes.GetLastError()} rgn={rgn} parts={len(parts)} off=({offx},{offy}) WH=({W},{H})", flush=True)
            gdi.DeleteObject(rgn)
        self._expect_rgn = expect

    def _guard_loop(self):
        """Chromium 在窗口被激活（输入窗/要回答的卡片需要键盘焦点）时会自己重设窗口区域，把我们裁掉标题栏的区域冲掉，
        于是标题栏和浅色底漏出来。这里盯着：区域和预期不一致就重新套上。"""
        if os.environ.get("SAYDO_NOGUARD"):  # 仅调试
            return
        h = 0
        while True:
            time.sleep(0.05)
            try:
                if not self.visible or not self._expect_rgn:
                    continue
                if not h or not win32gui.IsWindow(h):
                    h = find_window(PANEL_TITLE)
                if not h or not win32gui.IsWindowVisible(h):
                    continue
                box = ctypes.wintypes.RECT()
                kind = ctypes.windll.user32.GetWindowRgnBox(h, ctypes.byref(box))
                cur = (box.left, box.top, box.right, box.bottom) if kind else None
                if cur is None or any(abs(a - b) > 2 for a, b in zip(cur, self._expect_rgn)):
                    self._guard_hits = getattr(self, "_guard_hits", 0) + 1
                    if self._guard_hits <= 5:  # 只记前几次，免得刷屏
                        print(f"[ui] 窗口区域被改掉了 cur={cur} expect={self._expect_rgn} rect={win32gui.GetWindowRect(h)} layout={self.layout}，重新套上", flush=True)
                    with self.lock:
                        self.apply(h)
            except Exception as e:
                print("[ui] guard 出错:", e, flush=True)
                time.sleep(1)

    def on_layout(self, args: dict):
        """网页上报：卡片 CSS 尺寸、圆角、DPR、窗口边框厚度。
        必须过滤无意义的值：隐藏态的卡片只有 1px 高，若记下来，下次显示窗口就只有几像素高，
        Chromium 会判定窗口不可见而停止渲染，ResizeObserver 也不再触发，永远没机会纠正。"""
        with self.lock:
            dpr = float(args.get("dpr") or self.layout["dpr"])
            if (args.get("h") or 0) < 10 or (args.get("w") or 0) < 100:
                return
            h = find_window(PANEL_TITLE)
            if h and self.visible and self.anchor and not self.docked:
                # 用户可能拖动过窗口：以当前可见区域左上角为准
                rc = win32gui.GetWindowRect(h)
                self.anchor = (rc[0] + self.layout["offx"], rc[1] + self.layout["offy"])
            self.layout["dpr"] = dpr
            for k in ("w", "h", "radius"):
                if args.get(k) is not None:
                    self.layout[k] = int(round(args[k]))
            if isinstance(args.get("parts"), list):
                self.layout["parts"] = [p for p in args["parts"] if all(isinstance(p.get(k), (int, float)) for k in ("x", "y", "w", "h", "r"))]
            ox, oy = args.get("offx"), args.get("offy")
            if ox is not None and oy is not None and 0 <= ox <= 24 * dpr and 20 * dpr <= oy <= 90 * dpr:
                self.layout["offx"], self.layout["offy"] = int(ox), int(oy)
            if h and self.visible:
                self.apply(h)

    def hide_panel(self):
        h = find_window(PANEL_TITLE)
        if h:
            win32gui.ShowWindow(h, win32con.SW_HIDE)
        self.visible = False

    def panel_visible(self) -> bool:
        h = find_window(PANEL_TITLE)
        return bool(h and win32gui.IsWindowVisible(h))

    def set_interactive(self, yes: bool):
        h = find_window(PANEL_TITLE)
        if not h:
            return
        ex = win32gui.GetWindowLong(h, win32con.GWL_EXSTYLE)
        new = (ex & ~WS_EX_NOACTIVATE) if yes else (ex | WS_EX_NOACTIVATE | win32con.WS_EX_TOOLWINDOW)
        if new != ex:
            win32gui.SetWindowLong(h, win32con.GWL_EXSTYLE, new)
        self.interactive = yes

    def focus_panel(self):
        h = find_window(PANEL_TITLE)
        if h:
            self.set_interactive(True)
            w.force_foreground(h)

    def drag(self):
        h = find_window(PANEL_TITLE)
        if not h:
            return
        self.docked = False   # 用户自己拖动了：这次显示期间不再强制回到底部正中
        x0, y0 = win32api.GetCursorPos()
        l0, t0 = win32gui.GetWindowRect(h)[:2]

        def loop():
            # 窗口没有标题栏了，系统的"拖标题栏"用不了：自己跟着鼠标移动，直到左键松开
            while ctypes.windll.user32.GetAsyncKeyState(0x01) & 0x8000:
                x, y = win32api.GetCursorPos()
                win32gui.SetWindowPos(h, 0, l0 + x - x0, t0 + y - y0, 0, 0,
                                      win32con.SWP_NOSIZE | win32con.SWP_NOZORDER | win32con.SWP_NOACTIVATE)
                time.sleep(0.008)
            rc = win32gui.GetWindowRect(h)
            self.anchor = (rc[0] + self.layout["offx"], rc[1] + self.layout["offy"])
        threading.Thread(target=loop, daemon=True).start()


WIN = Windows()


def interactive(yes: bool):
    WIN.set_interactive(yes)


# ───────────────────────── 向用户提问 ─────────────────────────

_pending = {}
_pending_lock = threading.Lock()
_ids = itertools.count(1)


def ask(spec: dict, timeout=600):
    """阻塞等待用户在弹窗里作答。返回前端回传的 value；取消/超时抛 RuntimeError。"""
    aid = f"ask{next(_ids)}"
    q = queue.Queue()
    with _pending_lock:
        _pending[aid] = q
    spec = dict(spec, phase="ask", id=aid)
    if spec.get("kind") == "folder" and "suggestions" not in spec:
        spec["suggestions"] = folder_suggestions()
    if not WIN.panel_visible():
        WIN.show_panel_at(w.cursor_pos())
    WIN.set_interactive(True)
    state(spec)
    try:
        v = q.get(timeout=timeout)
    except queue.Empty:
        v = None
    with _pending_lock:
        _pending.pop(aid, None)
    if v is _CANCEL or v is None:
        raise RuntimeError("等待用户选择超时，或已被取消")
    return v


_CANCEL = object()


def cancel_asks():
    with _pending_lock:
        for q in _pending.values():
            q.put(_CANCEL)
        _pending.clear()


def ask_reply(aid: str, value):
    with _pending_lock:
        q = _pending.pop(aid, None)
    if q:
        q.put(value)


# ───────────────────────── 文件夹 ─────────────────────────

def folder_suggestions() -> list:
    from . import session
    out, seen = [], set()

    def add(path, name, kind):
        k = path.lower()
        if os.path.isdir(path) and k not in seen:
            seen.add(k)
            out.append({"path": path, "name": name, "kind": kind})

    src = session.current_source_dir()
    if src:
        add(src, "源文件所在文件夹", "source")
    for f in config.get()["recentFolders"]:
        add(f, os.path.basename(f.rstrip("\\/")) or f, "recent")
    home = os.environ.get("USERPROFILE", "")
    for sub, name in (("Desktop", "桌面"), ("Documents", "文档"), ("Downloads", "下载"), ("Pictures", "图片"), ("Videos", "视频"), ("Music", "音乐")):
        add(os.path.join(home, sub), name, "known")
    return out


_BROWSE = """
import sys, tkinter as tk
from tkinter import filedialog
r = tk.Tk(); r.withdraw(); r.attributes('-topmost', True)
p = filedialog.askdirectory(title='选择文件夹', initialdir=sys.argv[1] or None, parent=r)
sys.stdout.buffer.write((p or '').encode('utf-8'))
"""


def browse_folder(start: str = "") -> str:
    """系统文件夹选择器（独立进程里跑 tkinter，置顶，避免被置顶弹窗盖住）"""
    r = subprocess.run([sys.executable.replace("pythonw.exe", "python.exe"), "-c", _BROWSE, start or ""],
                       capture_output=True, creationflags=w.CREATE_NO_WINDOW, timeout=600)
    return r.stdout.decode("utf-8", "ignore").strip().replace("/", "\\")


def open_path(path: str):
    os.startfile(path)


def reveal_path(path: str):
    p = path.replace("/", "\\")
    arg = f'"{p}"' if os.path.isdir(p) else f'/select,"{p}"'
    subprocess.Popen(f"explorer {arg}", creationflags=w.CREATE_NO_WINDOW)
