//! 弹窗（panel 窗口）桥：状态推送、定位、焦点控制、向用户提问（选文件夹/确认/选项）。
//!
//! 焦点策略：
//! - 听写 / 转写 / 执行中：窗口带 WS_EX_NOACTIVATE，**绝不抢焦点**（用户的选区还在原程序里）。
//! - 需要交互（输入框 / 提问 / 结果可点）：去掉 NOACTIVATE，让点击能正常工作；只有"输入框"会主动 set_focus。

use crate::capture::{FgInfo, Selection};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize};

// ───────────────────────── 视图模型 ─────────────────────────

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    pub path: String,
    pub name: String,
    pub dir: String,
    pub ext: String,
    pub size: u64,
    pub is_dir: bool,
}

pub fn file_view(path: &str) -> FileView {
    let p = std::path::Path::new(path);
    let md = std::fs::metadata(p).ok();
    FileView {
        path: path.to_string(),
        name: p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string()),
        dir: p.parent().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        ext: p.extension().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default(),
        size: md.as_ref().map(|m| m.len()).unwrap_or(0),
        is_dir: md.map(|m| m.is_dir()).unwrap_or(false),
    }
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CtxView {
    /// none | text | files | screen
    pub kind: String,
    pub label: String,
    pub preview: String,
    pub files: Vec<FileView>,
    pub app: String,
    pub title: String,
}

pub fn ctx_view(fg: &FgInfo, sel: &Selection, screen: bool) -> CtxView {
    let mut v = CtxView { kind: "none".into(), app: fg.exe.clone(), title: fg.title.clone(), ..Default::default() };
    if screen {
        v.kind = "screen".into();
        v.label = "当前屏幕 / 页面".into();
        return v;
    }
    match sel {
        Selection::None => v.label = "未选中内容".into(),
        Selection::Text(t) => {
            v.kind = "text".into();
            v.label = format!("已选中 {} 字", t.chars().count());
            v.preview = t.chars().take(120).collect();
        }
        Selection::Files(fs) => {
            v.kind = "files".into();
            v.files = fs.iter().map(|f| file_view(f)).collect();
            v.label = if fs.len() == 1 { v.files[0].name.clone() } else { format!("{} 个文件", fs.len()) };
        }
    }
    v
}

// ───────────────────────── 事件 ─────────────────────────

pub fn emit(app: &AppHandle, v: Value) {
    let _ = app.emit_to("panel", "panel-state", v);
}

pub fn emit_error(app: &AppHandle, msg: &str) {
    interactive(app, false);
    CANCELLABLE.store(false, Ordering::Relaxed); // 报错卡不需要 Esc 取消
    emit(app, json!({ "phase": "error", "message": msg }));
}

// ───────────────────────── 窗口 ─────────────────────────

fn set_noactivate(w: &tauri::WebviewWindow, on: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW};
    if let Ok(h) = w.hwnd() {
        let hwnd = HWND(h.0 as *mut core::ffi::c_void);
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let flag = WS_EX_NOACTIVATE.0 as isize;
            let new = if on { ex | flag | WS_EX_TOOLWINDOW.0 as isize } else { ex & !flag };
            if new != ex {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
            }
        }
    }
}

/// 切换面板是否可交互（false = 不抢焦点的纯展示）
pub fn interactive(app: &AppHandle, yes: bool) {
    CANCELLABLE.store(!yes, Ordering::Relaxed);
    if let Some(w) = app.get_webview_window("panel") {
        set_noactivate(&w, !yes);
    }
}

pub fn focus_panel(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("panel") {
        set_noactivate(&w, false);
        let _ = w.set_focus();
    }
}

/// 面板是否显示着 / 是否处于"不可交互"（聆听、转写、执行中）——此时全局 Esc 可取消整个任务
static PANEL_SHOWN: AtomicBool = AtomicBool::new(false);
static CANCELLABLE: AtomicBool = AtomicBool::new(false);
pub fn esc_cancellable() -> bool {
    PANEL_SHOWN.load(Ordering::Relaxed) && CANCELLABLE.load(Ordering::Relaxed)
}

