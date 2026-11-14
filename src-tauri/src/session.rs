//! 一次"按住说话"的完整生命周期：
//!   按下 → 录音 + 抓选区 → 松开 → 转写 → 路由(屏幕/选文件夹) → agent 执行 → 弹窗给结果
//! 以及"短按/打字/追问"走的同一条执行通路。

use crate::agent::{self, RunInput, Sink};
use crate::audio::{self, Recorder};
use crate::capture::{self, FgInfo, Selection};
use crate::{config, history, router, stt, tools, ui};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::AppHandle;

pub struct Session {
    pub id: u64,
    pub fg: FgInfo,
    pub cursor: (i32, i32),
    pub sel: Mutex<Selection>,
    pub ready: AtomicBool,
    pub rec: Mutex<Option<Recorder>>,
    /// 上一轮 (指令, 回答)，追问用
    pub last: Mutex<Option<(String, String)>>,
    pub screen: AtomicBool,
    /// 按下热键时自动保存的截图（只保存、不读取）
    pub shot: Mutex<Option<PathBuf>>,
    pub shot_ready: AtomicBool,
    /// 前台 Office 文档的描述（按下热键时并行取好）
    pub doc: Mutex<Option<String>>,
    pub doc_ready: AtomicBool,
}

static CUR: OnceLock<Mutex<Option<Arc<Session>>>> = OnceLock::new();
fn cur() -> &'static Mutex<Option<Arc<Session>>> {
    CUR.get_or_init(|| Mutex::new(None))
}
static PRESS_SEQ: AtomicU64 = AtomicU64::new(0);
static RUN_PID: AtomicU32 = AtomicU32::new(0);
static RUN_CANCEL: OnceLock<Mutex<Arc<AtomicBool>>> = OnceLock::new();
fn run_cancel() -> &'static Mutex<Arc<AtomicBool>> {
    RUN_CANCEL.get_or_init(|| Mutex::new(Arc::new(AtomicBool::new(false))))
}

pub fn set_run_pid(pid: u32) {
    RUN_PID.store(pid, Ordering::SeqCst);
}

/// 取消当前进行中的一切：agent 子进程、等待中的提问、录音
pub fn cancel_current(_app: &AppHandle) {
    run_cancel().lock().unwrap().store(true, Ordering::SeqCst);
    ui::cancel_asks();
    let pid = RUN_PID.swap(0, Ordering::SeqCst);
    if pid != 0 {
        crate::proc::kill_tree(pid);
    }
    if let Some(s) = cur().lock().unwrap().as_ref() {
        s.rec.lock().unwrap().take(); // 丢弃录音器 → 线程结束
    }
}

fn new_run_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    *run_cancel().lock().unwrap() = flag.clone();
    flag
}

pub fn current_source_dir() -> Option<String> {
    let s = cur().lock().unwrap().clone()?;
    let sel = s.sel.lock().unwrap();
    if let Selection::Files(fs) = &*sel {
        return std::path::Path::new(fs.first()?).parent().map(|p| p.to_string_lossy().to_string());
    }
    None
}

pub fn current_fg_cursor() -> (FgInfo, (i32, i32)) {
    match cur().lock().unwrap().as_ref() {
        Some(s) => (s.fg.clone(), s.cursor),
        None => (capture::foreground(), capture::cursor_xy()),
    }
}

pub fn describe_context() -> String {
    let Some(s) = cur().lock().unwrap().clone() else { return "当前没有上下文".into() };
    let mut o = format!("前台程序：{}，窗口标题：{}\n", s.fg.exe, s.fg.title);
    match &*s.sel.lock().unwrap() {
        Selection::None => o.push_str("选中内容：无"),
        Selection::Text(t) => o.push_str(&format!("选中文字：\n{t}")),
        Selection::Files(f) => o.push_str(&format!("选中文件：\n{}", f.join("\n"))),
    }
    o
}

// ───────────────────────── 实时字幕 ─────────────────────────

