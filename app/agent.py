"""Agent 调度：把"语音指令 + 当前上下文"交给后端（Claude Code / Codex / agy / OpenAI API），
流式把进度和结果推给弹窗，最后解析出结果文件。

为什么主力是 CLI：Claude Code / Codex / agy 在本机已经配好了 Google 连接器、各种 MCP 服务器和 Skills，
无头模式下原样继承。直接调 OpenAI API 则只有我们自己实现的那点工具，所以它只作"快速模式"。"""
import base64
import json
import os
import shutil
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

from . import config, ui, winapi as w


class Cancelled(Exception):
    pass


# ───────────────────────── 进度汇报 ─────────────────────────

class Sink:
    def __init__(self, cancel: threading.Event, on_proc=None):
        self.cancel = cancel
        self.on_proc = on_proc or (lambda p: None)
        self.steps, self.text = [], ""
        self.t0 = time.time()
        self._last = 0.0
        self._lock = threading.Lock()

    def _flush(self, force=False):
        now = time.time()
        if not force and now - self._last < 0.09:
            return
        self._last = now
        tail = self.steps[-5:]
        ui.state({"phase": "working", "steps": tail, "stepCount": len(self.steps), "text": self.text, "elapsed": int(now - self.t0)})

    def step(self, s):
        with self._lock:
            if not self.steps or self.steps[-1] != s:
                self.steps.append(s)
            self._flush(True)

    def clear_text(self):
        with self._lock:
            self.text = ""

    def push(self, d):
        with self._lock:
            self.text += d
            self._flush()

    def set_text(self, t):
        with self._lock:
            self.text = t
            self._flush(True)

    def heartbeat(self):
        with self._lock:
            self._flush(True)


def workspace_dir() -> str:
    d = os.path.join(config.config_dir(), "workspace")
    os.makedirs(d, exist_ok=True)
    return d


def _human(n):
    return f"{n / (1 << 20):.1f} MB" if n >= 1 << 20 else f"{n >> 10} KB" if n >= 1 << 10 else f"{n} B"


def _trunc(s, n):
    s = s.replace("\r", " ").replace("\n", " ")
    return s[:n] + "…" if len(s) > n else s


# ───────────────────────── 提示词 ─────────────────────────

