//! 常驻的 Claude Code 进程（--input-format stream-json）。
//! `claude -p` 每次冷启动要 15~18 秒（是 Claude Code 自身启动，和 MCP 无关），常驻后每条消息 4~6 秒。
//! 代价：同一进程里对话上下文会累积 → 每个任务的提示词里声明"全新独立任务"，并且每 MAX_TURNS 轮换一个新进程。

use super::cli::{claude_args, claude_event, claude_trunc, find_exe, kill_process_tree};
use super::{workspace_dir, Sink};
use crate::config::Config;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::AppHandle;

const MAX_TURNS: u32 = 10;

/// 新进程是否已发过"预热消息"。流式输入模式下 Claude 要等第一条消息才连 MCP / 加载 Skills，
/// 不先发一条的话，用户的第一个任务要多等 3~4 秒。
static PRIMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 读线程把事件送到当前请求；None = 没人在等
type Slot = Arc<Mutex<Option<Sender<Value>>>>;

struct Proc {
    child: Child,
    stdin: ChildStdin,
    sig: String,
    turns: u32,
}

struct Warm {
    proc: Mutex<Option<Proc>>,
    slot: Slot,
    /// 同一时刻只处理一个请求
    req: Mutex<()>,
}

static WARM: OnceLock<Warm> = OnceLock::new();
fn warm() -> &'static Warm {
    WARM.get_or_init(|| Warm { proc: Mutex::new(None), slot: Arc::new(Mutex::new(None)), req: Mutex::new(()) })
}

fn sig(cfg: &Config) -> String {
    serde_json::to_string(&json!([cfg.approval, cfg.keep_hooks, cfg.claude])).unwrap_or_default()
}

fn kill_locked(p: &mut Option<Proc>) {
    if let Some(mut pr) = p.take() {
        kill_process_tree(pr.child.id());
        let _ = pr.child.kill();
        let _ = pr.child.wait();
    }
}

/// 保证有一个可用的常驻进程；没找到 claude 返回 false
fn ensure(app: &AppHandle, cfg: &Config) -> Result<bool, String> {
    let Some(exe) = find_exe("claude", &cfg.claude.path) else { return Ok(false) };
    let w = warm();
    let mut guard = w.proc.lock().unwrap();
    let s = sig(cfg);
    if let Some(p) = guard.as_mut() {
        let alive = matches!(p.child.try_wait(), Ok(None));
        if alive && p.sig == s && p.turns < MAX_TURNS {
            return Ok(true);
        }
    }
    kill_locked(&mut guard);
    PRIMED.store(false, Ordering::SeqCst);
    let args = claude_args(app, cfg, &[], false)?;
    let mut cmd = Command::new(&exe);
    crate::proc::hide(&mut cmd);
    let mut child = cmd
        .args(&args)
        .current_dir(workspace_dir(app))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 Claude Code 失败：{e}"))?;
    let stdin = child.stdin.take().ok_or("无法写入 Claude Code 的标准输入")?;
    let stdout = child.stdout.take().ok_or("无法读取 Claude Code 的输出")?;
    let stderr = child.stderr.take();
    let slot = w.slot.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if let Some(tx) = slot.lock().unwrap().as_ref() {
                    let _ = tx.send(v);
                }
            }
        }
        if let Some(tx) = slot.lock().unwrap().as_ref() {
            let _ = tx.send(json!({ "type": "__eof" }));
        }
    });
    if let Some(e) = stderr {
        std::thread::spawn(move || for _ in BufReader::new(e).lines() {});
    }
    *guard = Some(Proc { child, stdin, sig: s, turns: 0 });
    Ok(true)
}

/// 应用启动 / 配置变更后调用：只常驻当前选中的后端（选哪个常驻哪个），其余的关掉
pub fn prewarm(app: &AppHandle) {
    let cfg = crate::config::get();
    if !cfg.warm_claude {
        kill();
        return;
    }
    match cfg.backend.as_str() {
        "claude" => {
            super::codex_warm::kill();
            if matches!(ensure(app, &cfg), Ok(true)) && !PRIMED.swap(true, Ordering::SeqCst) {
                let sink = Sink::silent(app.clone());
                let _ = ask(app, &cfg, "【预热】只回复：好", &sink);
            }
        }
        "codex" => {
            kill_claude();
            super::codex_warm::prewarm(app, &cfg);
        }
        _ => kill(),
    }
}

fn kill_claude() {
    if let Some(w) = WARM.get() {
        kill_locked(&mut w.proc.lock().unwrap());
    }
}

/// 关掉所有常驻进程（退出 / 关闭预热时）
pub fn kill() {
    kill_claude();
    super::codex_warm::kill();
}

/// 把一条提示词交给常驻进程，阻塞到结果出来（或被取消）
pub fn ask(app: &AppHandle, cfg: &Config, prompt: &str, sink: &Sink) -> Result<String, String> {
    let w = warm();
    let _req = w.req.lock().unwrap();
    if !ensure(app, cfg)? {
        return Err("找不到 claude 命令行。请先安装 Claude Code（设置 → 后端 里可手动指定路径）".into());
    }
    let (tx, rx): (Sender<Value>, Receiver<Value>) = channel();
    *w.slot.lock().unwrap() = Some(tx);
    let msg = json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": prompt }] } });
    {
        let mut g = w.proc.lock().unwrap();
        let Some(p) = g.as_mut() else { return Err("Claude Code 进程已退出，请再试一次".into()) };
        if writeln!(p.stdin, "{msg}").and_then(|_| p.stdin.flush()).is_err() {
            kill_locked(&mut g);
            *w.slot.lock().unwrap() = None;
            return Err("Claude Code 进程已退出，已重启，请再试一次".into());
        }
    }

    let mut res: (Option<String>, bool) = (None, false);
    let mut eof = false;
    let outcome = loop {
        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(v) => {
                if v["type"] == "__eof" {
                    eof = true;
                    break Ok(());
                }
                claude_event(&v, sink, &mut res);
                if v["type"] == "result" {
                    break Ok(());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if sink.cancel.load(Ordering::SeqCst) {
                    kill_locked(&mut w.proc.lock().unwrap());
                    break Err("已取消".to_string());
                }
                sink.heartbeat();
            }
            Err(RecvTimeoutError::Disconnected) => {
                eof = true;
                break Ok(());
            }
        }
    };
    *w.slot.lock().unwrap() = None;
    outcome?;

    let turns = {
        let mut g = w.proc.lock().unwrap();
        g.as_mut().map(|p| {
            p.turns += 1;
            p.turns
        })
    };
    match res {
        (Some(t), false) if !eof => {
            if turns.unwrap_or(0) >= MAX_TURNS {
                let (app2, cfg2) = (app.clone(), cfg.clone());
                std::thread::spawn(move || {
                    let _ = ensure(&app2, &cfg2); // 提前换一个新的，避免上下文越滚越大
                });
            }
            Ok(t)
        }
        (Some(t), true) => Err(format!("Claude Code 报错：{}", claude_trunc(&t, 300))),
        _ => {
            kill_locked(&mut w.proc.lock().unwrap());
            Err("Claude Code 进程意外退出，已重启，请再试一次".into())
        }
    }
}
