"""Windows 原生接口封装（纯 ctypes，不编译任何东西）：前台窗口、SendInput、剪贴板、全局热键钩子、麦克风(waveIn)。"""
import ctypes
import ctypes.wintypes as wt
import os
import threading
import time

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
winmm = ctypes.WinDLL("winmm")

LRESULT = ctypes.c_ssize_t
CREATE_NO_WINDOW = 0x08000000

# ───────────────────────── 签名声明（64 位下句柄必须声明，否则会被截断成 32 位 int） ─────────────────────────
user32.GetForegroundWindow.restype = wt.HWND
user32.GetWindowTextW.argtypes = [wt.HWND, wt.LPWSTR, ctypes.c_int]
user32.GetWindowThreadProcessId.argtypes = [wt.HWND, ctypes.POINTER(wt.DWORD)]
user32.GetAsyncKeyState.argtypes = [ctypes.c_int]
user32.OpenClipboard.argtypes = [wt.HWND]
user32.GetClipboardData.argtypes = [wt.UINT]
user32.GetClipboardData.restype = wt.HANDLE
user32.SetClipboardData.argtypes = [wt.UINT, wt.HANDLE]
user32.SetClipboardData.restype = wt.HANDLE
user32.IsClipboardFormatAvailable.argtypes = [wt.UINT]
user32.RegisterClipboardFormatW.argtypes = [wt.LPCWSTR]
user32.GetClipboardSequenceNumber.restype = wt.DWORD
kernel32.GlobalAlloc.argtypes = [wt.UINT, ctypes.c_size_t]
kernel32.GlobalAlloc.restype = wt.HANDLE
kernel32.GlobalLock.argtypes = [wt.HANDLE]
kernel32.GlobalLock.restype = ctypes.c_void_p
kernel32.GlobalUnlock.argtypes = [wt.HANDLE]
kernel32.GlobalSize.argtypes = [wt.HANDLE]
kernel32.GlobalSize.restype = ctypes.c_size_t
kernel32.GetModuleHandleW.restype = wt.HMODULE
user32.SetForegroundWindow.argtypes = [wt.HWND]


def set_dpi_aware():
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
    except Exception:
        try:
            user32.SetProcessDPIAware()
        except Exception:
            pass


# ───────────────────────── 前台窗口 / 光标 ─────────────────────────

def cursor_pos():
    p = wt.POINT()
    user32.GetCursorPos(ctypes.byref(p))
    return p.x, p.y


def exe_of_pid(pid: int) -> str:
    try:
        import psutil
        return psutil.Process(pid).name()
    except Exception:
        return ""


def foreground() -> dict:
    h = user32.GetForegroundWindow()
    pid = wt.DWORD(0)
    user32.GetWindowThreadProcessId(h, ctypes.byref(pid))
    buf = ctypes.create_unicode_buffer(512)
    user32.GetWindowTextW(h, buf, 512)
    return {"hwnd": int(h or 0), "pid": int(pid.value), "title": buf.value, "exe": exe_of_pid(pid.value)}


def force_foreground(hwnd: int) -> bool:
    """把窗口提到前台。后台进程直接 SetForegroundWindow 会被前台锁拒绝；
    标准做法：把本线程输入附着到当前前台窗口的线程上再设置。"""
    if not hwnd:
        return False
    cur = user32.GetForegroundWindow()
    if int(cur or 0) == hwnd:
        return True
    pid = wt.DWORD(0)
    fg_tid = user32.GetWindowThreadProcessId(cur, ctypes.byref(pid)) if cur else 0
    my_tid = kernel32.GetCurrentThreadId()
    attached = bool(fg_tid and fg_tid != my_tid and user32.AttachThreadInput(my_tid, fg_tid, True))
    try:
        user32.ShowWindow(wt.HWND(hwnd), 9) if user32.IsIconic(wt.HWND(hwnd)) else None
        user32.BringWindowToTop(wt.HWND(hwnd))
        user32.SetForegroundWindow(wt.HWND(hwnd))
    finally:
        if attached:
            user32.AttachThreadInput(my_tid, fg_tid, False)
    time.sleep(0.05)
    return int(user32.GetForegroundWindow() or 0) == hwnd