/// 按住期间每 ~0.6s 用本地模型把已录的音频整段重识别一次，让用户边说边看到识别结果，说错了当场就能发现。
/// SenseVoice 不是流式模型，但识别几秒音频只要零点几秒，整段重识别足够快。云端引擎不支持，松开后才出字。
fn caption_loop(app: AppHandle, s: Arc<Session>) {
    let mut last_n = 0usize;
    let started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(600));
        // 录得太久（多半是漏了松开事件）就不再刷字幕：整段重识别的开销随时长增长，会把 CPU 吃满
        if started.elapsed() > Duration::from_secs(90) {
            return;
        }
        let cfg = config::get();
        if cfg.stt_engine != "local" || !stt::model_ready(&app) {
            return;
        }
        let mut pcm = match s.rec.lock().unwrap().as_ref() {
            Some(r) => r.snapshot(),
            None => return, // 录音已结束（松开 / 取消）
        };
        if pcm.len() < 11200 || pcm.len() == last_n {
            continue;
        }
        last_n = pcm.len();
        // 字幕只重识别最后 20 秒，开销有上限
        if pcm.len() > 16000 * 20 {
            pcm = pcm[pcm.len() - 16000 * 20..].to_vec();
        }
        // 静音喂给模型会"幻听"出一个不相干的字（比如韩语的"그"），直接跳过
        if audio::is_silent(&pcm) {
            continue;
        }
        audio::normalize(&mut pcm);
        if let Ok(t) = stt::transcribe(&app, &pcm, stt::lang_id(&cfg.lang)) {
            let t = t.trim();
            // 录音结束后不再推送，免得盖掉最终结果
            if !t.is_empty() && s.rec.lock().unwrap().is_some() {
                ui::emit(&app, json!({ "phase": "caption", "text": t }));
            }
        }
    }
}

// ───────────────────────── 按下 ─────────────────────────