/// 面板是否固定停靠在屏幕底部正中（用户拖动过就变成 false，下次按热键再停靠）
static DOCKED: AtomicBool = AtomicBool::new(true);
static MON_PT: Mutex<(i32, i32)> = Mutex::new((0, 0));
/// 页面最近一次上报的布局（逻辑像素）
static LAYOUT: Mutex<(f64, f64)> = Mutex::new((380.0, 70.0));
use std::sync::atomic::AtomicBool;

fn monitor_at(w: &tauri::WebviewWindow, pt: (i32, i32)) -> Option<tauri::Monitor> {
    let mons = w.available_monitors().ok()?;
    mons.iter()
        .find(|m| {
            let (p, s) = (m.position(), m.size());
            pt.0 >= p.x && pt.0 < p.x + s.width as i32 && pt.1 >= p.y && pt.1 < p.y + s.height as i32
        })
        .cloned()
        .or_else(|| w.primary_monitor().ok().flatten())
}

/// 底部正中：水平居中，底边距工作区底部 28 逻辑像素；高度变化时向上生长
fn dock(w: &tauri::WebviewWindow) {
    let (lw, lh) = *LAYOUT.lock().unwrap();
    let pt = *MON_PT.lock().unwrap();
    let Some(m) = monitor_at(w, pt) else { return };
    let scale = m.scale_factor();
    let (pw, ph) = ((lw * scale).round() as i32, (lh * scale).round() as i32);
    let wa = m.work_area();
    let (l, r, b) = (wa.position.x, wa.position.x + wa.size.width as i32, wa.position.y + wa.size.height as i32);
    let x = l + (r - l - pw) / 2;
    let y = b - (28.0 * scale).round() as i32 - ph;
    let _ = w.set_size(PhysicalSize::new(pw.max(1) as u32, ph.max(1) as u32));
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

/// 按热键时显示面板：停靠在光标所在屏幕的底部正中，不抢焦点
pub fn show_panel_at(app: &AppHandle, cursor: (i32, i32)) {
    let Some(w) = app.get_webview_window("panel") else { return };
    DOCKED.store(true, Ordering::Relaxed);
    *MON_PT.lock().unwrap() = cursor;
    set_noactivate(&w, true);
    dock(&w);
    let _ = w.show();
    PANEL_SHOWN.store(true, Ordering::Relaxed);
    let _ = w.set_always_on_top(true);
}

pub fn hide_panel(app: &AppHandle) {
    PANEL_SHOWN.store(false, Ordering::Relaxed);
    CANCELLABLE.store(false, Ordering::Relaxed);
    if let Some(w) = app.get_webview_window("panel") {
        let _ = w.hide();
    }
}

pub fn panel_visible(app: &AppHandle) -> bool {
    app.get_webview_window("panel").and_then(|w| w.is_visible().ok()).unwrap_or(false)
}

/// 页面上报内容尺寸（逻辑像素）；窗口随内容缩放，停靠时保持底部正中
#[tauri::command]
pub fn panel_layout(app: AppHandle, w: f64, h: f64) {
    let Some(win) = app.get_webview_window("panel") else { return };
    if !(w.is_finite() && h.is_finite()) || w < 20.0 || h < 20.0 {
        return;
    }
    *LAYOUT.lock().unwrap() = (w.min(900.0), h.min(900.0));
    if DOCKED.load(Ordering::Relaxed) {
        dock(&win);
    } else {
        let s = win.scale_factor().unwrap_or(1.0);
        let _ = win.set_size(PhysicalSize::new((w.min(900.0) * s).round() as u32, (h.min(900.0) * s).round() as u32));
    }
}

/// 用户按住面板拖动
#[tauri::command]
pub fn panel_drag(app: AppHandle) {
    if let Some(w) = app.get_webview_window("panel") {
        DOCKED.store(false, Ordering::Relaxed);
        let _ = w.start_dragging();
    }
}

#[tauri::command]
pub fn panel_focus(app: AppHandle) {
    focus_panel(&app);
}

#[tauri::command]
pub fn panel_hide(app: AppHandle) {
    crate::session::cancel_current(&app);
    hide_panel(&app);
    emit(&app, json!({ "phase": "idle" }));
}

// ───────────────────────── 向用户提问 ─────────────────────────

static PENDING: OnceLock<Mutex<HashMap<String, Sender<Value>>>> = OnceLock::new();
fn pending() -> &'static Mutex<HashMap<String, Sender<Value>>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}
static ASK_ID: AtomicU64 = AtomicU64::new(1);

/// 阻塞等待用户在弹窗里作答。spec 至少含 kind/title；返回前端回传的 value。
pub fn ask(app: &AppHandle, mut spec: Value) -> Result<Value, String> {
    let id = format!("ask{}", ASK_ID.fetch_add(1, Ordering::Relaxed));
    let (tx, rx) = channel::<Value>();
    pending().lock().unwrap().insert(id.clone(), tx);
    spec["phase"] = json!("ask");
    spec["id"] = json!(id);
    if spec["kind"] == "folder" && spec.get("suggestions").is_none() {
        spec["suggestions"] = json!(folder_suggestions());
    }
    if !panel_visible(app) {
        show_panel_at(app, crate::capture::cursor_xy());
    }
    interactive(app, true);
    emit(app, spec);
    match rx.recv_timeout(Duration::from_secs(600)) {
        Ok(v) => Ok(v),
        Err(_) => {
            pending().lock().unwrap().remove(&id);
            Err("等待用户选择超时，或已被取消".into())
        }
    }
}

pub fn cancel_asks() {
    pending().lock().unwrap().clear(); // 丢掉 Sender → 等待方立刻收到断开错误
}

#[tauri::command]
pub fn ask_reply(id: String, value: Value) {
    if let Some(tx) = pending().lock().unwrap().remove(&id) {
        let _ = tx.send(value);
    }
}

// ───────────────────────── 文件夹 ─────────────────────────

#[derive(Serialize, Clone)]
pub struct FolderItem {
    pub path: String,
    pub name: String,
    pub kind: String,
}

#[tauri::command]
pub fn folder_suggestions() -> Vec<FolderItem> {
    let mut out: Vec<FolderItem> = vec![];
    let mut seen: Vec<String> = vec![];
    let mut add = |path: String, name: &str, kind: &str, out: &mut Vec<FolderItem>| {
        let key = path.to_lowercase();
        if std::path::Path::new(&path).is_dir() && !seen.contains(&key) {
            seen.push(key);
            out.push(FolderItem { path, name: name.to_string(), kind: kind.to_string() });
        }
    };
    // 源文件所在目录
    if let Some(dir) = crate::session::current_source_dir() {
        add(dir, "源文件所在文件夹", "source", &mut out);
    }
    for f in crate::config::get().recent_folders {
        let name = std::path::Path::new(&f).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| f.clone());
        add(f, &name, "recent", &mut out);
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        for (sub, name) in [("Desktop", "桌面"), ("Documents", "文档"), ("Downloads", "下载"), ("Pictures", "图片"), ("Videos", "视频"), ("Music", "音乐")] {
            add(format!("{home}\\{sub}"), name, "known", &mut out);
        }
    }
    out
}

