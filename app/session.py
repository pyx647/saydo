"""一次"按住说话"的完整生命周期：
按下 → 录音 + 抓选区 → 松开 → 转写 → 路由(屏幕/选文件夹) → agent → 弹窗给结果；以及"短按/打字/追问"走的同一条执行通路。"""
import itertools
import os
import subprocess
import threading
import time
import wave

from . import agent, capture, config, history, router, stt, tools, ui, winapi as w

_cur = None
_cur_lock = threading.Lock()
_press_seq = itertools.count(1)
_last_seq = 0
_run_cancel = threading.Event()
_run_proc = None


class Session:
    def __init__(self, sid, fg, cursor, sel_kind="none", sel_val=None, ready=False):
        self.id, self.fg, self.cursor = sid, fg, cursor
        self.sel_kind, self.sel_val = sel_kind, sel_val
        self.ready = threading.Event()
        if ready:
            self.ready.set()
        self.rec = None
        self.last = None      # (指令, 回答)，追问用
        self.screen = False
        self.shot = None      # 按下热键那一刻自动留下的屏幕截图（路径）——只保存，不读；用得上再读
        self.shot_ready = threading.Event()


def set_proc(p):
    global _run_proc
    _run_proc = p


def current():
    with _cur_lock:
        return _cur


def current_source_dir():
    s = current()
    if s and s.sel_kind == "files" and s.sel_val:
        return os.path.dirname(s.sel_val[0])
    return None


def current_fg_cursor():
    s = current()
    return (s.fg, s.cursor) if s else (w.foreground(), w.cursor_pos())


def describe_context() -> str:
    s = current()
    if not s:
        return "当前没有上下文"
    o = f"前台程序：{s.fg.get('exe')}，窗口标题：{s.fg.get('title')}\n"
    if s.sel_kind == "text":
        return o + f"选中文字：\n{s.sel_val}"
    if s.sel_kind == "files":
        return o + "选中文件：\n" + "\n".join(s.sel_val)
    return o + "选中内容：无"


def cancel_current():
    """取消当前进行中的一切：agent 子进程、等待中的提问、录音"""
    global _run_cancel
    _run_cancel.set()
    ui.cancel_asks()
    p = _run_proc
    if p is not None and p.poll() is None:
        subprocess.run(["taskkill", "/PID", str(p.pid), "/T", "/F"], capture_output=True, creationflags=w.CREATE_NO_WINDOW)
    s = current()
    if s and s.rec:
        try:
            s.rec.stop()
        except Exception:
            pass
        s.rec = None


def _new_run_flag() -> threading.Event:
    global _run_cancel
    _run_cancel = threading.Event()
    return _run_cancel


class _FakeRecorder:
    """测试钩子：SAYDO_TEST_WAV 指向一个 16kHz 单声道 wav 时，用它代替麦克风"""

    def __init__(self, path):
        with wave.open(path) as f:
            self.pcm = f.readframes(f.getnframes())

    def start(self):
        pass

    def snapshot(self):
        return self.pcm

    def stop(self):
        return self.pcm


def _make_recorder():
    t = os.environ.get("SAYDO_TEST_WAV")
    if t:
        return _FakeRecorder(t)
    return w.Recorder(level_cb=lambda v: ui.emit("audio-level", v))


def _caption_loop(s, rec):
    """实时字幕：按住期间每 ~0.6s 用本地模型把已录的音频整段重识别一次，让用户边说边看到识别结果。
    （SenseVoice 不是流式模型，但识别 3 秒音频只要零点几秒，整段重识别足够快。云端引擎不支持，松开后才出字。）"""
    last_n = 0
    while s.rec is rec:
        time.sleep(0.6)
        if s.rec is not rec:
            return
        cfg = config.get()
        if cfg["sttEngine"] != "local" or not stt.model_ready():
            return
        pcm = rec.snapshot()
        if len(pcm) < 32000 * 0.7 or len(pcm) == last_n:
            continue
        last_n = len(pcm)
        try:
            t = stt.transcribe_local(pcm, cfg["lang"])
        except Exception:
            continue
        if t and s.rec is rec:
            ui.state({"phase": "caption", "text": t})


# ───────────────────────── 按下 ─────────────────────────