# ───────────────────────── SendInput ─────────────────────────

class MOUSEINPUT(ctypes.Structure):
    _fields_ = [("dx", wt.LONG), ("dy", wt.LONG), ("mouseData", wt.DWORD), ("dwFlags", wt.DWORD), ("time", wt.DWORD), ("dwExtraInfo", ctypes.c_size_t)]


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [("wVk", wt.WORD), ("wScan", wt.WORD), ("dwFlags", wt.DWORD), ("time", wt.DWORD), ("dwExtraInfo", ctypes.c_size_t)]


class HARDWAREINPUT(ctypes.Structure):
    _fields_ = [("uMsg", wt.DWORD), ("wParamL", wt.WORD), ("wParamH", wt.WORD)]


class _IU(ctypes.Union):
    _fields_ = [("mi", MOUSEINPUT), ("ki", KEYBDINPUT), ("hi", HARDWAREINPUT)]


class INPUT(ctypes.Structure):
    _anonymous_ = ("u",)
    _fields_ = [("type", wt.DWORD), ("u", _IU)]


KEYEVENTF_KEYUP = 0x0002
KEYEVENTF_EXTENDEDKEY = 0x0001
EXTENDED_VKS = {0x2D, 0x2E, 0x24, 0x23, 0x21, 0x22, 0x25, 0x26, 0x27, 0x28, 0x5B, 0x5C, 0xA3, 0xA5}  # Ins/Del/Home/End/PgUp/PgDn/方向键/Win/右Ctrl/右Alt
user32.MapVirtualKeyW.argtypes = [wt.UINT, wt.UINT]
user32.MapVirtualKeyW.restype = wt.UINT
user32.SendInput.argtypes = [wt.UINT, ctypes.POINTER(INPUT), ctypes.c_int]


SAYDO_EXTRA = 0x5A7D0  # 我们自己注入的按键都带这个标记，热键钩子据此忽略（测试时外部注入的按键不带，仍会被接受）


def send_keys(seq, gap: float = 0.0, tag: bool = True):
    """seq: [(vk, is_up), ...]。gap>0 时逐个事件发送并间隔 gap 秒：
    Explorer 等应用一次性收到整批按键会丢事件（实测 Ctrl+C 不生效），必须带间隔。"""
    if gap > 0 and len(seq) > 1:
        for item in seq:
            send_keys([item], 0.0, tag)
            time.sleep(gap)
        return
    arr = (INPUT * len(seq))()
    for i, (vk, up) in enumerate(seq):
        arr[i].type = 1
        # 同时带扫描码：Explorer(DirectUI) 等应用只认带扫描码的按键，光有虚拟键码会被忽略
        scan = user32.MapVirtualKeyW(vk, 0)
        ext = KEYEVENTF_EXTENDEDKEY if vk in EXTENDED_VKS else 0
        arr[i].ki = KEYBDINPUT(vk, scan, (KEYEVENTF_KEYUP if up else 0) | ext, 0, SAYDO_EXTRA if tag else 0)
    user32.SendInput(len(seq), arr, ctypes.sizeof(INPUT))


def key_down(vk: int) -> bool:
    return bool(user32.GetAsyncKeyState(vk) & 0x8000)


# ───────────────────────── 剪贴板（原始格式，用于备份/还原） ─────────────────────────

CF_UNICODETEXT, CF_HDROP, CF_DIB = 13, 15, 8


def clip_seq() -> int:
    return int(user32.GetClipboardSequenceNumber())


