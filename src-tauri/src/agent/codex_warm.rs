//! 常驻的 Codex（`codex app-server`，JSON-RPC over stdio）。
//! 实测（本机，含 12 个 MCP）：一次性 `codex exec` 约 10~12 秒；常驻后每条约 2.5~4.5 秒。
//! 做法：进程常驻 + 握手一次；线程（对话）复用，每 MAX_TURNS 轮换一个新线程，避免上下文累积。
//! 每个任务的提示词里声明"全新独立任务"。

use super::cli::{claude_trunc, find_exe, kill_process_tree};
use super::{workspace_dir, Sink};
use crate::config::Config;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::AppHandle;

const MAX_TURNS: u32 = 10;

/// 新进程 / 新线程是否已发过"预热消息"（首次开线程要连 MCP，第一个任务会多等好几秒）
static PRIMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct Proc {
    child: Child,
    stdin: ChildStdin,
    sig: String,
    /// 当前线程 id 与已用轮数
    thread: Option<String>,
    turns: u32,
}

struct Warm {
    proc: Mutex<Option<Proc>>,
    /// 当前进程的事件接收端（读线程持有发送端）。只在持有 `req` 时使用
    rx: Mutex<Option<Receiver<Value>>>,
    req: Mutex<()>,
    next_id: AtomicU64,
}

static WARM: OnceLock<Warm> = OnceLock::new();
fn warm() -> &'static Warm {
    WARM.get_or_init(|| Warm { proc: Mutex::new(None), rx: Mutex::new(None), req: Mutex::new(()), next_id: AtomicU64::new(1) })
}

fn sig(cfg: &Config) -> String {
    serde_json::to_string(&json!([cfg.full_access, cfg.codex])).unwrap_or_default()
}

fn kill_locked(p: &mut Option<Proc>) {
    if let Some(mut pr) = p.take() {
        kill_process_tree(pr.child.id());
        let _ = pr.child.kill();
        let _ = pr.child.wait();
    }
}

pub fn kill() {
    if let Some(w) = WARM.get() {
        kill_locked(&mut w.proc.lock().unwrap());
    }
}

fn write_line(w: &Warm, v: Value) -> Result<(), String> {
    let mut g = w.proc.lock().unwrap();
    let p = g.as_mut().ok_or("Codex 进程已退出，请再试一次")?;
    writeln!(p.stdin, "{v}").and_then(|_| p.stdin.flush()).map_err(|_| "Codex 进程已退出，请再试一次".to_string())
}