def _save_shot(s: Session, img):
    try:
        s.shot = capture.save_image(img, os.path.join(config.config_dir(), "shots"))
    except Exception as e:
        print("[session] 截图存盘失败:", e, flush=True)
    finally:
        s.shot_ready.set()


def on_down():
    global _cur, _last_seq
    seq = next(_press_seq)
    _last_seq = seq
    cancel_current()
    fg, cursor = w.foreground(), w.cursor_pos()
    prev = current()
    from_self = fg["pid"] == os.getpid() or ui.find_window(ui.PANEL_TITLE) == fg["hwnd"]
    # 前台就是 SayDo 自己的弹窗 → 这是在对上一轮结果"追问"：沿用上一轮的上下文，不再抓选区
    if from_self and prev:
        s = Session(seq, prev.fg, cursor, prev.sel_kind, prev.sel_val, ready=True)
        s.last = prev.last
    else:
        s = Session(seq, fg, cursor, ready=False)
    with _cur_lock:
        _cur = s
    tools.reset_allow()
    try:
        s.rec = _make_recorder()
        s.rec.start()
    except Exception as e:
        ui.WIN.show_panel_at(cursor)
        ui.error(str(e))
        s.ready.set()
        return
    # 按下热键的同一刻：①抓屏（趁弹窗还没出现）②读选中的文字/文件（下面）③录音（上面已开始）。截图只存盘，不读、不发给任何模型。
    if from_self and prev:
        s.shot = prev.shot
        s.shot_ready.set()
    else:
        if ui.WIN.panel_visible():
            ui.WIN.hide_panel()   # 上一轮的结果卡还在的话先收掉，免得被截进去
            time.sleep(0.04)
        try:
            img = capture.grab_image(s.fg["hwnd"])
            threading.Thread(target=_save_shot, args=(s, img), daemon=True, name="shot").start()
        except Exception as e:
            print("[session] 抓屏失败:", e, flush=True)
            s.shot_ready.set()
    if not (from_self and prev) or not ui.WIN.panel_visible():
        ui.WIN.show_panel_at(cursor)
    ui.interactive(False)
    pending = {"kind": "pending", "label": "正在读取选中内容…", "preview": "", "files": [], "app": s.fg.get("exe", ""), "title": s.fg.get("title", "")}
    ui.state({"phase": "listening", "followUp": bool(from_self and prev), "ctx": ui.ctx_view(s.fg, s.sel_kind, s.sel_val) if s.ready.is_set() else pending})
    threading.Thread(target=_caption_loop, args=(s, s.rec), daemon=True, name="caption").start()
    if not s.ready.is_set():
        try:
            s.sel_kind, s.sel_val = capture.grab_selection(s.fg)
        except Exception:
            s.sel_kind, s.sel_val = "none", None
        ui.state({"phase": "ctx", "ctx": ui.ctx_view(s.fg, s.sel_kind, s.sel_val)})
        s.ready.set()


# ───────────────────────── 松开 ─────────────────────────

def on_up():
    seq = _last_seq
    t0 = time.time()
    s = None
    while time.time() - t0 < 3:
        c = current()
        if c and c.id == seq:
            s = c
            break
        time.sleep(0.02)
    if not s:
        return
    s.ready.wait(2.5)
    rec, s.rec = s.rec, None
    if rec is None:
        return
    pcm = rec.stop()
    cfg = config.get()
    if len(pcm) < 32000 * 0.3:     # 短按（<0.3s）：不是说话，是想打字
        if cfg["tapOpensInput"]:
            show_input(s)
        else:
            ui.WIN.hide_panel()
        return
    ui.state({"phase": "transcribing"})
    try:
        text = stt.transcribe(pcm)
    except Exception as e:
        ui.error(str(e))
        return
    if not text.strip():
        ui.error("没听清，请再按住说一次")
        return
    ui.state({"phase": "caption", "text": text.strip(), "final": True})
    run_instruction(s, text.strip())


def show_input(s: Session):
    ui.interactive(True)
    ui.state({"phase": "input", "ctx": ui.ctx_view(s.fg, s.sel_kind, s.sel_val, s.screen), "hasPrev": s.last is not None})
    ui.WIN.focus_panel()


# ───────────────────────── 执行 ─────────────────────────

