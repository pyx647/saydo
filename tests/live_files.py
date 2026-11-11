import os, subprocess, tempfile, time
import win32gui
from app import winapi as w, capture
w.set_dpi_aware()
d = os.path.join(tempfile.gettempdir(), "saydo_files_test"); os.makedirs(d, exist_ok=True)
files = [os.path.join(d, n) for n in ("report_week45.docx", "data.csv")]
for f in files: open(f, "w").write("x")
subprocess.Popen(["explorer.exe", "/select," + files[0]])
hwnd = 0
for _ in range(40):
    time.sleep(0.25)
    found = []
    def cb(h, _):
        if win32gui.GetClassName(h) == "CabinetWClass" and "saydo_files_test" in win32gui.GetWindowText(h): found.append(h)
    win32gui.EnumWindows(cb, None)
    if found: hwnd = found[0]; break
print("explorer hwnd:", hwnd)
if hwnd:
    time.sleep(0.8)
    ok = w.force_foreground(hwnd); fg = w.foreground()
    print("foreground:", ok, fg["exe"], fg["title"])
    if fg["exe"].lower() == "explorer.exe" and "saydo_files_test" in fg["title"]:
        w.set_clipboard_text("原来的剪贴板")
        print("grab (1 file selected):", capture.grab_selection(fg))
        w.send_keys([(0x11, False), (0x41, False), (0x41, True), (0x11, True)]); time.sleep(0.3)  # Ctrl+A 全选（在我自己的资源管理器窗口里）
        print("grab (all selected):", capture.grab_selection(fg))
        with w.Clipboard() as c: print("clipboard restored:", w.parse_text(c.get_raw(w.CF_UNICODETEXT)))
    else:
        print("ABORT: 前台不是我打开的资源管理器窗口，未发送任何按键")
    win32gui.PostMessage(hwnd, 0x0010, 0, 0)  # WM_CLOSE 关掉测试窗口