class Clipboard:
    def __enter__(self):
        for _ in range(30):
            if user32.OpenClipboard(None):
                return self
            time.sleep(0.01)
        raise OSError("剪贴板被占用")

    def __exit__(self, *a):
        user32.CloseClipboard()

    @staticmethod
    def get_raw(fmt: int):
        if not user32.IsClipboardFormatAvailable(fmt):
            return None
        h = user32.GetClipboardData(fmt)
        if not h:
            return None
        p = kernel32.GlobalLock(h)
        if not p:
            return None
        try:
            return ctypes.string_at(p, kernel32.GlobalSize(h))
        finally:
            kernel32.GlobalUnlock(h)

    @staticmethod
    def set_raw(fmt: int, data: bytes):
        h = kernel32.GlobalAlloc(0x0002, len(data))  # GMEM_MOVEABLE
        p = kernel32.GlobalLock(h)
        ctypes.memmove(p, data, len(data))
        kernel32.GlobalUnlock(h)
        user32.SetClipboardData(fmt, h)

    @staticmethod
    def empty():
        user32.EmptyClipboard()


def backup_formats():
    fmts = [CF_UNICODETEXT, CF_HDROP, CF_DIB]
    for n in ("Preferred DropEffect", "HTML Format", "Rich Text Format"):
        fmts.append(int(user32.RegisterClipboardFormatW(n)))
    return fmts


def clip_backup():
    out = []
    try:
        with Clipboard() as c:
            for f in backup_formats():
                d = c.get_raw(f)
                if d:
                    out.append((f, d))
    except OSError:
        pass
    return out


def clip_restore(backup):
    try:
        with Clipboard() as c:
            c.empty()
            for f, d in backup:
                c.set_raw(f, d)
    except OSError:
        pass


def parse_text(raw: bytes) -> str:
    return raw.decode("utf-16-le", errors="ignore").split("\x00")[0]


def parse_hdrop(raw: bytes):
    if not raw or len(raw) < 20:
        return []
    off = int.from_bytes(raw[0:4], "little")
    wide = int.from_bytes(raw[16:20], "little")
    body = raw[off:]
    if wide:
        s = body.decode("utf-16-le", errors="ignore")
    else:
        s = body.decode("mbcs", errors="ignore")
    return [p for p in s.split("\x00") if p]


def set_clipboard_text(text: str):
    with Clipboard() as c:
        c.empty()
        c.set_raw(CF_UNICODETEXT, (text + "\x00").encode("utf-16-le"))


# ───────────────────────── 全局热键（WH_KEYBOARD_LL / WH_MOUSE_LL） ─────────────────────────

HOOKPROC = ctypes.WINFUNCTYPE(LRESULT, ctypes.c_int, wt.WPARAM, wt.LPARAM)


class KBDLLHOOKSTRUCT(ctypes.Structure):
    _fields_ = [("vkCode", wt.DWORD), ("scanCode", wt.DWORD), ("flags", wt.DWORD), ("time", wt.DWORD), ("dwExtraInfo", ctypes.c_size_t)]


class MSLLHOOKSTRUCT(ctypes.Structure):
    _fields_ = [("pt", wt.POINT), ("mouseData", wt.DWORD), ("flags", wt.DWORD), ("time", wt.DWORD), ("dwExtraInfo", ctypes.c_size_t)]


user32.SetWindowsHookExW.argtypes = [ctypes.c_int, HOOKPROC, wt.HINSTANCE, wt.DWORD]
user32.SetWindowsHookExW.restype = wt.HHOOK
user32.CallNextHookEx.argtypes = [wt.HHOOK, ctypes.c_int, wt.WPARAM, wt.LPARAM]
user32.CallNextHookEx.restype = LRESULT

WH_KEYBOARD_LL, WH_MOUSE_LL = 13, 14
WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP = 0x100, 0x101, 0x104, 0x105
WM_XBUTTONDOWN, WM_XBUTTONUP = 0x20B, 0x20C
LLKHF_INJECTED = 0x10

VK_NAMES = {0x20: "Space", 0x0D: "Enter", 0x09: "Tab", 0xC0: "`"}