/// 系统文件夹选择器（挂在面板上，避免被置顶面板盖住）
#[tauri::command]
pub async fn browse_folder(app: AppHandle, start: Option<String>) -> Option<String> {
    let win = app.get_webview_window("panel");
    if let Some(w) = &win {
        let _ = w.set_always_on_top(false);
    }
    let win2 = win.clone();
    let r = tokio::task::spawn_blocking(move || {
        let mut d = rfd::FileDialog::new().set_title("选择文件夹");
        if let Some(s) = start {
            if std::path::Path::new(&s).is_dir() {
                d = d.set_directory(s);
            }
        }
        if let Some(w) = &win2 {
            d = d.set_parent(w);
        }
        d.pick_folder()
    })
    .await
    .ok()
    .flatten();
    if let Some(w) = &win {
        let _ = w.set_always_on_top(true);
    }
    r.map(|p| p.to_string_lossy().to_string())
}

// ───────────────────────── 文件操作（弹窗里的按钮） ─────────────────────────

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("cmd")
        .args(["/C", "start", "", &path])
        .creation_flags(0x0800_0000)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let p = path.replace('/', "\\");
    let arg = if std::path::Path::new(&p).is_dir() { format!("\"{p}\"") } else { format!("/select,\"{p}\"") };
    std::process::Command::new("explorer")
        .raw_arg(arg)
        .creation_flags(0x0800_0000)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