def build_prompt(inp: dict, with_mcp: bool):
    """inp: instruction, fg, sel_kind, sel_val, shot, dest, prev。返回 (prompt, 需额外授权的目录)"""
    cfg = config.get()
    dirs = []
    fg = inp["fg"]
    ctx = f"- 前台程序：{fg.get('exe') or '未知'}，窗口标题：「{_trunc(fg.get('title', ''), 120)}」\n"
    if inp.get("shot"):
        ctx += f"- 屏幕截图：`{inp['shot']}`（用户说的是\"当前屏幕/页面\"，请先查看这张截图再回答；忽略任何选中内容）\n"
        dirs.append(os.path.dirname(inp["shot"]))
    else:
        k, v = inp["sel_kind"], inp["sel_val"]
        if k == "text":
            if len(v) > 6000:
                p = os.path.join(workspace_dir(), f"selection-{int(time.time() * 1000)}.txt")
                with open(p, "w", encoding="utf-8") as f:
                    f.write(v)
                ctx += f"- 选中文字（共 {len(v)} 字，太长已存为文件 `{p}`，开头预览如下）：\n\"\"\"\n{v[:500]}…\n\"\"\"\n"
            else:
                ctx += f"- 选中文字（{len(v)} 字）：\n\"\"\"\n{v}\n\"\"\"\n"
        elif k == "files":
            ctx += "- 选中的文件：\n"
            for f in v:
                fv = ui.file_view(f)
                ctx += f"  - `{f}`（{'文件夹' if fv['isDir'] else fv['ext']}，{_human(fv['size'])}）\n"
                d = os.path.dirname(f)
                if d not in dirs:
                    dirs.append(d)
        else:
            ctx += "- 选中内容：无\n"
    if inp.get("shot_saved") and not inp.get("shot"):
        ctx += f"- 按下热键时自动保存了一张屏幕截图：`{inp['shot_saved']}`（我没看过它；只有任务确实要看屏幕时才读取，选中的文字/文件与它无关时不要读）\n"
        dirs.append(os.path.dirname(inp["shot_saved"]))
    if inp.get("dest"):
        ctx += f"- 目标文件夹：用户已在弹窗里选定 `{inp['dest']}`，直接使用，不要再问。\n"
        dirs.append(inp["dest"])

    p = "你是 SayDo —— 用户在 Windows 11 上通过【选中内容 + 按住热键说话】唤起的桌面快捷 Agent。\n"
    p += "你的回答会显示在一个很小的弹窗里：只给结论或成品，简洁，不寒暄、不复述任务、不解释你做了哪些步骤（除非用户问）。用户说什么语言就用什么语言回答。\n\n"
    p += "## 当前上下文\n" + ctx
    if inp.get("prev"):
        pi, pa = inp["prev"]
        p += f"\n## 上一轮对话（用户在追问）\n用户说：{_trunc(pi, 300)}\n你答：{_trunc(pa, 1500)}\n"
    p += f"\n## 用户的语音指令\n「{inp['instruction'].strip()}」\n\n## 规则\n"
    p += "1. 指令针对\"当前页面/屏幕/界面\"时，以截图为准；否则以选中的文字/文件为操作对象。\n"
    p += "2. 对文件操作（转格式、重命名、压缩、提取内容等）：直接动手做。输出文件默认放在原文件同一目录；绝不覆盖已存在的文件（重名就加 (1)）；删除只能进回收站。\n"
    if with_mcp:
        p += "3. 需要用户选择文件夹时，调用 saydo 的 `pick_folder` 工具；需要用户拍板（发邮件、对外分享、覆盖、不可逆操作）时，调用 `confirm` 工具，不要自己猜。需要看屏幕时，上下文里有截图文件就直接读那个文件，没有才调用 `screenshot`；选中的文字/文件已经在上面的上下文里，不要为了它们截屏，也不要再调用 get_context。\n"
    else:
        p += "3. 需要用户选择目标位置而上下文里没有时，在回答里直接说明需要什么，不要猜。\n"
    p += "4. 你本机配好的所有 MCP（Gmail / Drive / 日历 / Sheets 等）和 Skills 都可以用来完成任务。\n"
    p += "5. 如果你创建、移动、重命名或修改了文件，在回答的最后每个文件单独一行写：`FILE: <绝对路径>`（这些行会被弹窗渲染成\"打开 / 定位\"按钮）。\n"
    if cfg["extraPrompt"].strip():
        p += f"\n## 用户的常驻偏好\n{cfg['extraPrompt'].strip()}\n"
    return p, dirs


def extract_files(text: str):
    files, body = [], []
    for line in text.splitlines():
        t = line.strip().lstrip("-*• 📎").strip()
        if t.upper().startswith(("FILE:", "FILE：")):
            path = t[5:].strip().strip("`\"'* ").replace("/", "\\")
            if os.path.exists(path) and path not in files:
                files.append(path)
        else:
            body.append(line)
    return "\n".join(body).strip(), files


# ───────────────────────── 通用：流式子进程 ─────────────────────────

def find_exe(name: str, override: str = ""):
    o = (override or "").strip().strip('"')
    if o:
        return o if os.path.exists(o) else None
    return shutil.which(name)


def _kill_tree(pid):
    subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], capture_output=True, creationflags=w.CREATE_NO_WINDOW)


def run_stream(exe, args, stdin_text, cwd, sink, on_line):
    """逐行读 JSON 事件。返回 stderr 尾部。取消时杀进程树并抛 Cancelled。"""
    proc = subprocess.Popen([exe] + args, stdin=subprocess.PIPE if stdin_text is not None else subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, cwd=cwd, creationflags=w.CREATE_NO_WINDOW, encoding="utf-8", errors="replace")
    sink.on_proc(proc)
    err_lines = []

    def read_err():
        for ln in proc.stderr:
            if sum(map(len, err_lines)) < 4000:
                err_lines.append(ln)

    def write_in():
        try:
            proc.stdin.write(stdin_text)
            proc.stdin.close()
        except Exception:
            pass

    def beat():
        while proc.poll() is None:
            if sink.cancel.is_set():
                _kill_tree(proc.pid)
                return
            sink.heartbeat()
            time.sleep(0.5)

    threading.Thread(target=read_err, daemon=True).start()
    if stdin_text is not None:
        threading.Thread(target=write_in, daemon=True).start()
    threading.Thread(target=beat, daemon=True).start()
    for line in proc.stdout:
        if sink.cancel.is_set():
            break
        try:
            on_line(json.loads(line))
        except ValueError:
            pass
    if sink.cancel.is_set():
        _kill_tree(proc.pid)
        raise Cancelled()
    try:
        proc.wait(timeout=8)
    except subprocess.TimeoutExpired:
        _kill_tree(proc.pid)
    sink.on_proc(None)
    return "".join(err_lines)


