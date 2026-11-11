"""真机测试选区抓取。安全守卫：只有确认前台窗口是"我自己开的测试窗口 / 我自己开的资源管理器窗口"才发按键。"""
import os, subprocess, sys, tempfile, threading, time, tkinter as tk
from app import winapi as w, capture
w.set_dpi_aware()
result = {}

def worker(root, text_widget):
    try:
        time.sleep(0.4)
        hwnd = int(root.winfo_id()); hwnd = w.user32.GetParent(hwnd) or hwnd
        ok = w.force_foreground(hwnd)
        fg = w.foreground()
        result["fg_ok"] = ok and fg["pid"] == os.getpid()
        if not result["fg_ok"]:
            result["abort"] = f"前台不是测试窗口: {fg}"; return
        w.set_clipboard_text("用户原来的剪贴板")
        # 模拟用户此刻正按着 Alt（热键修饰键）：真的按下 Alt，验证抓取会临时松开再还原
        w.send_keys([(0xA4, False)]); time.sleep(0.05)  # 左 Alt，和真实键盘一致
        result["alt_before"] = w.key_down(0xA4)
        kind, val = capture.grab_selection(fg)
        result["alt_after"] = w.key_down(0xA4)
        w.send_keys([(0xA4, True)])
        result["grab"] = (kind, val)
        with w.Clipboard() as c:
            result["clip_after"] = w.parse_text(c.get_raw(w.CF_UNICODETEXT))
    finally:
        root.after(0, root.destroy)

root = tk.Tk(); root.title("SayDo capture test"); root.geometry("420x120+300+300")
t = tk.Text(root); t.pack(fill="both", expand=True); t.insert("1.0", "你好 SayDo 选区抓取测试 12345 — Ctrl+Alt 不应变成 Ctrl+Alt+C")
t.tag_add("sel", "1.0", "end-1c"); t.focus_set()
threading.Thread(target=worker, args=(root, t), daemon=True).start()
root.mainloop()
for k, v in result.items(): print(k, "=", v)