def submit_text(text: str):
    """面板里打字 / 点快捷芯片 / 追问"""
    t = (text or "").strip()
    if not t:
        return
    cancel_current()
    s = current()
    if s is None:
        s = Session(0, w.foreground(), w.cursor_pos(), ready=True)
        with _cur_lock:
            globals()["_cur"] = s
    threading.Thread(target=run_instruction, args=(s, t), daemon=True).start()


def replace_original(text: str):
    s = current()
    hwnd = s.fg["hwnd"] if s else 0
    ui.WIN.hide_panel()
    capture.paste_back(hwnd, text)


def run_instruction(s: Session, text: str):
    cfg = config.get()
    cancel = _new_run_flag()
    ui.interactive(False)
    ui.state({"phase": "heard", "text": text})
    sel_kind, sel_val = s.sel_kind, s.sel_val
    shot = None
    dest = None

    s.shot_ready.wait(2)
    saved_shot = s.shot if s.shot and os.path.exists(s.shot) else None

    if router.is_screen_intent(text):
        if saved_shot:
            shot = saved_shot   # 直接用按下热键时留下的那张（那时弹窗还没出现，拍到的就是用户看的页面）
        else:
            ui.state({"phase": "working", "steps": ["截取当前屏幕…"], "stepCount": 1, "text": "", "elapsed": 0})
            ui.WIN.hide_panel()
            time.sleep(0.18)
            try:
                shot = capture.screenshot(s.fg["hwnd"], s.cursor, os.path.join(config.config_dir(), "shots"))
            except Exception as e:
                ui.WIN.show_panel_at(s.cursor)
                return ui.error(f"截屏失败：{e}")
            ui.WIN.show_panel_at(s.cursor)
        sel_kind, sel_val = "none", None
        s.screen = True
        ui.state({"phase": "ctx", "ctx": ui.ctx_view(s.fg, "none", None, True)})
    else:
        s.screen = False
        if router.needs_folder(text, sel_kind):
            label = os.path.basename(sel_val[0]) if len(sel_val) == 1 else f"{len(sel_val)} 个文件"
            try:
                v = ui.ask({"kind": "folder", "title": "放到哪个文件夹？", "detail": label})
            except RuntimeError:
                return
            if not v.get("path"):
                ui.WIN.hide_panel()
                return
            config.remember_folder(v["path"])
            dest = v["path"]
            ui.interactive(False)
    if cancel.is_set():
        return

    ctx_label = ui.ctx_view(s.fg, sel_kind, sel_val, shot is not None)["label"]
    inp = {"instruction": text, "fg": s.fg, "sel_kind": sel_kind, "sel_val": sel_val, "shot": shot, "shot_saved": None if shot else saved_shot, "dest": dest, "prev": s.last}
    sink = agent.Sink(cancel, on_proc=set_proc)
    sink.step("思考中…")
    t0 = time.time()
    try:
        out_text, files = agent.run(inp, sink)
    except agent.Cancelled:
        return
    except Exception as e:
        if cancel.is_set():
            return
        history.push(text, str(e), cfg["backend"], ctx_label, [], False)
        return ui.error(str(e))
    if cancel.is_set():
        return
    s.last = (text, out_text)
    history.push(text, out_text, cfg["backend"], ctx_label, files, True)
    if not ui.WIN.panel_visible():
        ui.WIN.show_panel_at(s.cursor)
    ui.interactive(True)
    ui.state({"phase": "result", "text": out_text, "files": [ui.file_view(f) for f in files],
              "canReplace": s.sel_kind == "text" and not s.screen, "backend": cfg["backend"],
              "elapsed": int(time.time() - t0), "instruction": text})


def test_backend(backend=None) -> str:
    """设置页"测试后端"：不带上下文，让后端只回两个字"""
    cfg = config.get()
    if backend:
        c = dict(cfg, backend=backend)
        config.save(c)
    try:
        sink = agent.Sink(threading.Event())
        t0 = time.time()
        text, _ = agent.run({"instruction": "这是连通性测试。只回复两个字：成功", "fg": {"exe": "", "title": ""}, "sel_kind": "none", "sel_val": None,
                             "shot": None, "dest": None, "prev": None}, sink)
        return f"{text.strip()}（{time.time() - t0:.1f}s）"
    finally:
        if backend:
            config.save(cfg)