def _need(name, c, hint):
    exe = find_exe(name, c["path"])
    if not exe:
        raise RuntimeError(f"找不到 {name} 命令行。{hint}（设置 → 后端 里可手动指定路径）")
    return exe


def _extra(c):
    return c["extraArgs"].split()


def mcp_python() -> str:
    """MCP 子进程需要 stdio，所以必须是 python.exe 而不是 pythonw.exe"""
    return sys.executable.replace("pythonw.exe", "python.exe")


MCP_SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mcp_stdio.py")


# ───────────────────────── Claude Code ─────────────────────────

def _claude_tool(name, inp):
    s = lambda k: str(inp.get(k, ""))
    if name in ("Bash", "PowerShell"):
        return "运行命令：" + _trunc(s("description") or s("command"), 56)
    if name == "Read":
        return "读取：" + _trunc(os.path.basename(s("file_path")), 40)
    if name in ("Write", "Edit", "MultiEdit", "NotebookEdit"):
        return "写入文件"
    if name in ("Glob", "Grep"):
        return "搜索文件"
    if name in ("WebFetch", "WebSearch"):
        return "联网查询"
    if name == "Skill":
        return "使用技能：" + s("skill")
    if name in ("Task", "Agent"):
        return "派出子任务"
    if name.startswith("mcp__"):
        rest = name[5:].split("__", 1)
        return f"{rest[0].replace('claude_ai_', '').replace('_', ' ')} · {(rest[1] if len(rest) > 1 else '').replace('_', ' ')}"
    return name


def _claude_handler(sink, res):
    """Claude stream-json 事件 → 进度/文本；res 收集最终结果"""
    def on_line(v):
        t = v.get("type")
        if t == "stream_event" and v.get("parent_tool_use_id") is None:
            e = v.get("event", {})
            if e.get("type") == "content_block_start" and e.get("content_block", {}).get("type") == "text":
                sink.clear_text()
            elif e.get("type") == "content_block_delta" and e.get("delta", {}).get("type") == "text_delta":
                sink.push(e["delta"].get("text", ""))
        elif t == "assistant" and v.get("parent_tool_use_id") is None:
            for b in v.get("message", {}).get("content", []) or []:
                if b.get("type") == "tool_use":
                    sink.step(_claude_tool(b.get("name", ""), b.get("input") or {}))
        elif t == "result":
            res["text"] = v.get("result")
            res["err"] = bool(v.get("is_error"))
    return on_line


def _claude_args(cfg, dirs=(), oneshot=True):
    mcp_path = os.path.join(config.config_dir(), "mcp-saydo.json")
    with open(mcp_path, "w", encoding="utf-8") as f:
        json.dump({"mcpServers": {"saydo": {"command": mcp_python(), "args": [MCP_SCRIPT]}}}, f)
    args = ["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--mcp-config", mcp_path]
    if not oneshot:
        args += ["--input-format", "stream-json"]
    if cfg["approval"] == "auto":
        args += ["--permission-mode", "bypassPermissions"]
    else:
        args += ["--permission-mode", "acceptEdits", "--permission-prompt-tool", "mcp__saydo__approve"]
    if not cfg["keepHooks"]:
        # 用户 ~/.claude 里的 SessionStart/Stop hooks 会让每次调用多花十几秒；MCP/Skills 不受影响
        args += ["--settings", '{"disableAllHooks":true}']
    for d in dirs:
        args += ["--add-dir", d]
    if cfg["claude"]["model"].strip():
        args += ["--model", cfg["claude"]["model"].strip()]
    return args + _extra(cfg["claude"])