def vk_to_name(vk: int):
    if 0x30 <= vk <= 0x39 or 0x41 <= vk <= 0x5A:
        return chr(vk)
    if 0x70 <= vk <= 0x87:
        return f"F{vk - 0x70 + 1}"
    return VK_NAMES.get(vk)


def name_to_vk(name: str):
    for k, v in VK_NAMES.items():
        if v == name:
            return k
    if len(name) == 1 and name.isalnum():
        return ord(name.upper())
    if name.startswith("F") and name[1:].isdigit() and 1 <= int(name[1:]) <= 24:
        return 0x70 + int(name[1:]) - 1
    return None


def is_modifier(vk: int) -> bool:
    return vk in (0x10, 0xA0, 0xA1, 0x11, 0xA2, 0xA3, 0x12, 0xA4, 0xA5, 0x5B, 0x5C)


class HotkeyHook:
    """按住说话热键。on_down / on_up 在新线程里被调用（钩子回调必须极快返回，否则系统会把钩子摘掉）。

    加固点：忽略注入事件（我们自己模拟的 Ctrl+C 不能被当成用户松开了热键；测试时 allow_injected=True）；
    热键主键被吞掉，避免按住时 Space 自动连发漏进前台程序。"""

    def __init__(self, on_down, on_up, allow_injected=False):
        self.on_down, self.on_up = on_down, on_up
        self.allow_injected = allow_injected
        self.spec = None  # ("kb", ctrl, alt, shift, win, vk) | ("mouse", button)
        self.paused = False
        self.held = False
        self.main_down = False
        self.mod = {"ctrl": False, "alt": False, "shift": False, "win": False}
        self.record_cb = None
        self._kb_proc = HOOKPROC(self._kb)
        self._ms_proc = HOOKPROC(self._ms)
        self._thread = None
        self._ready = threading.Event()

    # —— 规格 ——
    def set_spec(self, s: str):
        self.held = False
        self.spec = self.parse(s)

    @staticmethod
    def parse(s: str):
        s = (s or "").strip()
        if s.startswith("Mouse:"):
            return ("mouse", 1 if s.endswith("XButton1") else 2) if s[6:] in ("XButton1", "XButton2") else None
        parts = s.split("+")
        vk = name_to_vk(parts[-1])
        if vk is None:
            return None
        m = set(parts[:-1])
        return ("kb", "Ctrl" in m, "Alt" in m, "Shift" in m, "Win" in m, vk)

    def record(self, timeout=8.0):
        """等待用户按下一个新热键，返回规格字符串"""
        ev = threading.Event()
        res = []

        def cb(spec):
            res.append(spec)
            ev.set()

        self.record_cb = cb
        ev.wait(timeout)
        self.record_cb = None
        if not res:
            raise TimeoutError("超时：8 秒内没检测到按键或鼠标侧键")
        return res[0]

    def _spec_str(self, vk):
        p = [n for n, k in (("Ctrl", "ctrl"), ("Alt", "alt"), ("Shift", "shift"), ("Win", "win")) if self.mod[k]]
        p.append(vk_to_name(vk) or f"VK{vk}")
        return "+".join(p)

    def _fire(self, down: bool):
        threading.Thread(target=(self.on_down if down else self.on_up), daemon=True).start()

    # —— 钩子回调 ——
    def _kb(self, code, wparam, lparam):
        try:
            if code >= 0:
                kb = KBDLLHOOKSTRUCT.from_address(lparam)
                if kb.dwExtraInfo == SAYDO_EXTRA or ((kb.flags & LLKHF_INJECTED) and not self.allow_injected):
                    return user32.CallNextHookEx(None, code, wparam, lparam)
                vk = kb.vkCode
                down = wparam in (WM_KEYDOWN, WM_SYSKEYDOWN)
                if vk in (0x11, 0xA2, 0xA3):
                    self.mod["ctrl"] = down
                elif vk in (0x12, 0xA4, 0xA5):
                    self.mod["alt"] = down
                elif vk in (0x10, 0xA0, 0xA1):
                    self.mod["shift"] = down
                elif vk in (0x5B, 0x5C):
                    self.mod["win"] = down
                if self.record_cb:
                    if down and not is_modifier(vk):
                        cb, self.record_cb = self.record_cb, None
                        cb(self._spec_str(vk))
                        return 1
                elif self.spec and self.spec[0] == "kb":
                    _, c, a, s, w, target = self.spec
                    mods_ok = (self.mod["ctrl"], self.mod["alt"], self.mod["shift"], self.mod["win"]) == (c, a, s, w)
                    was = self.held
                    swallow = False
                    if vk == target:
                        swallow = (down and mods_ok) or (not down and was)
                        self.main_down = down
                    if vk == target or is_modifier(vk):
                        sat = self.main_down and mods_ok
                        if sat and not was and not self.paused:
                            self.held = True
                            self._fire(True)
                        elif not sat and was:
                            self.held = False
                            self._fire(False)
                    if swallow:
                        return 1
        except Exception:
            pass
        return user32.CallNextHookEx(None, code, wparam, lparam)

    def _ms(self, code, wparam, lparam):
        try:
            if code >= 0 and wparam in (WM_XBUTTONDOWN, WM_XBUTTONUP):
                ms = MSLLHOOKSTRUCT.from_address(lparam)
                button = (ms.mouseData >> 16) & 0xFFFF
                down = wparam == WM_XBUTTONDOWN
                if self.record_cb:
                    if down:
                        cb, self.record_cb = self.record_cb, None
                        cb(f"Mouse:XButton{1 if button == 1 else 2}")
                        return 1
                elif self.spec and self.spec[0] == "mouse" and self.spec[1] == button:
                    if down:
                        if not self.held and not self.paused:
                            self.held = True
                            self._fire(True)
                    elif self.held:
                        self.held = False
                        self._fire(False)
                    return 1
        except Exception:
            pass
        return user32.CallNextHookEx(None, code, wparam, lparam)

    # —— 线程 ——
    def start(self):
        def run():
            hmod = kernel32.GetModuleHandleW(None)
            self._kh = user32.SetWindowsHookExW(WH_KEYBOARD_LL, self._kb_proc, hmod, 0)
            self._mh = user32.SetWindowsHookExW(WH_MOUSE_LL, self._ms_proc, hmod, 0)
            self._tid = kernel32.GetCurrentThreadId()
            self._ready.set()
            msg = wt.MSG()
            while user32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
                user32.TranslateMessage(ctypes.byref(msg))
                user32.DispatchMessageW(ctypes.byref(msg))

        self._thread = threading.Thread(target=run, daemon=True, name="hotkey-hook")
        self._thread.start()
        self._ready.wait(3)
        return bool(getattr(self, "_kh", None))

    def stop(self):
        user32.PostThreadMessageW(getattr(self, "_tid", 0), 0x0012, 0, 0)  # WM_QUIT


