"""内置 MCP 服务器（stdio）。由 Claude Code / Codex 以 `python mcp_stdio.py` 拉起。
它自己不干活，只把 tools/call 通过本地桥转给正在运行的 SayDo 主进程（那边才有弹窗）。
协议：newline-delimited JSON-RPC 2.0。只用标准库，不依赖 SayDo 包。"""
import json
import os
import sys
import urllib.request

TOOLS = [
    {"name": "pick_folder",
     "description": "在用户屏幕上弹出文件夹选择器，让用户挑一个文件夹，返回路径。需要用户决定把文件移动/复制/保存到哪里、而上下文里没有明确目标时调用，不要自己猜目录。",
     "inputSchema": {"type": "object", "properties": {"prompt": {"type": "string", "description": "弹窗标题，如 \"把 report.docx 移到哪里？\""}}}},
    {"name": "confirm",
     "description": "弹窗让用户确认一个动作（允许/拒绝）。发送邮件、对外分享、覆盖文件、批量或不可逆操作之前调用。",
     "inputSchema": {"type": "object", "properties": {"title": {"type": "string"}, "detail": {"type": "string", "description": "要执行动作的具体内容（收件人、文件列表等）"}}, "required": ["title"]}},
    {"name": "ask_user",
     "description": "弹窗向用户提一个问题：给 options 就是单选，不给就是文本输入。",
     "inputSchema": {"type": "object", "properties": {"question": {"type": "string"}, "options": {"type": "array", "items": {"type": "string"}}}, "required": ["question"]}},
    {"name": "screenshot",
     "description": "截取用户当前正在看的窗口（失败则截整个屏幕），返回图片。需要看用户屏幕/页面上有什么时调用。",
     "inputSchema": {"type": "object", "properties": {}}},
    {"name": "get_context",
     "description": "获取用户按下热键那一刻的上下文：前台程序、窗口标题、选中的文字或文件路径。",
     "inputSchema": {"type": "object", "properties": {}}},
    {"name": "approve",
     "description": "Claude Code 的 --permission-prompt-tool：决定一个需要授权的工具调用是否放行。",
     "inputSchema": {"type": "object", "properties": {"tool_name": {"type": "string"}, "input": {"type": "object"}, "tool_use_id": {"type": "string"}}, "required": ["tool_name", "input"]}},
]


def call_bridge(tool, args):
    p = os.path.join(os.environ.get("APPDATA", os.path.expanduser("~")), "com.user.saydo", "server.json")
    try:
        with open(p, encoding="utf-8") as f:
            info = json.load(f)
    except Exception:
        raise RuntimeError("SayDo 主程序没有在运行")
    req = urllib.request.Request(
        f"http://127.0.0.1:{info['port']}/call", data=json.dumps({"tool": tool, "args": args}).encode("utf-8"),
        headers={"X-Token": info["bridge_token"], "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=900) as r:
            v = json.loads(r.read().decode("utf-8"))
    except Exception as e:
        raise RuntimeError(f"连接 SayDo 主程序失败：{e}")
    if v.get("ok"):
        return v["content"]
    raise RuntimeError(v.get("error") or "未知错误")


def handle(req):
    method = req.get("method", "")
    if method == "initialize":
        return {"protocolVersion": (req.get("params") or {}).get("protocolVersion", "2024-11-05"), "capabilities": {"tools": {}},
                "serverInfo": {"name": "saydo", "version": "0.1.0"}}
    if method == "ping":
        return {}
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        p = req.get("params") or {}
        try:
            return {"content": call_bridge(p.get("name", ""), p.get("arguments") or {})}
        except Exception as e:
            return {"content": [{"type": "text", "text": str(e)}], "isError": True}
    return None


def main():
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        res = handle(req)
        if "id" in req and res is not None:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": res}, ensure_ascii=False) + "\n")
            sys.stdout.flush()
        elif "id" in req and req.get("method", "").split("/")[0] in ("tools", "initialize"):
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32601, "message": "method not found"}}) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