class ClaudeWarm:
    """常驻的 Claude Code 进程（--input-format stream-json）。
    `claude -p` 每次冷启动要 15~18 秒（和 MCP 无关，是 Claude Code 自身启动），常驻后每条消息 4~6 秒。
    代价：同一进程里对话上下文会累积 → 每个任务在提示词里声明"全新独立任务"，并且每 MAX_TURNS 轮重启一次。"""
    MAX_TURNS = 10

    def __init__(self):
        self.proc = None
        self.sig = None
        self.turns = 0
        self.cur = None
        self.ready = threading.Event()
        self.alive = False
        self._plock = threading.RLock()
        self._req = threading.Lock()

    @staticmethod
    def _sig(cfg):
        return json.dumps([cfg["approval"], cfg["keepHooks"], cfg["claude"]], sort_keys=True)

    def ensure(self):
        cfg = config.get()
        exe = find_exe("claude", cfg["claude"]["path"])
        if not exe:
            return False
        with self._plock:
            sig = self._sig(cfg)
            if self.proc and self.proc.poll() is None and self.sig == sig and self.turns < self.MAX_TURNS:
                return True
            self._kill_locked()
            self.sig, self.turns = sig, 0
            self.ready = threading.Event()
            self.ready.set()  # 流式输入模式下 init 事件要等到第一条消息才发；进程一启动就能写入，不用等握手
            self.proc = subprocess.Popen([exe] + _claude_args(cfg, oneshot=False), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                         stderr=subprocess.PIPE, cwd=workspace_dir(), creationflags=w.CREATE_NO_WINDOW, encoding="utf-8", errors="replace")
            threading.Thread(target=self._read, args=(self.proc, self.ready), daemon=True, name="claude-warm").start()
            threading.Thread(target=lambda p=self.proc: [None for _ in p.stderr], daemon=True).start()
            return True

    def _read(self, proc, ready):
        try:
            for line in proc.stdout:
                try:
                    v = json.loads(line)
                except ValueError:
                    continue
                if v.get("type") == "system" and v.get("subtype") == "init":
                    ready.set()
                h = self.cur
                if h and proc is self.proc:
                    h(v)
        finally:
            ready.set()
            h = self.cur
            if h and proc is self.proc:
                h({"type": "__eof"})

    def _kill_locked(self):
        if self.proc and self.proc.poll() is None:
            _kill_tree(self.proc.pid)
        self.proc = None

    def kill(self):
        with self._plock:
            self._kill_locked()

    def ask(self, prompt, sink):
        with self._req:
            if not self.ensure():
                raise RuntimeError("找不到 claude 命令行。请先安装 Claude Code（设置 → 后端 里可手动指定路径）")
            res = {"text": None, "err": False, "eof": False}
            done = threading.Event()
            inner = _claude_handler(sink, res)

            def handler(v):
                if v.get("type") == "__eof":
                    res["eof"] = True
                    done.set()
                    return
                inner(v)
                if v.get("type") == "result":
                    done.set()

            self.cur = handler
            proc = self.proc
            try:
                msg = {"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": prompt}]}}
                proc.stdin.write(json.dumps(msg, ensure_ascii=False) + "\n")
                proc.stdin.flush()
                while not done.wait(0.4):
                    if sink.cancel.is_set():
                        self.kill()
                        raise Cancelled()
                    sink.heartbeat()
            finally:
                self.cur = None
            self.turns += 1
            if res["eof"] or res["text"] is None:
                self.kill()
                raise RuntimeError("Claude Code 进程意外退出，已重启，请再试一次")
            if res["err"]:
                raise RuntimeError("Claude Code 报错：" + _trunc(res["text"], 300))
            if self.turns >= self.MAX_TURNS:
                threading.Thread(target=self.ensure, daemon=True).start()  # 提前换一个新的，避免上下文越滚越大
            return res["text"]


WARM = ClaudeWarm()


def prewarm():
    """应用启动 / 配置变更后调用：后端是 claude 且开了预热，就提前把进程拉起来"""
    try:
        c = config.get()
        if c["backend"] == "claude" and c.get("warmClaude", True):
            WARM.ensure()
        else:
            WARM.kill()
    except Exception:
        pass