# ───────────────────────── 麦克风（waveIn，直接要 16kHz/16bit/单声道，系统负责转换） ─────────────────────────

class WAVEFORMATEX(ctypes.Structure):
    _fields_ = [("wFormatTag", wt.WORD), ("nChannels", wt.WORD), ("nSamplesPerSec", wt.DWORD), ("nAvgBytesPerSec", wt.DWORD),
                ("nBlockAlign", wt.WORD), ("wBitsPerSample", wt.WORD), ("cbSize", wt.WORD)]


class WAVEHDR(ctypes.Structure):
    _fields_ = [("lpData", ctypes.c_void_p), ("dwBufferLength", wt.DWORD), ("dwBytesRecorded", wt.DWORD), ("dwUser", ctypes.c_size_t),
                ("dwFlags", wt.DWORD), ("dwLoops", wt.DWORD), ("lpNext", ctypes.c_void_p), ("reserved", ctypes.c_size_t)]


for _fn in ("waveInPrepareHeader", "waveInUnprepareHeader", "waveInAddBuffer"):
    getattr(winmm, _fn).argtypes = [ctypes.c_void_p, ctypes.POINTER(WAVEHDR), wt.UINT]
for _fn in ("waveInStart", "waveInStop", "waveInReset", "waveInClose"):
    getattr(winmm, _fn).argtypes = [ctypes.c_void_p]
