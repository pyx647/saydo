"""系统托盘图标（pywin32 Shell_NotifyIcon）。左键 = 设置；右键菜单：设置 / 暂停热键 / 退出。"""
import os

import win32api
import win32con
import win32gui

WM_TRAY = win32con.WM_USER + 20
ID_SETTINGS, ID_PAUSE, ID_QUIT = 1001, 1002, 1003


class Tray:
    def __init__(self, icon_path, on_settings, on_toggle_pause, is_paused, on_quit):
        self.icon_path, self.on_settings, self.on_toggle_pause, self.is_paused, self.on_quit = icon_path, on_settings, on_toggle_pause, is_paused, on_quit
        self.hwnd = 0

    def run(self):
        hinst = win32api.GetModuleHandle(None)
        cls = "SayDoTray"
        wc = win32gui.WNDCLASS()
        wc.hInstance, wc.lpszClassName = hinst, cls
        wc.lpfnWndProc = {win32con.WM_DESTROY: self._destroy, WM_TRAY: self._notify, win32con.WM_COMMAND: self._command}
        try:
            win32gui.RegisterClass(wc)
        except Exception:
            pass
        self.hwnd = win32gui.CreateWindow(cls, "SayDoTray", 0, 0, 0, 0, 0, 0, 0, hinst, None)
        flags = win32con.LR_LOADFROMFILE | win32con.LR_DEFAULTSIZE
        try:
            hicon = win32gui.LoadImage(0, self.icon_path, win32con.IMAGE_ICON, 0, 0, flags) if os.path.exists(self.icon_path) else win32gui.LoadIcon(0, win32con.IDI_APPLICATION)
        except Exception:
            hicon = win32gui.LoadIcon(0, win32con.IDI_APPLICATION)
        nid = (self.hwnd, 0, win32gui.NIF_ICON | win32gui.NIF_MESSAGE | win32gui.NIF_TIP, WM_TRAY, hicon, "SayDo — 选中 + 按住热键 + 说话")
        win32gui.Shell_NotifyIcon(win32gui.NIM_ADD, nid)
        win32gui.PumpMessages()

    def _menu(self):
        m = win32gui.CreatePopupMenu()
        win32gui.AppendMenu(m, win32con.MF_STRING, ID_SETTINGS, "设置 / 历史")
        win32gui.AppendMenu(m, win32con.MF_STRING, ID_PAUSE, "恢复热键" if self.is_paused() else "暂停热键")
        win32gui.AppendMenu(m, win32con.MF_SEPARATOR, 0, "")
        win32gui.AppendMenu(m, win32con.MF_STRING, ID_QUIT, "退出 SayDo")
        pos = win32gui.GetCursorPos()
        win32gui.SetForegroundWindow(self.hwnd)
        win32gui.TrackPopupMenu(m, win32con.TPM_LEFTALIGN | win32con.TPM_BOTTOMALIGN, pos[0], pos[1], 0, self.hwnd, None)
        win32gui.PostMessage(self.hwnd, win32con.WM_NULL, 0, 0)

    def _notify(self, hwnd, msg, wparam, lparam):
        if lparam == win32con.WM_LBUTTONUP:
            self.on_settings()
        elif lparam == win32con.WM_RBUTTONUP:
            self._menu()
        return 1

    def _command(self, hwnd, msg, wparam, lparam):
        cid = win32api.LOWORD(wparam)
        if cid == ID_SETTINGS:
            self.on_settings()
        elif cid == ID_PAUSE:
            self.on_toggle_pause()
        elif cid == ID_QUIT:
            self.quit()
        return 0

    def quit(self):
        try:
            win32gui.Shell_NotifyIcon(win32gui.NIM_DELETE, (self.hwnd, 0))
        except Exception:
            pass
        self.on_quit()
        win32gui.PostQuitMessage(0)

    def _destroy(self, hwnd, msg, wparam, lparam):
        win32gui.PostQuitMessage(0)
        return 0