def run_claude(inp, sink):
    cfg = config.get()
    exe = _need("claude", cfg["claude"], "请先安装 Claude Code")
    prompt, dirs = build_prompt(inp, True)
    if cfg.get("warmClaude", True):
        sink.step("交给 Claude Code…")
        prompt = "【新任务】下面是一个全新的、独立的任务，与此前对话无关；不要引用之前的内容（除非下面的“上一轮对话”里给出）。\n\n" + prompt
        return WARM.ask(prompt, sink)
    sink.step("启动 Claude Code…")
    res = {"text": None, "err": False}
    tail = run_stream(exe, _claude_args(cfg, dirs), prompt, workspace_dir(), sink, _claude_handler(sink, res))
    if res["text"] is None:
        raise RuntimeError("Claude Code 没有返回结果。" + _trunc(tail, 300))
    if res["err"]:
        raise RuntimeError("Claude Code 报错：" + _trunc(res["text"], 300))
    return res["text"]


# ───────────────────────── Codex ─────────────────────────

def run_codex(inp, sink):
    cfg = config.get()
    exe = _need("codex", cfg["codex"], "请先安装 Codex CLI（npm i -g @openai/codex）")
    prompt, _ = build_prompt(inp, True)
    ws = workspace_dir()
    args = ["exec", "--json", "--skip-git-repo-check"]
    if inp.get("shot"):
        args += ["-i", inp["shot"]]
    args += ["--sandbox", "danger-full-access" if cfg["fullAccess"] else "workspace-write", "-c", 'approval_policy="never"']
    # TOML 字面量字符串（单引号）：Windows 路径里的反斜杠不用转义
    args += ["-c", f"mcp_servers.saydo.command='{mcp_python()}'", "-c", f"mcp_servers.saydo.args=['{MCP_SCRIPT}']", "-C", ws]
    if cfg["codex"]["model"].strip():
        args += ["-m", cfg["codex"]["model"].strip()]
    args += _extra(cfg["codex"]) + ["-"]
    sink.step("启动 Codex…")
    res = {"text": None, "fail": None}

    def on_line(v):
        t = v.get("type")
        it = v.get("item") or {}
        if t == "item.started":
            k = it.get("type")
            if k == "command_execution":
                sink.step("运行命令：" + _trunc(it.get("command", ""), 56))
            elif k == "mcp_tool_call":
                sink.step(f"{it.get('server', 'mcp')} · {str(it.get('tool', '')).replace('_', ' ')}")
            elif k == "web_search":
                sink.step("联网查询")
            elif k == "file_change":
                sink.step("修改文件")
        elif t == "item.completed" and it.get("type") == "agent_message":
            res["text"] = it.get("text", "")
            sink.set_text(res["text"])
        elif t == "turn.failed":
            res["fail"] = (v.get("error") or {}).get("message", "turn failed")
        elif t == "error":
            res["fail"] = v.get("message", "error")

    tail = run_stream(exe, args, prompt, ws, sink, on_line)
    if res["text"] is not None:
        return res["text"]
    raise RuntimeError("Codex 报错：" + _trunc(res["fail"], 300) if res["fail"] else "Codex 没有返回结果。" + _trunc(tail, 200))


# ───────────────────────── agy (Antigravity) ─────────────────────────

def run_agy(inp, sink):
    cfg = config.get()
    exe = _need("agy", cfg["agy"], "请先安装 Antigravity CLI")
    prompt, dirs = build_prompt(inp, False)
    args = ["--output-format", "stream-json", "--dangerously-skip-permissions"]
    for d in dirs:
        args += ["--add-dir", d]
    if cfg["agy"]["model"].strip():
        args += ["--model", cfg["agy"]["model"].strip()]
    args += _extra(cfg["agy"]) + ["-p", prompt]
    sink.step("启动 Antigravity…")
    res = {"text": None, "fail": None, "cur": -1}

    def on_line(v):
        ev = v.get("event")
        if ev == "step_update":
            su = v.get("step_update") or {}
            idx, st = su.get("step_index", -1), su.get("step_type")
            if st == "agent_response":
                if idx != res["cur"]:
                    res["cur"] = idx
                    sink.clear_text()
                if su.get("text_delta"):
                    sink.push(su["text_delta"])
            elif st not in (None, "user_input") and su.get("state") == "ACTIVE":
                sink.step(st.replace("_", " "))
        elif ev == "result":
            r = v.get("result") or {}
            if r.get("status") == "SUCCESS":
                res["text"] = (r.get("response") or "").strip()
            else:
                res["fail"] = str(r.get("status") or "失败")

    tail = run_stream(exe, args, None, workspace_dir(), sink, on_line)
    if res["text"] is not None:
        return res["text"]
    raise RuntimeError(f"agy 报错：{res['fail']}" if res["fail"] else "agy 没有返回结果。" + _trunc(tail, 200))