winmm.waveInOpen.argtypes = [ctypes.POINTER(ctypes.c_void_p), wt.UINT, ctypes.POINTER(WAVEFORMATEX), ctypes.c_size_t, ctypes.c_size_t, wt.DWORD]

WHDR_DONE = 1


class Recorder:
    """16kHz 单声道 int16 录音。stop() 返回 bytes（int16 little-endian）。level_cb(0..1) 约每 50ms 回调一次。"""

    def __init__(self, level_cb=None, max_seconds=120):
        self.level_cb = level_cb
        self.max_bytes = 32000 * max_seconds
        self.chunks = []
        self._stop = threading.Event()
        self._thread = None
        self._err = None
        self._ready = threading.Event()

    def start(self):
        self._thread = threading.Thread(target=self._run, daemon=True, name="mic")
        self._thread.start()
        self._ready.wait(4)
        if self._err:
            raise OSError(self._err)

    def _run(self):
        fmt = WAVEFORMATEX(1, 1, 16000, 32000, 2, 16, 0)
        h = ctypes.c_void_p()
        r = winmm.waveInOpen(ctypes.byref(h), 0xFFFFFFFF, ctypes.byref(fmt), 0, 0, 0)
        if r != 0:
            self._err = f"打开麦克风失败（错误码 {r}）。请检查 Windows 设置 → 隐私 → 麦克风，以及默认录音设备。"
            self._ready.set()
            return
        BUF = 3200  # 100ms
        n = 4
        bufs = [ctypes.create_string_buffer(BUF) for _ in range(n)]
        hdrs = []
        for b in bufs:
            hd = WAVEHDR(ctypes.cast(b, ctypes.c_void_p), BUF, 0, 0, 0, 0, None, 0)
            winmm.waveInPrepareHeader(h, ctypes.byref(hd), ctypes.sizeof(hd))
            winmm.waveInAddBuffer(h, ctypes.byref(hd), ctypes.sizeof(hd))
            hdrs.append(hd)
        winmm.waveInStart(h)
        self._ready.set()
        total = 0

        def drain(requeue):
            nonlocal total
            peak = 0
            for hd, b in zip(hdrs, bufs):
                if hd.dwFlags & WHDR_DONE:
                    data = ctypes.string_at(b, hd.dwBytesRecorded)
                    if data and total < self.max_bytes:
                        self.chunks.append(data)
                        total += len(data)
                        import array
                        a = array.array("h", data[: len(data) // 2 * 2])
                        if a:
                            peak = max(peak, max(abs(x) for x in a[:: max(1, len(a) // 400)]))
                    hd.dwFlags &= ~WHDR_DONE
                    if requeue:
                        winmm.waveInAddBuffer(h, ctypes.byref(hd), ctypes.sizeof(hd))
            return peak

        last_lvl = 0
        while not self._stop.is_set():
            time.sleep(0.03)
            p = drain(True)
            if p:
                last_lvl = p
            if self.level_cb and time.time() % 0.05 < 0.03:
                try:
                    self.level_cb(min(1.0, (last_lvl / 32768.0 * 4) ** 0.6))
                except Exception:
                    pass
            last_lvl = int(last_lvl * 0.6)
        winmm.waveInStop(h)
        winmm.waveInReset(h)
        drain(False)
        for hd in hdrs:
            winmm.waveInUnprepareHeader(h, ctypes.byref(hd), ctypes.sizeof(hd))
        winmm.waveInClose(h)

    def snapshot(self) -> bytes:
        """录音进行中取到目前为止的音频（实时字幕用）"""
        return b"".join(self.chunks)

    def stop(self) -> bytes:
        self._stop.set()
        if self._thread:
            self._thread.join(3)
        return b"".join(self.chunks)
