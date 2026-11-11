"""SayDo 暴露给 CLI agent 的工具（经内置 MCP 服务器）：pick_folder / confirm / ask_user / screenshot / get_context / approve。
`approve` 是 Claude Code 的 --permission-prompt-tool：每次它要执行需要授权的动作，都先问 SayDo，
由我们按"审批强度"自动放行，或弹窗让用户点。"""
import base64
import json
import os
import threading
import time

from . import capture, config, ui, winapi as w

_allow_always = set()
_allow_lock = threading.Lock()


def reset_allow():
    with _allow_lock:
        _allow_always.clear()


def _text(s: str):
    return [{"type": "text", "text": s}]


def dispatch(tool: str, args: dict):
    """返回 MCP content 数组；出错抛异常"""
    from . import session
    if tool == "pick_folder":
        v = ui.ask({"kind": "folder", "title": args.get("prompt") or "选择一个文件夹"})
        p = v.get("path") if isinstance(v, dict) else None
        if p:
            config.remember_folder(p)
            return _text(f"用户选择的文件夹：{p}")
        return _text("用户取消了选择，请停止当前操作并告诉用户已取消。")
    if tool == "confirm":
        v = ui.ask({"kind": "confirm", "title": args.get("title") or "需要你确认", "detail": args.get("detail") or ""})
        return _text("用户已确认，可以继续。" if v.get("ok") else "用户拒绝了，不要执行这个操作。")
    if tool == "ask_user":
        opts = args.get("options") or []
        v = ui.ask({"kind": "choice" if opts else "text", "title": args.get("question") or "请选择", "options": opts})
        return _text(f"用户的回答：{v.get('value', '')}")
    if tool == "screenshot":
        fg, cursor = session.current_fg_cursor()
        ui.WIN.hide_panel()
        time.sleep(0.16)
        try:
            p = capture.screenshot(fg["hwnd"], cursor, os.path.join(config.config_dir(), "shots"))
        finally:
            if ui.WIN.visible:
                ui.WIN.show_panel_at(cursor) if not ui.WIN.anchor else None
        with open(p, "rb") as f:
            b64 = base64.b64encode(f.read()).decode()
        return [{"type": "text", "text": f"截图已保存：{p}"}, {"type": "image", "data": b64, "mimeType": "image/png"}]
    if tool == "get_context":
        return _text(session.describe_context())
    if tool == "approve":
        return approve(args)
    raise RuntimeError(f"未知工具：{tool}")


# ───────────────────────── 审批 ─────────────────────────

SAFE_BUILTIN = {"Read", "Glob", "Grep", "LS", "WebSearch", "WebFetch", "TodoWrite", "Skill", "NotebookRead", "Task", "Agent", "ToolSearch", "BashOutput", "KillShell"}
READONLY_VERBS = ("get", "list", "search", "read", "find", "query", "fetch", "view", "describe", "show", "status", "check", "lookup",
                  "download", "export", "count", "snapshot", "screenshot", "analyze", "summar")
DANGEROUS_CMD = ("rm -r", "rm -f", "rm -fr", "remove-item", "del /s", "del /q", "rmdir /s", "rd /s", "format ", "shutdown", "stop-computer",
                 "restart-computer", "reg delete", "git push", "git reset --hard", "git clean", "diskpart", "mkfs", "iex", "invoke-expression",
                 "invoke-webrequest", "curl ", "wget ", "sudo ", "taskkill", "stop-process", "set-executionpolicy", "net user", "schtasks", "cipher /w")

ALLOW, ASK = "allow", "ask"


def decide(mode: str, tool: str, inp: dict) -> str:
    if mode == "auto":
        return ALLOW
    if tool.startswith("mcp__saydo__") or tool in SAFE_BUILTIN:
        return ALLOW
    if mode == "careful":
        return ASK
    # balanced
    if tool in ("Edit", "Write", "MultiEdit", "NotebookEdit"):
        return ALLOW
    if tool in ("Bash", "PowerShell"):
        cmd = str(inp.get("command", "")).lower()
        return ASK if any(d in cmd for d in DANGEROUS_CMD) else ALLOW
    if tool.startswith("mcp__"):
        name = tool.rsplit("__", 1)[-1].lower()
        return ALLOW if any(name.startswith(v) or f"_{v}" in name for v in READONLY_VERBS) else ASK
    return ASK


def describe_call(tool: str, inp: dict):
    if tool in ("Bash", "PowerShell"):
        return "运行命令", str(inp.get("command", ""))[:700]
    if tool.startswith("mcp__"):
        rest = tool[5:].split("__", 1)
        server = rest[0].replace("claude_ai_", "").replace("_", " ")
        name = (rest[1] if len(rest) > 1 else "").replace("_", " ")
        return f"{server} · {name}", json.dumps(inp, ensure_ascii=False, indent=2)[:900]
    return tool, json.dumps(inp, ensure_ascii=False, indent=2)[:700]


def approve(args: dict):
    tool = args.get("tool_name", "")
    inp = args.get("input") or {}
    cfg = config.get()
    allowed = decide(cfg["approval"], tool, inp) == ALLOW
    if not allowed:
        with _allow_lock:
            allowed = tool in _allow_always
    if not allowed:
        title, detail = describe_call(tool, inp)
        v = ui.ask({"kind": "confirm", "title": f"允许执行：{title}？", "detail": detail, "allowAlways": True, "danger": True})
        if v.get("always"):
            with _allow_lock:
                _allow_always.add(tool)
        allowed = bool(v.get("ok"))
    verdict = ({"behavior": "allow", "updatedInput": inp} if allowed
               else {"behavior": "deny", "message": "用户在 SayDo 弹窗里拒绝了这个操作。请停止并告诉用户已取消。"})
    return _text(json.dumps(verdict, ensure_ascii=False))
