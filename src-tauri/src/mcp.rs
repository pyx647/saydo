//! 内置 MCP 服务器（stdio）。由 Claude Code / Codex 以 `saydo.exe --mcp` 拉起。
//! 它自己不干活，只把 tools/call 通过本地桥转给正在运行的 SayDo 主进程（那边才有弹窗）。
//! 协议：newline-delimited JSON-RPC 2.0。

use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn tool_defs() -> Value {
    json!([
      {
        "name": "pick_folder",
        "description": "在用户屏幕上弹出文件夹选择器，让用户挑一个文件夹，返回路径。需要用户决定把文件移动/复制/保存到哪里、而上下文里没有明确目标时调用，不要自己猜目录。",
        "inputSchema": { "type": "object", "properties": { "prompt": { "type": "string", "description": "弹窗标题，如 \"把 report.docx 移到哪里？\"" } } }
      },
      {
        "name": "confirm",
        "description": "弹窗让用户确认一个动作（允许/拒绝）。发送邮件、对外分享、覆盖文件、批量或不可逆操作之前调用。",
        "inputSchema": { "type": "object", "properties": { "title": { "type": "string" }, "detail": { "type": "string", "description": "要执行动作的具体内容（收件人、文件列表等）" } }, "required": ["title"] }
      },
      {
        "name": "ask_user",
        "description": "弹窗向用户提一个问题：给 options 就是单选，不给就是文本输入。",
        "inputSchema": { "type": "object", "properties": { "question": { "type": "string" }, "options": { "type": "array", "items": { "type": "string" } } }, "required": ["question"] }
      },
      {
        "name": "screenshot",
        "description": "截取用户当前正在看的窗口（失败则截整个屏幕），返回图片。需要看用户屏幕/页面上有什么时调用。",
        "inputSchema": { "type": "object", "properties": {} }
      },
      {
        "name": "get_context",
        "description": "获取用户按下热键那一刻的上下文：前台程序、窗口标题、选中的文字或文件路径。",
        "inputSchema": { "type": "object", "properties": {} }
      },
      {
        "name": "approve",
        "description": "Claude Code 的 --permission-prompt-tool：决定一个需要授权的工具调用是否放行。",
        "inputSchema": {
          "type": "object",
          "properties": { "tool_name": { "type": "string" }, "input": { "type": "object" }, "tool_use_id": { "type": "string" } },
          "required": ["tool_name", "input"]
        }
      }
    ])
}

fn call_bridge(tool: &str, args: &Value) -> Result<Value, String> {
    let info_path = crate::config::config_dir_static().join("server.json");
    let info: Value = serde_json::from_str(&std::fs::read_to_string(&info_path).map_err(|_| "SayDo 主程序没有在运行".to_string())?)
        .map_err(|e| e.to_string())?;
    let port = info["port"].as_u64().ok_or("server.json 无效")?;
    let token = info["token"].as_str().ok_or("server.json 无效")?;
    let resp = ureq::post(&format!("http://127.0.0.1:{port}/call"))
        .set("X-Token", token)
        .timeout(std::time::Duration::from_secs(900))
        .send_json(json!({ "tool": tool, "args": args }))
        .map_err(|e| format!("连接 SayDo 主程序失败：{e}"))?;
    let v: Value = resp.into_json().map_err(|e| e.to_string())?;
    if v["ok"].as_bool().unwrap_or(false) {
        Ok(v["content"].clone())
    } else {
        Err(v["error"].as_str().unwrap_or("未知错误").to_string())
    }
}

pub fn run_stdio() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(line) else { continue };
        let id = req.get("id").cloned();
        let method = req["method"].as_str().unwrap_or("");
        let reply = match method {
            "initialize" => Some(json!({
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "saydo", "version": env!("CARGO_PKG_VERSION") }
            })),
            "ping" => Some(json!({})),
            "tools/list" => Some(json!({ "tools": tool_defs() })),
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let args = &req["params"]["arguments"];
                Some(match call_bridge(name, args) {
                    Ok(content) => json!({ "content": content }),
                    Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
                })
            }
            _ => None, // notifications/* 等：不回复
        };
        if let (Some(id), Some(result)) = (id, reply) {
            let msg = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            let _ = writeln!(out, "{msg}");
            let _ = out.flush();
        } else if method.starts_with("tools/") || method == "initialize" {
            if let Some(id) = req.get("id") {
                let msg = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "method not found" } });
                let _ = writeln!(out, "{msg}");
                let _ = out.flush();
            }
        }
    }
}
