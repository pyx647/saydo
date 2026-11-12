//! 三个 Code CLI 的无头运行器：Claude Code / Codex / agy（Antigravity）。
//! 都走"提示词从 stdin 喂入 → 逐行读 JSON 事件 → 推进度 → 取最终文本"。

use super::{build_prompt, workspace_dir, RunInput, Sink};
use crate::config::{CliCfg, Config};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tauri::AppHandle;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// 在 PATH 里找可执行文件（.exe/.cmd/.bat），支持用户手填完整路径
pub fn find_exe(name: &str, override_path: &str) -> Option<PathBuf> {
    let o = override_path.trim().trim_matches('"');
    if !o.is_empty() {
        let p = PathBuf::from(o);
        return p.exists().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in ["exe", "cmd", "bat"] {
            let p = dir.join(format!("{name}.{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(0x0800_0000).output();
}

fn split_args(s: &str) -> Vec<String> {
    s.split_whitespace().map(|x| x.to_string()).collect()
}

fn trunc(s: &str, n: usize) -> String {
    let one: String = s.replace(['\r', '\n'], " ");
    let t: String = one.chars().take(n).collect();
    if one.chars().count() > n { format!("{t}…") } else { t }
}

/// 通用流式运行：返回 stderr 尾部（用于报错）。on_line 处理每行 JSON。
async fn run_stream(
    exe: &Path,
    args: Vec<String>,
    stdin_text: Option<String>,
    cwd: &Path,
    sink: &Sink,
    mut on_line: impl FnMut(&Value),
) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(&args)
        .current_dir(cwd)
        .stdin(if stdin_text.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .creation_flags(0x0800_0000);
    let mut child = cmd.spawn().map_err(|e| format!("启动 {} 失败：{e}", exe.display()))?;
    let pid = child.id().unwrap_or(0);
    crate::session::set_run_pid(pid);

    if let Some(t) = stdin_text {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(t.as_bytes()).await;
            let _ = si.shutdown().await;
        }
    }
    let stdout = child.stdout.take().ok_or("无法读取输出")?;
    let stderr = child.stderr.take();
    let err_task = tokio::spawn(async move {
        let mut tail = String::new();
        if let Some(se) = stderr {
            let mut lines = BufReader::new(se).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if tail.len() < 4000 {
                    tail.push_str(&l);
                    tail.push('\n');
                }
            }
        }
        tail
    });

    let mut lines = BufReader::new(stdout).lines();
    loop {
        if sink.cancelled() {
            kill_tree(pid);
            crate::session::set_run_pid(0);
            return Err("已取消".into());
        }
        match tokio::time::timeout(Duration::from_millis(400), lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                    on_line(&v);
                }
            }
            Ok(Ok(None)) | Ok(Err(_)) => break,
            Err(_) => sink.heartbeat(), // 超时只是没新输出，刷新一次耗时显示
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    crate::session::set_run_pid(0);
    Ok(err_task.await.unwrap_or_default())
}

pub async fn run(app: &AppHandle, cfg: &Config, inp: &RunInput, sink: &Sink) -> Result<String, String> {
    match cfg.backend.as_str() {
        "claude" => run_claude(app, cfg, inp, sink).await,
        "codex" => run_codex(app, cfg, inp, sink).await,
        _ => run_agy(app, cfg, inp, sink).await,
    }
}

fn need_exe(name: &str, c: &CliCfg, hint: &str) -> Result<PathBuf, String> {
    find_exe(name, &c.path).ok_or_else(|| format!("找不到 {name} 命令行。{hint}（设置 → 后端 里可手动指定路径）"))
}

// ───────────────────────── Claude Code ─────────────────────────

fn friendly_claude_tool(name: &str, input: &Value) -> String {
    let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
    match name {
        "Bash" | "PowerShell" => {
            let d = s("description");
            let c = s("command");
            format!("运行命令：{}", trunc(if d.is_empty() { &c } else { &d }, 56))
        }
        "Read" => format!("读取：{}", trunc(Path::new(&s("file_path")).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default().as_str(), 40)),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => "写入文件".into(),
        "Glob" | "Grep" => "搜索文件".into(),
        "WebFetch" | "WebSearch" => "联网查询".into(),
        "Skill" => format!("使用技能：{}", s("skill")),
        "Task" | "Agent" => "派出子任务".into(),
        n if n.starts_with("mcp__") => {
            let mut it = n.trim_start_matches("mcp__").splitn(2, "__");
            let server = it.next().unwrap_or("").trim_start_matches("claude_ai_").replace('_', " ");
            let tool = it.next().unwrap_or("").replace('_', " ");
            format!("{server} · {tool}")
        }
        other => other.to_string(),
    }
}

/// Claude 命令行参数（一次性 `-p` 与常驻流式输入共用）
pub fn claude_args(app: &AppHandle, cfg: &Config, dirs: &[PathBuf], oneshot: bool) -> Result<Vec<String>, String> {
    // 把 SayDo 自己注册成 MCP 服务器（提供 pick_folder / confirm / screenshot / approve）
    let mcp_path = crate::config::config_dir(app).join("mcp-saydo.json");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let mcp_json = serde_json::json!({ "mcpServers": { "saydo": { "command": me.to_string_lossy(), "args": ["--mcp"] } } });
    std::fs::write(&mcp_path, mcp_json.to_string()).map_err(|e| e.to_string())?;

    let mut args: Vec<String> = vec![
        "-p".into(), "--output-format".into(), "stream-json".into(), "--verbose".into(), "--include-partial-messages".into(),
        "--mcp-config".into(), mcp_path.to_string_lossy().to_string(),
    ];
    if !oneshot {
        args.extend(["--input-format".into(), "stream-json".into()]);
    }
    if cfg.approval == "auto" {
        args.extend(["--permission-mode".into(), "bypassPermissions".into()]);
    } else {
        args.extend(["--permission-mode".into(), "acceptEdits".into(), "--permission-prompt-tool".into(), "mcp__saydo__approve".into()]);
    }
    if !cfg.keep_hooks {
        // 用户 ~/.claude 里的 SessionStart/Stop hooks 会让每次调用多花十几秒；MCP/Skills 不受影响
        args.extend(["--settings".into(), r#"{"disableAllHooks":true}"#.into()]);
    }
    for d in dirs {
        args.extend(["--add-dir".into(), d.to_string_lossy().to_string()]);
    }
    args.extend(["--model".into(), cfg.claude_model(), "--effort".into(), cfg.claude_effort()]);
    args.extend(split_args(&cfg.claude.extra_args));
    Ok(args)
}

/// Claude stream-json 事件 → 进度 / 文本；(最终文本, 是否报错) 写进 res
pub fn claude_event(v: &Value, sink: &Sink, res: &mut (Option<String>, bool)) {
    match v["type"].as_str() {
        Some("stream_event") if v["parent_tool_use_id"].is_null() => {
            let e = &v["event"];
            match e["type"].as_str() {
                Some("content_block_start") if e["content_block"]["type"] == "text" => sink.clear_text(),
                Some("content_block_delta") if e["delta"]["type"] == "text_delta" => sink.push_delta(e["delta"]["text"].as_str().unwrap_or("")),
                _ => {}
            }
        }
        Some("assistant") if v["parent_tool_use_id"].is_null() => {
            if let Some(blocks) = v["message"]["content"].as_array() {
                for b in blocks {
                    if b["type"] == "tool_use" {
                        sink.step(&friendly_claude_tool(b["name"].as_str().unwrap_or(""), &b["input"]));
                    }
                }
            }
        }
        Some("result") => {
            res.0 = v["result"].as_str().map(|s| s.to_string());
            res.1 = v["is_error"].as_bool().unwrap_or(false);
        }
        _ => {}
    }
}

pub fn kill_process_tree(pid: u32) {
    kill_tree(pid)
}

pub fn claude_trunc(s: &str, n: usize) -> String {
    trunc(s, n)
}

async fn run_claude(app: &AppHandle, cfg: &Config, inp: &RunInput, sink: &Sink) -> Result<String, String> {
    let exe = need_exe("claude", &cfg.claude, "请先安装 Claude Code")?;
    let (prompt, dirs) = build_prompt(app, cfg, inp, true);
    let ws = workspace_dir(app);

    if cfg.warm_claude {
        sink.step("交给 Claude Code…");
        let prompt = format!("【新任务】下面是一个全新的、独立的任务，与此前对话无关；不要引用之前的内容（除非下面的“上一轮对话”里给出）。\n\n{prompt}");
        return super::warm::ask(app, cfg, &prompt, sink);
    }

    let args = claude_args(app, cfg, &dirs, true)?;
    sink.step("启动 Claude Code…");
    let mut res: (Option<String>, bool) = (None, false);
    let tail = run_stream(&exe, args, Some(prompt), &ws, sink, |v| claude_event(v, sink, &mut res)).await?;
    match res {
        (Some(t), false) => Ok(t),
        (Some(t), true) => Err(format!("Claude Code 报错：{}", trunc(&t, 300))),
        (None, _) => Err(format!("Claude Code 没有返回结果。{}", trunc(&tail, 300))),
    }
}

// ───────────────────────── Codex ─────────────────────────

async fn run_codex(app: &AppHandle, cfg: &Config, inp: &RunInput, sink: &Sink) -> Result<String, String> {
    let exe = need_exe("codex", &cfg.codex, "请先安装 Codex CLI（npm i -g @openai/codex）")?;
    let (prompt, _dirs) = build_prompt(app, cfg, inp, true);
    if cfg.warm_claude {
        sink.step("交给 Codex…");
        let prompt = format!("【新任务】下面是一个全新的、独立的任务，与此前对话无关；不要引用之前的内容（除非下面的“上一轮对话”里给出）。\n\n{prompt}");
        return super::codex_warm::ask(app, cfg, &prompt, sink);
    }
    let ws = workspace_dir(app);
    let me = std::env::current_exe().map_err(|e| e.to_string())?;

    let mut args: Vec<String> = vec!["exec".into(), "--json".into(), "--skip-git-repo-check".into()];
    if let Some(shot) = &inp.shot {
        args.extend(["-i".into(), shot.to_string_lossy().to_string()]);
    }
    args.extend(["--sandbox".into(), if cfg.full_access { "danger-full-access".into() } else { "workspace-write".into() }]);
    args.extend(["-c".into(), "approval_policy=\"never\"".into()]);
    // TOML 字面量字符串（单引号）：Windows 路径里的反斜杠不用转义
    args.extend(["-c".into(), format!("mcp_servers.saydo.command='{}'", me.to_string_lossy())]);
    args.extend(["-c".into(), "mcp_servers.saydo.args=['--mcp']".into()]);
    args.extend(["-c".into(), "mcp_servers.saydo.tool_timeout_sec=600".into()]);
    args.extend(["-C".into(), ws.to_string_lossy().to_string()]);
    args.extend(["-m".into(), cfg.codex_model()]);
    args.extend(["-c".into(), format!("model_reasoning_effort=\"{}\"", cfg.codex_effort())]);
    args.extend(split_args(&cfg.codex.extra_args));
    args.push("-".into());

    sink.step("启动 Codex…");
    let mut final_text: Option<String> = None;
    let mut failure: Option<String> = None;
    let tail = run_stream(&exe, args, Some(prompt), &ws, sink, |v| match v["type"].as_str() {
        Some("item.started") => {
            let it = &v["item"];
            match it["type"].as_str() {
                Some("command_execution") => sink.step(&format!("运行命令：{}", trunc(it["command"].as_str().unwrap_or(""), 56))),
                Some("mcp_tool_call") => sink.step(&format!("{} · {}", it["server"].as_str().unwrap_or("mcp"), it["tool"].as_str().unwrap_or("").replace('_', " "))),
                Some("web_search") => sink.step("联网查询"),
                Some("file_change") => sink.step("修改文件"),
                _ => {}
            }
        }
        Some("item.completed") => {
            let it = &v["item"];
            if it["type"] == "agent_message" {
                if let Some(t) = it["text"].as_str() {
                    final_text = Some(t.to_string());
                    sink.set_text(t);
                }
            }
        }
        Some("turn.failed") => failure = Some(v["error"]["message"].as_str().unwrap_or("turn failed").to_string()),
        Some("error") => failure = Some(v["message"].as_str().unwrap_or("error").to_string()),
        _ => {}
    })
    .await?;

    match (final_text, failure) {
        (Some(t), _) => Ok(t),
        (None, Some(f)) => Err(format!("Codex 报错：{}", trunc(&f, 300))),
        (None, None) => Err(format!("Codex 没有返回结果。{}", trunc(&tail, 200))),
    }
}

// ───────────────────────── agy (Antigravity) ─────────────────────────

async fn run_agy(app: &AppHandle, cfg: &Config, inp: &RunInput, sink: &Sink) -> Result<String, String> {
    let exe = need_exe("agy", &cfg.agy, "请先安装 Antigravity CLI")?;
    let (prompt, dirs) = build_prompt(app, cfg, inp, false);
    let ws = workspace_dir(app);

    let mut args: Vec<String> = vec!["--output-format".into(), "stream-json".into(), "--dangerously-skip-permissions".into()];
    for d in &dirs {
        args.extend(["--add-dir".into(), d.to_string_lossy().to_string()]);
    }
    if !cfg.agy.model.trim().is_empty() {
        args.extend(["--model".into(), cfg.agy.model.trim().to_string()]);
    }
    args.extend(split_args(&cfg.agy.extra_args));
    args.extend(["-p".into(), prompt]);

    sink.step("启动 Antigravity…");
    let mut final_text: Option<String> = None;
    let mut failure: Option<String> = None;
    let mut cur_step: i64 = -1;
    let tail = run_stream(&exe, args, None, &ws, sink, |v| match v["event"].as_str() {
        Some("step_update") => {
            let su = &v["step_update"];
            let idx = su["step_index"].as_i64().unwrap_or(-1);
            match su["step_type"].as_str() {
                Some("agent_response") => {
                    if idx != cur_step {
                        cur_step = idx;
                        sink.clear_text();
                    }
                    if let Some(d) = su["text_delta"].as_str() {
                        sink.push_delta(d);
                    }
                }
                Some("user_input") | None => {}
                Some(other) if su["state"] == "ACTIVE" => sink.step(&other.replace('_', " ")),
                _ => {}
            }
        }
        Some("result") => {
            let r = &v["result"];
            if r["status"] == "SUCCESS" {
                final_text = r["response"].as_str().map(|s| s.trim().to_string());
            } else {
                failure = Some(format!("{}", r["status"].as_str().unwrap_or("失败")));
            }
        }
        _ => {}
    })
    .await?;

    match (final_text, failure) {
        (Some(t), _) => Ok(t),
        (None, Some(f)) => Err(format!("agy 报错：{f}")),
        (None, None) => Err(format!("agy 没有返回结果。{}", trunc(&tail, 200))),
    }
}