pub fn on_down(app: AppHandle) {
    let seq = PRESS_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    // 按下的瞬间面板是不是正显示着上一轮的结果（要在任何取消 / 隐藏动作之前判断）
    let result_up = ui::result_showing();
    std::thread::spawn(move || {
        cancel_current(&app);
        let fg = capture::foreground();
        let cursor = capture::cursor_xy();
        let prev = cur().lock().unwrap().clone();
        // 追问 = 对上一轮结果接着说：沿用上一轮的上下文，不再抓选区。两种情况：
        //  1. 前台就是 SayDo 自己的弹窗（用户点过它）；
        //  2. 结果卡还开着（面板不抢焦点，前台仍是原来的程序）且上一轮有结果 → 直接按热键就接着聊，不用先点它。
        // 结果卡已经关掉就是新对话。
        let has_prev_answer = prev.as_ref().map(|p| p.last.lock().unwrap().is_some()).unwrap_or(false);
        let from_self = fg.pid == std::process::id() || (result_up && has_prev_answer);
        let (fg, sel, ready) = match (&prev, from_self) {
            (Some(p), true) => (p.fg.clone(), p.sel.lock().unwrap().clone(), true),
            _ => (fg, Selection::None, false),
        };
        let s = Arc::new(Session {
            id: seq,
            fg,
            cursor,
            sel: Mutex::new(sel),
            ready: AtomicBool::new(ready),
            rec: Mutex::new(None),
            last: Mutex::new(if from_self { prev.as_ref().and_then(|p| p.last.lock().unwrap().clone()) } else { None }),
            screen: AtomicBool::new(false),
            shot: Mutex::new(None),
            shot_ready: AtomicBool::new(false),
            doc: Mutex::new(None),
            doc_ready: AtomicBool::new(false),
        });
        *cur().lock().unwrap() = Some(s.clone());
        tools::reset_allow();
        // 前台是 Office：并行问它"现在开的是哪份文档"（不增加等待）；追问沿用上一轮的
        if from_self {
            *s.doc.lock().unwrap() = prev.as_ref().and_then(|p| p.doc.lock().unwrap().clone());
            s.doc_ready.store(true, Ordering::SeqCst);
        } else {
            let s6 = s.clone();
            std::thread::spawn(move || {
                *s6.doc.lock().unwrap() = capture::office_context(&s6.fg);
                s6.doc_ready.store(true, Ordering::SeqCst);
            });
        }
        crate::log::log(&format!("session {seq}: down, fg={} follow_up={}", s.fg.exe, from_self));

        match audio::start(app.clone()) {
            Ok(r) => *s.rec.lock().unwrap() = Some(r),
            Err(e) => {
                ui::show_panel_at(&app, cursor);
                ui::emit_error(&app, &e);
                s.ready.store(true, Ordering::SeqCst);
                return;
            }
        }
        // 按下热键的同一瞬间：给当前页面截一张图（只保存，不读取）。先于弹窗出现，免得截到自己
        if from_self {
            *s.shot.lock().unwrap() = prev.as_ref().and_then(|p| p.shot.lock().unwrap().clone());
            s.shot_ready.store(true, Ordering::SeqCst);
        } else {
            if ui::panel_visible(&app) {
                ui::hide_panel(&app);
                std::thread::sleep(Duration::from_millis(40));
            }
            match capture::grab_image(s.fg.hwnd, cursor) {
                Ok(img) => {
                    let (s2, dir) = (s.clone(), config::config_dir(&app).join("shots"));
                    std::thread::spawn(move || {
                        if let Ok(p) = capture::save_image(&img, &dir) {
                            *s2.shot.lock().unwrap() = Some(p);
                        }
                        s2.shot_ready.store(true, Ordering::SeqCst);
                    });
                }
                Err(_) => s.shot_ready.store(true, Ordering::SeqCst),
            }
        }
        {
            // 最长录 60 秒：到点自动当作松开，避免漏掉松开事件时一直录下去
            let (app5, s5) = (app.clone(), s.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(60));
                if PRESS_SEQ.load(Ordering::SeqCst) == s5.id && s5.rec.lock().unwrap().is_some() {
                    crate::log::log(&format!("session {}: 60s limit reached, auto-release", s5.id));
                    on_up(app5);
                }
            });
            let (app2, s2) = (app.clone(), s.clone());
            std::thread::spawn(move || caption_loop(app2, s2));
            let app3 = app.clone();
            std::thread::spawn(move || stt::preload(&app3)); // 趁用户说话的时间加载语音模型
        }
        if !from_self || !ui::panel_visible(&app) {
            ui::show_panel_at(&app, cursor);
        }
        ui::interactive(&app, false);
        let pending = ui::CtxView { kind: "pending".into(), label: "正在读取选中内容…".into(), app: s.fg.exe.clone(), title: s.fg.title.clone(), ..Default::default() };
        ui::emit(&app, json!({ "phase": "listening", "followUp": from_self, "ctx": if from_self { ui::ctx_view(&s.fg, &s.sel.lock().unwrap(), false) } else { pending } }));

        if !from_self {
            let sel = capture::grab_selection(&s.fg);
            let view = ui::ctx_view(&s.fg, &sel, false);
            *s.sel.lock().unwrap() = sel;
            ui::emit(&app, json!({ "phase": "ctx", "ctx": view }));
        }
        s.ready.store(true, Ordering::SeqCst);
    });
}

// ───────────────────────── 松开 ─────────────────────────

