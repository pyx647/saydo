"""探针：Edge/Chrome --app 窗口能否做成"无标题栏 + 圆角/胶囊 + 置顶 + 不抢焦点"的弹窗。
只启动并操作我自己的窗口，用 PrintWindow 抓取该窗口自身内容（不截屏幕，不会拍到别的窗口）。"""
import ctypes
import os
import shutil
import subprocess
import sys
import tempfile
import time

import win32con
import win32gui
import win32ui
from PIL import Image

from app import winapi as w

w.set_dpi_aware()
BROWSERS = {
    "edge": r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    "chrome": r"C:\Program Files\Google\Chrome\Application\chrome.exe",
}
which = sys.argv[1] if len(sys.argv) > 1 else "edge"
exe = BROWSERS[which]
prof = os.path.join(tempfile.gettempdir(), f"saydo_probe_{which}")
shutil.rmtree(prof, ignore_errors=True)
html = ("data:text/html;charset=utf-8,<title>SayDoProbe</title><body style='margin:0;background:%2314131f;color:white;font:18px Segoe UI;"
        "display:flex;align-items:center;justify-content:center;height:100vh'>正在聆听… SayDo probe</body>")
flags = ["--no-first-run", "--no-default-browser-check"]
proc = subprocess.Popen([exe, f"--app={html}", f"--user-data-dir={prof}", *flags, "--window-size=440,120", "--window-position=300,300"])
hwnd = 0
for _ in range(80):
    time.sleep(0.25)
    found = []
    win32gui.EnumWindows(lambda h, _: found.append(h) if win32gui.IsWindowVisible(h) and "SayDoProbe" in win32gui.GetWindowText(h) else None, None)
    if found:
        hwnd = found[0]
        break
print(which, "hwnd:", hwnd, win32gui.GetClassName(hwnd) if hwnd else None)


def grab(hwnd, path):
    l, t, r, b = win32gui.GetWindowRect(hwnd)
    wd, ht = r - l, b - t
    hdc = win32gui.GetWindowDC(hwnd)
    mfc = win32ui.CreateDCFromHandle(hdc)
    mem = mfc.CreateCompatibleDC()
    bmp = win32ui.CreateBitmap()
    bmp.CreateCompatibleBitmap(mfc, wd, ht)
    mem.SelectObject(bmp)
    ctypes.windll.user32.PrintWindow(hwnd, mem.GetSafeHdc(), 2)  # PW_RENDERFULLCONTENT
    info = bmp.GetInfo()
    bits = bmp.GetBitmapBits(True)
    Image.frombuffer("RGB", (info["bmWidth"], info["bmHeight"]), bits, "raw", "BGRX", 0, 1).save(path)
    win32gui.DeleteObject(bmp.GetHandle())
    mem.DeleteDC()
    mfc.DeleteDC()
    win32gui.ReleaseDC(hwnd, hdc)
    return wd, ht


if hwnd:
    time.sleep(1.2)
    print("before rect:", win32gui.GetWindowRect(hwnd), "size", grab(hwnd, os.path.join(tempfile.gettempdir(), f"probe_{which}_1.png")))
    st = win32gui.GetWindowLong(hwnd, win32con.GWL_STYLE)
    st &= ~(win32con.WS_CAPTION | win32con.WS_THICKFRAME | win32con.WS_SYSMENU | win32con.WS_MINIMIZEBOX | win32con.WS_MAXIMIZEBOX)
    win32gui.SetWindowLong(hwnd, win32con.GWL_STYLE, st)
    ex = win32gui.GetWindowLong(hwnd, win32con.GWL_EXSTYLE) | win32con.WS_EX_TOOLWINDOW | 0x08000000
    win32gui.SetWindowLong(hwnd, win32con.GWL_EXSTYLE, ex)
    win32gui.SetWindowPos(hwnd, win32con.HWND_TOPMOST, 300, 300, 440, 72, win32con.SWP_FRAMECHANGED | win32con.SWP_NOACTIVATE | win32con.SWP_SHOWWINDOW)
    # 胶囊：SetWindowRgn
    rgn = ctypes.windll.gdi32.CreateRoundRectRgn(0, 0, 441, 73, 72, 72)
    ctypes.windll.user32.SetWindowRgn(hwnd, rgn, True)
    time.sleep(1.2)
    print("after rect:", win32gui.GetWindowRect(hwnd), "size", grab(hwnd, os.path.join(tempfile.gettempdir(), f"probe_{which}_2.png")))
    win32gui.PostMessage(hwnd, win32con.WM_CLOSE, 0, 0)
proc.terminate()