/// 发一个 JSON-RPC 请求并等它的响应（期间收到的其它通知丢弃——只用于握手 / 开线程）
fn call(w: &Warm, rx: &Receiver<Value>, sink: Option<&Sink>, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
    let id = w.next_id.fetch_add(1, Ordering::SeqCst);
    write_line(w, json!({ "id": id, "method": method, "params": params }))?;
    let end = Instant::now() + timeout;
    loop {
        if Instant::now() >= end {
            return Err(format!("Codex 响应超时（{method}）"));
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(v) => {
                if v["__eof"] == true {
                    return Err("Codex 进程意外退出，请检查登录状态后再试".into());
                }
                if v["id"] == id && v.get("method").is_none() {
                    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
                        return Err(e["message"].as_str().unwrap_or("Codex 请求失败").to_string());
                    }
                    return Ok(v["result"].clone());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if sink.map(|s| s.cancel.load(Ordering::SeqCst)).unwrap_or(false) {
                    return Err("已取消".into());
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Err("Codex 进程意外退出".into()),
        }
    }
}

/// 保证有可用的常驻进程（含握手）；没找到 codex 返回 false
fn ensure(app: &AppHandle, cfg: &Config) -> Result<bool, String> {
    let Some(exe) = find_exe("codex", &cfg.codex.path) else { return Ok(false) };
    let w = warm();
    let s = sig(cfg);
    {
        let mut g = w.proc.lock().unwrap();
        if let Some(p) = g.as_mut() {
            if matches!(p.child.try_wait(), Ok(None)) && p.sig == s {
                return Ok(true);
            }
        }
        kill_locked(&mut g);
        PRIMED.store(false, Ordering::SeqCst);

        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut args: Vec<String> = vec!["app-server".into()];
        // TOML 字面量字符串（单引号）：Windows 路径里的反斜杠不用转义
        args.extend(["-c".into(), format!("mcp_servers.saydo.command='{}'", me.to_string_lossy())]);
        args.extend(["-c".into(), "mcp_servers.saydo.args=['--mcp']".into()]);
        // Codex 对 MCP 工具调用有超时（实测日志：timed out awaiting tools/call）；confirm / pick_folder 要等人，放宽到 10 分钟
        args.extend(["-c".into(), "mcp_servers.saydo.tool_timeout_sec=600".into()]);
        args.extend(cfg.codex.extra_args.split_whitespace().map(|x| x.to_string()));
        let mut cmd = Command::new(&exe);
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd
            .args(&args)
            .current_dir(workspace_dir(app))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 Codex 失败：{e}"))?;
        let stdin = child.stdin.take().ok_or("无法写入 Codex 的标准输入")?;
        let stdout = child.stdout.take().ok_or("无法读取 Codex 的输出")?;
        let stderr = child.stderr.take();
        let (tx, rx): (Sender<Value>, Receiver<Value>) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        return;
                    }
                }
            }
            let _ = tx.send(json!({ "__eof": true }));
        });
        if let Some(e) = stderr {
            std::thread::spawn(move || for _ in BufReader::new(e).lines() {});
        }
        *w.rx.lock().unwrap() = Some(rx);
        *g = Some(Proc { child, stdin, sig: s, thread: None, turns: 0 });
    }
    // 握手
    let rx_g = w.rx.lock().unwrap();
    let rx = rx_g.as_ref().ok_or("Codex 事件通道未就绪")?;
    let r = call(w, rx, None, "initialize", json!({ "clientInfo": { "name": "saydo", "version": env!("CARGO_PKG_VERSION") }, "capabilities": { "experimentalApi": true, "mcpServerOpenaiFormElicitation": true, "extensions": { "openai/form": {} } } }), Duration::from_secs(30));
    if let Err(e) = r {
        drop(rx_g);
        kill_locked(&mut w.proc.lock().unwrap());
        return Err(e);
    }
    write_line(w, json!({ "method": "initialized" }))?;
    Ok(true)
}

/// 应用启动 / 配置变更后调用：提前把进程拉起来、握手，并发一条预热消息把线程和 MCP 都连好
pub fn prewarm(app: &AppHandle, cfg: &Config) {
    {
        let w = warm();
        let _req = w.req.lock().unwrap();
        if !matches!(ensure(app, cfg), Ok(true)) {
            return;
        }
    }
    if !PRIMED.swap(true, Ordering::SeqCst) {
        let sink = Sink::silent(app.clone());
        let _ = ask(app, cfg, "【预热】只回复：好", &sink);
    }
}

/// 弹确认卡；用户点"允许"= Some(true)，"拒绝" = Some(false)，被取消（Esc / ×）= None
fn confirm_card(app: &AppHandle, title: String, detail: String, danger: bool) -> Option<bool> {
    crate::ui::ask(app, json!({ "kind": "confirm", "title": title, "detail": detail, "danger": danger }))
        .ok()
        .map(|v| v["ok"].as_bool().unwrap_or(false))
}