fn wait_for<F: Fn() -> bool>(f: F, max_ms: u64) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(max_ms) {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

pub fn on_up(app: AppHandle) {
    let seq = PRESS_SEQ.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let get = || cur().lock().unwrap().clone().filter(|s| s.id == seq);
        if !wait_for(|| get().is_some(), 3000) {
            return;
        }
        let s = get().unwrap();
        wait_for(|| s.ready.load(Ordering::SeqCst), 2500);
        let Some(rec) = s.rec.lock().unwrap().take() else {
            crate::log::log(&format!("session {}: up but no recorder (already stopped)", s.id));
            return;
        };
        let mut pcm = rec.stop();
        crate::log::log(&format!("session {}: recorder stopped, {:.1}s of audio", s.id, pcm.len() as f32 / 16000.0));
        let cfg = config::get();

        // 短按（<0.3s）：不是说话，是想打字
        if pcm.len() < 16000 * 3 / 10 {
            if cfg.tap_opens_input {
                show_input(&app, &s);
            } else {
                ui::hide_panel(&app);
            }
            return;
        }

        if audio::is_silent(&pcm) {
            crate::log::log(&format!("session {}: audio is pure silence (no mic permission?)", s.id));
            ui::emit_error(
                &app,
                if cfg!(target_os = "macos") {
                    "没有录到任何声音：请在 系统设置 → 隐私与安全性 → 麦克风 里允许 SayDo，并确认输入设备没有静音"
                } else {
                    "没有录到任何声音：请检查麦克风权限，并确认输入设备没有静音"
                },
            );
            return;
        }
        ui::emit(&app, json!({ "phase": "transcribing" }));
        audio::normalize(&mut pcm);
        let r = if cfg.stt_engine == "cloud" {
            stt::transcribe_cloud(&cfg.cloud_stt, &pcm, &cfg.lang)
        } else if !stt::model_ready(&app) {
            Err("本地语音模型还没下载（设置 → 语音 → 下载模型），或改用云端转写".to_string())
        } else {
            stt::transcribe(&app, &pcm, stt::lang_id(&cfg.lang))
        };
        match &r {
            Ok(t) => crate::log::log(&format!("session {}: transcribed {} chars", s.id, t.chars().count())),
            Err(e) => crate::log::log(&format!("session {}: transcribe error: {e}", s.id)),
        }
        match r {
            Ok(t) if !t.trim().is_empty() => {
                ui::emit(&app, json!({ "phase": "caption", "text": t.trim(), "final": true }));
                run_instruction(&app, s, t.trim().to_string())
            }
            Ok(_) => ui::emit_error(&app, "没听清，请再按住说一次"),
            Err(e) => ui::emit_error(&app, &e),
        }
    });
}

fn show_input(app: &AppHandle, s: &Arc<Session>) {
    ui::interactive(app, true);
    let ctx = ui::ctx_view(&s.fg, &s.sel.lock().unwrap(), s.screen.load(Ordering::SeqCst));
    ui::emit(app, json!({ "phase": "input", "ctx": ctx, "hasPrev": s.last.lock().unwrap().is_some() }));
    ui::focus_panel(app);
}

// ───────────────────────── 执行 ─────────────────────────

/// 面板里打字/点快捷芯片/追问
#[tauri::command]
pub fn panel_submit(app: AppHandle, text: String) {
    let t = text.trim().to_string();
    if t.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        cancel_current(&app);
        // 先把锁放掉再决定要不要新建：在 unwrap_or_else 的闭包里再次 lock 同一把锁会死锁
        let existing = cur().lock().unwrap().clone();
        let s = existing.unwrap_or_else(|| {
            let fg = capture::foreground();
            let s = Arc::new(Session {
                id: 0,
                fg,
                cursor: capture::cursor_xy(),
                sel: Mutex::new(Selection::None),
                ready: AtomicBool::new(true),
                rec: Mutex::new(None),
                last: Mutex::new(None),
                screen: AtomicBool::new(false),
                shot: Mutex::new(None),
                shot_ready: AtomicBool::new(true),
                doc: Mutex::new(None),
                doc_ready: AtomicBool::new(true),
            });
            *cur().lock().unwrap() = Some(s.clone());
            s
        });
        run_instruction(&app, s, t);
    });
}

#[tauri::command]
pub fn panel_replace(app: AppHandle, text: String) -> Result<(), String> {
    let hwnd = cur().lock().unwrap().as_ref().map(|s| s.fg.hwnd).unwrap_or(0);
    ui::hide_panel(&app);
    capture::paste_back(hwnd, &text)
}

