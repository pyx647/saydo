//! SayDo 暴露给 CLI agent 的工具（通过内置 MCP 服务器）：
//!   pick_folder / confirm / ask_user / screenshot / get_context / approve
//! 其中 `approve` 是 Claude Code 的 --permission-prompt-tool：每次它要执行需要授权的动作，
//! 都先问 SayDo，由我们按"审批强度"自动放行、拒绝，或弹窗让用户点。

use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Manager};

fn text(s: impl Into<String>) -> Value {
    json!([{ "type": "text", "text": s.into() }])
}

static ALLOW_ALWAYS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
fn allow_set() -> &'static Mutex<HashSet<String>> {
    ALLOW_ALWAYS.get_or_init(|| Mutex::new(HashSet::new()))
}
pub fn reset_allow() {
    allow_set().lock().unwrap().clear();
}

/// 返回 MCP content 数组
pub fn dispatch(app: &AppHandle, tool: &str, args: &Value) -> Result<Value, String> {
    match tool {
        "pick_folder" => {
            let title = args["prompt"].as_str().unwrap_or("选择一个文件夹");
            let v = crate::ui::ask(app, json!({ "kind": "folder", "title": title }))?;
            match v["path"].as_str() {
                Some(p) if !p.is_empty() => {
                    crate::config::remember_folder(app, p);
                    Ok(text(format!("用户选择的文件夹：{p}")))
                }
                _ => Ok(text("用户取消了选择，请停止当前操作并告诉用户已取消。")),
            }
        }
        "confirm" => {
            let v = crate::ui::ask(
                app,
                json!({
                    "kind": "confirm",
                    "title": args["title"].as_str().unwrap_or("需要你确认"),
                    "detail": args["detail"].as_str().unwrap_or(""),
                }),
            )?;
            Ok(text(if v["ok"].as_bool().unwrap_or(false) { "用户已确认，可以继续。" } else { "用户拒绝了，不要执行这个操作。" }))
        }
        "ask_user" => {
            let opts = args["options"].clone();
            let kind = if opts.as_array().map(|a| !a.is_empty()).unwrap_or(false) { "choice" } else { "text" };
            let v = crate::ui::ask(app, json!({ "kind": kind, "title": args["question"].as_str().unwrap_or("请选择"), "options": opts }))?;
            Ok(text(format!("用户的回答：{}", v["value"].as_str().unwrap_or(""))))
        }
        "screenshot" => {
            let (fg, cursor) = crate::session::current_fg_cursor();
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.hide();
            }
            std::thread::sleep(std::time::Duration::from_millis(160));
            let r = crate::capture::screenshot(fg.hwnd, cursor, &crate::config::config_dir(app).join("shots"));
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.show();
            }
            let p = r?;
            let b64 = {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(std::fs::read(&p).map_err(|e| e.to_string())?)
            };
            Ok(json!([
                { "type": "text", "text": format!("截图已保存：{}", p.display()) },
                { "type": "image", "data": b64, "mimeType": "image/png" }
            ]))
        }
        "get_context" => Ok(text(crate::session::describe_context())),
        "approve" => approve(app, args),
        other => Err(format!("未知工具：{other}")),
    }
}

// ───────────────────────── 审批 ─────────────────────────

const SAFE_BUILTIN: &[&str] = &["Read", "Glob", "Grep", "LS", "WebSearch", "WebFetch", "TodoWrite", "Skill", "NotebookRead", "Task", "Agent", "ToolSearch", "BashOutput", "KillShell"];
const READONLY_VERBS: &[&str] = &[
    "get", "list", "search", "read", "find", "query", "fetch", "view", "describe", "show", "status", "check", "lookup", "download", "export", "count", "snapshot", "screenshot", "analyze", "summar",
];
const DANGEROUS_CMD: &[&str] = &[
    "rm -r", "rm -f", "rm -fr", "remove-item", "del /s", "del /q", "rmdir /s", "rd /s", "format ", "shutdown", "stop-computer", "restart-computer", "reg delete", "git push", "git reset --hard",
    "git clean", "diskpart", "mkfs", "iex", "invoke-expression", "invoke-webrequest", "curl ", "wget ", "sudo ", "taskkill", "stop-process", "set-executionpolicy", "net user", "schtasks", "cipher /w",
];