/// 回答 Codex 发来的服务器请求（协议见 `codex app-server generate-json-schema`）。
/// 返回 {"result": …} 或 {"error": …}
fn answer_request(app: &AppHandle, method: &str, p: &Value) -> Value {
    let reason = p["reason"].as_str().unwrap_or("").to_string();
    match method {
        "item/commandExecution/requestApproval" => {
            let cmd = p["command"].as_str().unwrap_or("");
            let detail = format!("{}{}", claude_trunc(cmd, 400), if reason.is_empty() { String::new() } else { format!("\n{reason}") });
            let d = match confirm_card(app, "允许 Codex 运行这条命令？".into(), detail, true) {
                Some(true) => "accept",
                Some(false) => "decline",
                None => "cancel",
            };
            json!({ "result": { "decision": d } })
        }
        "item/fileChange/requestApproval" => {
            let detail = if reason.is_empty() { p["grantRoot"].as_str().unwrap_or("").to_string() } else { reason };
            let d = match confirm_card(app, "允许 Codex 修改文件？".into(), detail, true) {
                Some(true) => "accept",
                Some(false) => "decline",
                None => "cancel",
            };
            json!({ "result": { "decision": d } })
        }
        "item/permissions/requestApproval" => {
            let ok = confirm_card(app, "允许 Codex 获得额外权限？".into(), reason, true).unwrap_or(false);
            if ok {
                json!({ "result": { "permissions": p["permissions"].clone(), "scope": "turn" } })
            } else {
                json!({ "result": { "permissions": {} } })
            }
        }
        "mcpServer/elicitation/request" => {
            let msg = p["message"].as_str().unwrap_or("需要你确认").to_string();
            if p["mode"] == "url" {
                // 要用户去某个网址授权：确认后用浏览器打开，再告诉 Codex 已处理
                let url = p["url"].as_str().unwrap_or("").to_string();
                let ok = confirm_card(app, "需要在浏览器里授权".into(), format!("{msg}\n{url}"), false).unwrap_or(false);
                if ok && !url.is_empty() {
                    let _ = crate::ui::open_path(url);
                }
                return json!({ "result": { "action": if ok { "accept" } else { "decline" }, "content": null } });
            }
            // 表单：没有必填项 = 纯"允许 / 不允许"的确认（连接器执行有副作用的操作前常见）；有必填项我们渲染不了，只能拒绝
            let has_required = p.pointer("/requestedSchema/required").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
            if has_required {
                return json!({ "result": { "action": "decline", "content": null } });
            }
            match confirm_card(app, "需要你确认".into(), msg, false) {
                Some(true) => json!({ "result": { "action": "accept", "content": {} } }),
                Some(false) => json!({ "result": { "action": "decline", "content": null } }),
                None => json!({ "result": { "action": "cancel", "content": null } }),
            }
        }
        "item/tool/requestUserInput" => {
            let mut answers = serde_json::Map::new();
            for q in p["questions"].as_array().cloned().unwrap_or_default() {
                let title = q["question"].as_str().unwrap_or("请选择").to_string();
                let opts: Vec<String> = q["options"].as_array().map(|a| a.iter().filter_map(|o| o["label"].as_str().map(String::from)).collect()).unwrap_or_default();
                let kind = if opts.is_empty() { "text" } else { "choice" };
                let v = crate::ui::ask(app, json!({ "kind": kind, "title": title, "options": opts }));
                let val = v.ok().and_then(|x| x["value"].as_str().map(String::from)).unwrap_or_default();
                if let Some(id) = q["id"].as_str() {
                    answers.insert(id.to_string(), json!({ "answers": [val] }));
                }
            }
            json!({ "result": { "answers": answers } })
        }
        _ => json!({ "error": { "code": -32601, "message": "SayDo 不支持此请求" } }),
    }
}

fn friendly_item(it: &Value) -> Option<String> {
    let ty = it["type"].as_str().unwrap_or("");
    match ty {
        "commandExecution" | "command_execution" => Some(format!("运行命令：{}", claude_trunc(it["command"].as_str().unwrap_or(""), 56))),
        "mcpToolCall" | "mcp_tool_call" => Some(format!("{} · {}", it["server"].as_str().unwrap_or("mcp"), it["tool"].as_str().unwrap_or("").replace('_', " "))),
        "webSearch" | "web_search" => Some("联网查询".into()),
        "fileChange" | "file_change" => Some("修改文件".into()),
        _ => None,
    }
}