fn run_instruction(app: &AppHandle, s: Arc<Session>, text: String) {
    let cfg = config::get();
    let cancel = new_run_flag();
    ui::interactive(app, false);
    ui::emit(app, json!({ "phase": "heard", "text": text }));

    // "记住……" / "忘掉……"：本地直接处理，不用问 Agent
    match router::memory_command(&text) {
        Some(router::MemCmd::Remember(m)) => {
            let added = config::add_memory(app, &m);
            memory_reply(app, &text, if added { format!("已记住：{m}") } else { format!("早就记着了：{m}") });
            return;
        }
        Some(router::MemCmd::Forget(k)) => {
            let n = config::forget_memory(app, &k);
            if n > 0 {
                memory_reply(app, &text, format!("已忘掉 {n} 条记忆"));
                return;
            }
            // 没有匹配的记忆：当作普通指令继续（比如"忘记密码怎么办"）
        }
        None => {}
    }

    let sel = s.sel.lock().unwrap().clone();
    let mut sel_used = sel.clone();
    let mut shot: Option<PathBuf> = None;
    let mut dest: Option<String> = None;

    wait_for(|| s.shot_ready.load(Ordering::SeqCst), 2000);
    let saved: Option<PathBuf> = s.shot.lock().unwrap().clone().filter(|p| p.exists());
    let mut shot_saved = saved.clone();
    if router::is_screen_intent(&text) {
        // macOS 没有「屏幕录制」权限时，截到的只有桌面壁纸或空白：直接说清楚，别让 Agent 对着一张空图瞎答
        #[cfg(target_os = "macos")]
        {
            if !crate::mac::screen_capture_allowed() {
                crate::mac::request_screen_capture();
                return ui::emit_error(
                    app,
                    "SayDo 没有「屏幕录制」权限，截不到屏幕内容：请在 系统设置 → 隐私与安全性 → 屏幕录制 里打开 SayDo，然后退出并重新打开 SayDo",
                );
            }
        }
        ui::emit(app, json!({ "phase": "working", "steps": ["读取当前屏幕…"], "stepCount": 1, "text": "", "elapsed": 0 }));
        let mut got = saved.clone();
        if got.is_none() {
            // 没有按下时的截图（比如纯打字唤起）：现截一张
            use tauri::Manager;
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.hide();
            }
            std::thread::sleep(Duration::from_millis(180));
            let dir = config::config_dir(app).join("shots");
            let r = capture::screenshot(s.fg.hwnd, s.cursor, &dir);
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.show();
            }
            match r {
                Ok(p) => got = Some(p),
                Err(e) => return ui::emit_error(app, &format!("截屏失败：{e}")),
            }
        }
        shot = got;
        shot_saved = None;
        sel_used = Selection::None;
        s.screen.store(true, Ordering::SeqCst);
        ui::emit(app, json!({ "phase": "ctx", "ctx": ui::ctx_view(&s.fg, &sel_used, true) }));
    } else {
        s.screen.store(false, Ordering::SeqCst);
        if router::needs_folder(&text, &sel) {
            let label = match &sel {
                Selection::Files(f) if f.len() == 1 => ui::file_view(&f[0]).name,
                Selection::Files(f) => format!("{} 个文件", f.len()),
                _ => String::new(),
            };
            match ui::ask(app, json!({ "kind": "folder", "title": "放到哪个文件夹？", "detail": label })) {
                Ok(v) => match v["path"].as_str() {
                    Some(p) if !p.is_empty() => {
                        config::remember_folder(app, p);
                        dest = Some(p.to_string());
                    }
                    _ => {
                        ui::hide_panel(app);
                        return;
                    }
                },
                Err(_) => return,
            }
            ui::interactive(app, false);
        }
    }
    if cancel.load(Ordering::SeqCst) {
        return;
    }

    let ctx_label = ui::ctx_view(&s.fg, &sel_used, shot.is_some()).label;
    wait_for(|| s.doc_ready.load(Ordering::SeqCst), 2000);
    let doc_ctx = s.doc.lock().unwrap().clone();
    // 每个任务一个专用临时文件夹（放在 Agent 的工作目录里，不用额外授权）
    let scratch = agent::workspace_dir(app).join("scratch").join(format!("{}", history::now()));
    let _ = std::fs::create_dir_all(&scratch);
    let input = RunInput { instruction: text.clone(), fg: s.fg.clone(), sel: sel_used, shot, shot_saved, doc_ctx, scratch: Some(scratch.clone()), dest, prev: s.last.lock().unwrap().clone() };
    let sink = Sink::new(app.clone(), cancel.clone());
    sink.step("思考中…");
    let t0 = Instant::now();
    let res = tauri::async_runtime::block_on(agent::run(app, &cfg, input, &sink));
    cleanup_scratch(&scratch, res.as_ref().ok().map(|o| o.files.as_slice()).unwrap_or(&[]));
    if cancel.load(Ordering::SeqCst) {
        return;
    }
    let elapsed = t0.elapsed().as_secs();
    match res {
        Ok(out) => {
            *s.last.lock().unwrap() = Some((text.clone(), out.text.clone()));
            history::push(app, history::Entry { ts: history::now(), instruction: text.clone(), result: out.text.clone(), backend: cfg.backend.clone(), context: ctx_label, files: out.files.clone(), ok: true });
            if !ui::panel_visible(app) {
                ui::show_panel_at(app, s.cursor);
            }
            ui::interactive(app, true);
            let can_replace = matches!(&*s.sel.lock().unwrap(), Selection::Text(_)) && !s.screen.load(Ordering::SeqCst);
            ui::emit(
                app,
                json!({
                    "phase": "result",
                    "text": out.text,
                    "files": out.files.iter().map(|f| ui::file_view(f)).collect::<Vec<_>>(),
                    "canReplace": can_replace,
                    "backend": cfg.backend,
                    "elapsed": elapsed,
                    "instruction": text,
                }),
            );
        }
        Err(e) => {
            history::push(app, history::Entry { ts: history::now(), instruction: text, result: e.clone(), backend: cfg.backend.clone(), context: ctx_label, files: vec![], ok: false });
            ui::emit_error(app, &e);
        }
    }
}