# ───────────────────────── OpenAI 快速模式 ─────────────────────────

TEXTY = {"txt", "md", "csv", "json", "log", "py", "js", "ts", "tsx", "rs", "html", "css", "xml", "yaml", "yml", "ini", "toml", "tex", "bib", "fasta", "fa", "sh", "ps1", "sql"}


def run_openai(inp, sink):
    c = config.get()["openai"]
    if not c["apiKey"].strip():
        raise RuntimeError("还没有配置 OpenAI API Key（设置 → 后端 → OpenAI 快速模式）")
    fg = inp["fg"]
    ctx = f"前台程序：{fg.get('exe')}（{fg.get('title')}）\n"
    parts = []
    if inp.get("shot"):
        with open(inp["shot"], "rb") as f:
            parts.append({"type": "image_url", "image_url": {"url": "data:image/png;base64," + base64.b64encode(f.read()).decode()}})
        ctx += "用户说的是当前屏幕/页面，已附上截图。\n"
    else:
        k, v = inp["sel_kind"], inp["sel_val"]
        if k == "text":
            ctx += f"选中文字：\n\"\"\"\n{v[:30000]}\n\"\"\"\n"
        elif k == "files":
            ctx += "选中文件：\n"
            for f in v:
                fv = ui.file_view(f)
                ctx += f"- {f} ({fv['size']} 字节)\n"
                if fv["ext"] in TEXTY and fv["size"] < 200_000:
                    try:
                        with open(f, encoding="utf-8", errors="replace") as fh:
                            ctx += f"  内容：\n\"\"\"\n{fh.read()[:20000]}\n\"\"\"\n"
                    except OSError:
                        pass
        else:
            ctx += "选中内容：无\n"
    if inp.get("prev"):
        ctx += f"\n上一轮：用户说「{inp['prev'][0]}」；你答：{inp['prev'][1][:1200]}\n"
    system = ("你是 SayDo，一个桌面快捷助手。回答显示在小弹窗里：简洁，只给结论或成品，不寒暄。用户说什么语言就用什么语言。"
              "你在快速模式下没有任何工具，不能操作文件或访问外部服务；如果任务需要，请直接告诉用户\"请在设置里切换到 Claude Code / Codex 后端\"。")
    parts.insert(0, {"type": "text", "text": f"## 上下文\n{ctx}\n## 用户的语音指令\n「{inp['instruction'].strip()}」"})
    body = {"model": c["model"], "stream": True, "messages": [{"role": "system", "content": system}, {"role": "user", "content": parts}]}
    req = urllib.request.Request(c["baseUrl"].rstrip("/") + "/chat/completions", data=json.dumps(body).encode("utf-8"),
                                 headers={"Authorization": f"Bearer {c['apiKey'].strip()}", "Content-Type": "application/json"})
    sink.step(f"调用 {c['model']}…")
    out = []
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            for raw in r:
                if sink.cancel.is_set():
                    raise Cancelled()
                line = raw.decode("utf-8", "ignore").strip()
                if line.startswith("data:") and line[5:].strip() != "[DONE]":
                    try:
                        d = json.loads(line[5:])["choices"][0]["delta"].get("content")
                    except (ValueError, KeyError, IndexError):
                        continue
                    if d:
                        out.append(d)
                        sink.push(d)
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"OpenAI 接口 {e.code}：{e.read().decode('utf-8', 'ignore')[:300]}")
    if not "".join(out).strip():
        raise RuntimeError("模型没有返回内容")
    return "".join(out)


RUNNERS = {"claude": run_claude, "codex": run_codex, "agy": run_agy, "openai": run_openai}


def run(inp: dict, sink: Sink):
    backend = config.get()["backend"]
    fn = RUNNERS.get(backend)
    if not fn:
        raise RuntimeError(f"未知后端：{backend}")
    return extract_files(fn(inp, sink))