/// 把一条提示词交给常驻 Codex，阻塞到结果出来（或被取消）
pub fn ask(app: &AppHandle, cfg: &Config, prompt: &str, sink: &Sink) -> Result<String, String> {
    let w = warm();
    let _req = w.req.lock().unwrap();
    if !ensure(app, cfg)? {
        return Err("找不到 codex 命令行。请先安装 Codex CLI（设置 → 后端 里可手动指定路径）".into());
    }
    let rx_g = w.rx.lock().unwrap();
    let rx = rx_g.as_ref().ok_or("Codex 事件通道未就绪")?;

    // 取（或新开）线程
    let existing = {
        let mut g = w.proc.lock().unwrap();
        let p = g.as_mut().ok_or("Codex 进程已退出，请再试一次")?;
        if p.turns >= MAX_TURNS {
            p.thread = None;
            p.turns = 0;
        }
        p.thread.clone()
    };
    let tid = match existing {
        Some(t) => t,
        None => {
            let mut params = json!({
                "cwd": workspace_dir(app),
                "approvalPolicy": "never",
                "sandbox": if cfg.full_access { "danger-full-access" } else { "workspace-write" },
                "ephemeral": true,
            });
            params["model"] = json!(cfg.codex_model());
            let r = call(w, rx, Some(sink), "thread/start", params, Duration::from_secs(60))?;
            let id = r.pointer("/thread/id").and_then(Value::as_str).ok_or("Codex 未返回会话标识")?.to_string();
            if let Some(p) = w.proc.lock().unwrap().as_mut() {
                p.thread = Some(id.clone());
            }
            id
        }
    };

    // 思考强度：设置里留空 = medium；要更快选 low，要更强选 high / xhigh
    let effort = cfg.codex_effort();
    let turn_req = w.next_id.fetch_add(1, Ordering::SeqCst);
    write_line(
        w,
        json!({ "id": turn_req, "method": "turn/start", "params": { "threadId": tid, "effort": effort, "input": [{ "type": "text", "text": prompt, "text_elements": [] }] } }),
    )?;

    let mut text = String::new();
    let mut final_text: Option<String> = None;
    let mut failure: Option<String> = None;
    let outcome: Result<(), String> = loop {
        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(v) => {
                if v["__eof"] == true {
                    break Err("Codex 进程意外退出，已重启，请再试一次".into());
                }
                // 对 turn/start 的响应：出错就直接报
                if v["id"] == turn_req && v.get("method").is_none() {
                    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
                        break Err(e["message"].as_str().unwrap_or("Codex 请求失败").to_string());
                    }
                    continue;
                }
                let Some(m) = v["method"].as_str() else { continue };
                // Codex 主动向我们要授权 / 要用户输入（比如 Google Drive 连接器上传文件前要确认）：
                // 弹 SayDo 的确认卡问用户，再把答案回给 Codex。之前这里一律拒绝，用户根本看不到弹窗，任务就卡在"请确认"上。
                if v.get("id").is_some() {
                    crate::log::log(&format!("codex asks: {m} mode={} msg={}", v["params"]["mode"].as_str().unwrap_or("-"), claude_trunc(v["params"]["message"].as_str().unwrap_or(""), 120)));
                    sink.step("等你确认…");
                    let reply = answer_request(app, m, &v["params"]);
                    crate::ui::interactive(app, false); // 回到"执行中"的不抢焦点状态
                    let mut msg = json!({ "id": v["id"] });
                    if let Some(r) = reply.get("result") {
                        msg["result"] = r.clone();
                    } else {
                        msg["error"] = reply["error"].clone();
                    }
                    let _ = write_line(w, msg);
                    continue;
                }
                match m {
                    "item/agentMessage/delta" => {
                        if let Some(d) = v.pointer("/params/delta").and_then(Value::as_str) {
                            text.push_str(d);
                            sink.push_delta(d);
                        }
                    }
                    "item/started" => {
                        if let Some(s) = friendly_item(&v["params"]["item"]) {
                            sink.step(&s);
                        }
                    }
                    "item/completed" => {
                        let it = &v["params"]["item"];
                        if it["type"] == "agentMessage" {
                            if let Some(t) = it["text"].as_str() {
                                final_text = Some(t.to_string());
                                sink.set_text(t);
                            }
                        }
                    }
                    "error" => {
                        failure = v.pointer("/params/error/message").or_else(|| v.pointer("/params/message")).and_then(Value::as_str).map(|s| s.to_string());
                    }
                    "turn/completed" => {
                        if v.pointer("/params/turn/status").and_then(Value::as_str) != Some("completed") {
                            failure = Some(v.pointer("/params/turn/error/message").and_then(Value::as_str).unwrap_or("Codex 未完成回复").to_string());
                        }
                        break Ok(());
                    }
                    _ => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if sink.cancel.load(Ordering::SeqCst) {
                    drop_and_kill(w);
                    break Err("已取消".into());
                }
                sink.heartbeat();
            }
            Err(RecvTimeoutError::Disconnected) => break Err("Codex 进程意外退出，已重启，请再试一次".into()),
        }
    };
    if let Err(e) = outcome {
        if e != "已取消" {
            drop_and_kill(w);
        }
        return Err(e);
    }
    if let Some(p) = w.proc.lock().unwrap().as_mut() {
        p.turns += 1;
    }
    match (final_text, failure) {
        (Some(t), None) => Ok(t.trim().to_string()),
        (None, None) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        (_, Some(f)) => Err(format!("Codex 报错：{}", claude_trunc(&f, 300))),
        _ => Err("Codex 没有返回结果".into()),
    }
}

fn drop_and_kill(w: &Warm) {
    kill_locked(&mut w.proc.lock().unwrap());
}