enum Decision {
    Allow,
    Ask,
}

fn decide(mode: &str, tool: &str, input: &Value) -> Decision {
    if mode == "auto" {
        return Decision::Allow;
    }
    if tool.starts_with("mcp__saydo__") || SAFE_BUILTIN.contains(&tool) {
        return Decision::Allow;
    }
    if mode == "careful" {
        return Decision::Ask;
    }
    // balanced
    match tool {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => Decision::Allow,
        "Bash" | "PowerShell" => {
            let cmd = input["command"].as_str().unwrap_or("").to_lowercase();
            if DANGEROUS_CMD.iter().any(|d| cmd.contains(d)) { Decision::Ask } else { Decision::Allow }
        }
        n if n.starts_with("mcp__") => {
            let t = n.rsplit("__").next().unwrap_or("").to_lowercase();
            if READONLY_VERBS.iter().any(|v| t.starts_with(v) || t.contains(&format!("_{v}"))) { Decision::Allow } else { Decision::Ask }
        }
        _ => Decision::Ask,
    }
}

fn describe_call(tool: &str, input: &Value) -> (String, String) {
    match tool {
        "Bash" | "PowerShell" => ("运行命令".into(), input["command"].as_str().unwrap_or("").chars().take(700).collect()),
        n if n.starts_with("mcp__") => {
            let mut it = n.trim_start_matches("mcp__").splitn(2, "__");
            let server = it.next().unwrap_or("").trim_start_matches("claude_ai_").replace('_', " ");
            let tool = it.next().unwrap_or("").replace('_', " ");
            (format!("{server} · {tool}"), serde_json::to_string_pretty(input).unwrap_or_default().chars().take(900).collect())
        }
        other => (other.to_string(), serde_json::to_string_pretty(input).unwrap_or_default().chars().take(700).collect()),
    }
}

fn approve(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let tool = args["tool_name"].as_str().unwrap_or("");
    let input = args["input"].clone();
    let cfg = crate::config::get();

    let allowed = match decide(&cfg.approval, tool, &input) {
        Decision::Allow => true,
        Decision::Ask => {
            if allow_set().lock().unwrap().contains(tool) {
                true
            } else {
                let (title, detail) = describe_call(tool, &input);
                let v = crate::ui::ask(
                    app,
                    json!({ "kind": "confirm", "title": format!("允许执行：{title}？"), "detail": detail, "allowAlways": true, "danger": true }),
                )?;
                if v["always"].as_bool().unwrap_or(false) {
                    allow_set().lock().unwrap().insert(tool.to_string());
                }
                v["ok"].as_bool().unwrap_or(false)
            }
        }
    };
    let verdict = if allowed {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": "用户在 SayDo 弹窗里拒绝了这个操作。请停止并告诉用户已取消。" })
    };
    Ok(text(verdict.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn balanced_rules() {
        assert!(matches!(decide("balanced", "Bash", &json!({"command":"ffmpeg -i a.mp4 b.gif"})), Decision::Allow));
        assert!(matches!(decide("balanced", "Bash", &json!({"command":"Remove-Item -Recurse C:\\x"})), Decision::Ask));
        assert!(matches!(decide("balanced", "mcp__claude_ai_Gmail__search_threads", &json!({})), Decision::Allow));
        assert!(matches!(decide("balanced", "mcp__claude_ai_Gmail__send_message", &json!({})), Decision::Ask));
        assert!(matches!(decide("balanced", "mcp__x__create_draft", &json!({})), Decision::Ask));
        assert!(matches!(decide("careful", "Edit", &json!({})), Decision::Ask));
        assert!(matches!(decide("auto", "mcp__x__send_message", &json!({})), Decision::Allow));
    }
}