/// 任务结束后清掉这个任务的临时文件夹，并顺手清理超过 6 小时的旧残留。
/// 如果 Agent 把成品也放进了临时文件夹（结果文件在里面），就保留，免得删掉用户要的东西。
fn cleanup_scratch(scratch: &std::path::Path, result_files: &[String]) {
    let sc = scratch.to_string_lossy().to_lowercase();
    if result_files.iter().any(|f| f.to_lowercase().starts_with(&sc)) {
        crate::log::log(&format!("scratch kept (contains a result file): {}", scratch.display()));
        return;
    }
    let dir = scratch.to_path_buf();
    std::thread::spawn(move || {
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(parent) = dir.parent() {
            let now = history::now();
            if let Ok(rd) = std::fs::read_dir(parent) {
                for e in rd.flatten() {
                    let stale = e.file_name().to_string_lossy().parse::<u64>().map(|t| now.saturating_sub(t) > 6 * 3600).unwrap_or(false);
                    if stale {
                        let _ = std::fs::remove_dir_all(e.path());
                    }
                }
            }
        }
    });
}

/// 记忆指令的回执：一张很小的结果卡
fn memory_reply(app: &AppHandle, instruction: &str, text: String) {
    use tauri::Emitter;
    let _ = app.emit("memories-changed", json!({}));
    if !ui::panel_visible(app) {
        ui::show_panel_at(app, capture::cursor_xy());
    }
    ui::interactive(app, true);
    ui::emit(app, json!({ "phase": "result", "text": text, "files": [], "canReplace": false, "backend": "memory", "elapsed": 0, "instruction": instruction }));
}

/// 设置页"测试后端"：不带任何上下文，让后端只回两个字
#[tauri::command]
pub async fn test_backend(app: AppHandle, backend: Option<String>) -> Result<String, String> {
    let mut cfg = config::get();
    if let Some(b) = backend {
        cfg.backend = b;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let sink = Sink::new(app.clone(), cancel);
    let inp = RunInput {
        instruction: "这是连通性测试。只回复两个字：成功".into(),
        fg: FgInfo::default(),
        sel: Selection::None,
        shot: None,
        shot_saved: None,
        doc_ctx: None,
        scratch: None,
        dest: None,
        prev: None,
    };
    let t0 = Instant::now();
    let out = agent::run(&app, &cfg, inp, &sink).await?;
    Ok(format!("{}（{:.1}s）", out.text.trim(), t0.elapsed().as_secs_f32()))
}
