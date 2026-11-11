"""抓取用户当前面对的东西：选中文字 / 选中文件 / 截屏 / 粘回。

选区抓取 = 模拟 Ctrl+C → 读剪贴板（文件=CF_HDROP，文字=CF_UNICODETEXT）→ 还原剪贴板。要点：
1. 用户此刻正按着热键修饰键。先临时"松开"Alt/Shift/Win（注入 keyup），发完再按回去，否则 Ctrl+C 会变成 Ctrl+Alt+C。
2. 终端里 Ctrl+C 是 SIGINT → 终端类程序改用 Ctrl+Insert（同样是复制，不发信号）。
3. 用剪贴板序列号判断"复制成没成功"，不用哨兵值，所以不会破坏用户原剪贴板。
"""
import ctypes
import ctypes.wintypes as wt
import os
import time

from . import winapi as w

TERMINALS = {"windowsterminal.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "conhost.exe", "openconsole.exe", "wezterm-gui.exe",
             "alacritty.exe", "mintty.exe", "putty.exe", "kitty.exe", "tabby.exe", "hyper.exe"}
VK_CONTROL, VK_C, VK_V, VK_INSERT = 0x11, 0x43, 0x56, 0x2D
GAP = 0.02  # 按键事件间隔（秒）


def send_copy(terminal: bool):
    ctrl_down = w.key_down(VK_CONTROL)
    released = [vk for vk in (0xA4, 0xA5, 0xA0, 0xA1, 0x5B, 0x5C) if w.key_down(vk)]
    # 个别键盘/远程桌面只报泛型 Alt/Shift 状态
    if not any(v in released for v in (0xA4, 0xA5)) and w.key_down(0x12):
        released.append(0x12)
    if not any(v in released for v in (0xA0, 0xA1)) and w.key_down(0x10):
        released.append(0x10)
    # 0xE8 是"未分配"虚拟键：Alt/Win 被按住期间先敲一下它，系统就不会把随后的 Alt/Win 松开当成
    # "单独点了一下 Alt/Win"（否则会激活窗口菜单 / 弹出开始菜单，吞掉后面的 Ctrl+C）。AutoHotkey 同款做法。
    def defuse():
        w.send_keys([(0xE8, False), (0xE8, True)], GAP)

    if released:
        defuse()
        w.send_keys([(vk, True) for vk in released], GAP)
    if not ctrl_down:
        w.send_keys([(VK_CONTROL, False)])
        time.sleep(GAP)
    k = VK_INSERT if terminal else VK_C
    w.send_keys([(k, False), (k, True)], GAP)
    if not ctrl_down:
        w.send_keys([(VK_CONTROL, True)])
    if released:
        w.send_keys([(vk, False) for vk in released], GAP)
        defuse()  # 用户之后真正松开 Alt/Win 时同样不触发菜单


def grab_selection(fg: dict):
    """返回 ("none", None) | ("text", str) | ("files", [path...])"""
    before = w.clip_seq()
    backup = w.clip_backup()
    send_copy(fg.get("exe", "").lower() in TERMINALS)
    t0 = time.time()
    changed = False
    while time.time() - t0 < 0.7:
        time.sleep(0.015)
        if w.clip_seq() != before:
            changed = True
            time.sleep(0.04)  # 让应用把多种格式写完
            break
    if not changed:
        return "none", None
    sel = ("none", None)
    try:
        with w.Clipboard() as c:
            files = w.parse_hdrop(c.get_raw(w.CF_HDROP) or b"")
            if files:
                sel = ("files", files)
            else:
                raw = c.get_raw(w.CF_UNICODETEXT)
                t = w.parse_text(raw) if raw else ""
                if t.strip():
                    sel = ("text", t)
    except OSError:
        pass
    if backup:
        w.clip_restore(backup)
    else:
        try:
            with w.Clipboard() as c:
                c.empty()
        except OSError:
            pass
    return sel


def paste_back(hwnd: int, text: str):
    w.set_clipboard_text(text)
    if hwnd:
        w.force_foreground(hwnd)
    time.sleep(0.18)
    w.send_keys([(VK_CONTROL, False), (VK_V, False), (VK_V, True), (VK_CONTROL, True)], GAP)


def _window_rect(hwnd: int):
    r = wt.RECT()
    try:
        # 扩展边框（不含 Win11 的隐形阴影边），比 GetWindowRect 准
        if ctypes.windll.dwmapi.DwmGetWindowAttribute(wt.HWND(hwnd), 9, ctypes.byref(r), ctypes.sizeof(r)) == 0:
            return r.left, r.top, r.right, r.bottom
    except Exception:
        pass
    if w.user32.GetWindowRect(wt.HWND(hwnd), ctypes.byref(r)):
        return r.left, r.top, r.right, r.bottom
    return None


def screenshot(hwnd: int, cursor, out_dir: str) -> str:
    """截源程序窗口（取不到/太小就截整个虚拟屏幕）→ PNG（长边 ≤ 1920），返回路径"""
    return save_image(grab_image(hwnd), out_dir)


def grab_image(hwnd: int):
    """只抓屏（很快，要在弹窗出现之前调用，免得把弹窗截进去）；存盘另做，见 save_image"""
    from PIL import ImageGrab
    img = None
    if hwnd:
        rc = _window_rect(hwnd)
        if rc and rc[2] - rc[0] > 200 and rc[3] - rc[1] > 100 and not w.user32.IsIconic(wt.HWND(hwnd)):
            try:
                img = ImageGrab.grab(bbox=rc, all_screens=True)
            except Exception:
                img = None
    if img is None:
        img = ImageGrab.grab(all_screens=True)
    mx = 1920
    if max(img.size) > mx:
        s = mx / max(img.size)
        img = img.resize((int(img.width * s), int(img.height * s)))
    return img


def save_image(img, out_dir: str) -> str:
    os.makedirs(out_dir, exist_ok=True)
    p = os.path.join(out_dir, f"shot-{int(time.time() * 1000)}.png")
    img.convert("RGB").save(p)
    shots = sorted(f for f in os.listdir(out_dir) if f.startswith("shot-"))
    for f in shots[:-12]:
        try:
            os.remove(os.path.join(out_dir, f))
        except OSError:
            pass
    return p
