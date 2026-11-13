//! 抓取"用户当前面对的东西"：前台窗口信息、选中文字/文件、截屏、把文字粘回原程序。
//!
//! 选区抓取 = 模拟 Ctrl+C 再读剪贴板（文件 = CF_HDROP，文字 = CF_UNICODETEXT），然后还原剪贴板。
//! 关键细节：
//! 1. 用户此刻正按着热键修饰键（Ctrl/Alt/Shift/Win）。直接发 Ctrl+C 会变成 Ctrl+Alt+C。
//!    所以先临时"松开"Alt/Shift/Win（注入 keyup），发完再"按回去"，让系统状态和物理状态一致。
//! 2. 终端里 Ctrl+C 是 SIGINT，会打断正在跑的程序 → 终端类程序改用 Ctrl+Insert（同样是复制，不发信号）。
//! 3. 用剪贴板序列号判断"有没有复制成功"，而不是靠哨兵值，这样不会破坏用户原剪贴板。
//!
//! 平台实现分开放：Windows = capture_win.rs，macOS = capture_mac.rs（先问辅助功能 API 要选中文字，不行再 ⌘C）。
//! 这个文件只留两边共用的部分（数据结构、截屏、文档描述）。

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FgInfo {
    /// Windows：窗口句柄；macOS：进程号（用来切回前台 / 找它的窗口截图）
    pub hwnd: isize,
    pub pid: u32,
    pub title: String,
    /// Windows：exe 文件名（如 chrome.exe）；macOS：应用名（如 Google Chrome）
    pub exe: String,
    /// macOS：bundle id（如 com.google.Chrome）；Windows 为空
    pub bundle: String,
}

#[derive(Clone, Debug, Default)]
pub enum Selection {
    #[default]
    None,
    Text(String),
    Files(Vec<String>),
}

#[cfg(windows)]
#[path = "capture_win.rs"]
mod imp;
#[cfg(target_os = "macos")]
#[path = "capture_mac.rs"]
mod imp;
#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    //! 其它系统：只为了能编译 / 跑单元测试，不提供实际功能
    use super::{FgInfo, Selection};
    pub fn cursor_xy() -> (i32, i32) {
        (0, 0)
    }
    pub fn foreground() -> FgInfo {
        FgInfo::default()
    }
    pub fn grab_selection(_fg: &FgInfo) -> Selection {
        Selection::None
    }
    pub fn paste_back(_hwnd: isize, _text: &str) -> Result<(), String> {
        Err("这个系统不支持".into())
    }
    pub fn office_context(_fg: &FgInfo) -> Option<String> {
        let _ = super::describe_doc;
        None
    }
}
pub use imp::{cursor_xy, foreground, grab_selection, office_context, paste_back};

// ───────────────────────── 截屏 ─────────────────────────

/// 截"源程序窗口"，失败则截鼠标所在显示器。保存为 PNG（长边最多 1920），返回路径。
/// 抓图（前台窗口，退而求其次整个光标所在显示器），长边缩到 1920；只抓不存，方便按热键时抢在弹窗出现前完成
pub fn grab_image(fg_hwnd: isize, cursor: (i32, i32)) -> Result<image::RgbaImage, String> {
    let mut img = None;
    if fg_hwnd != 0 {
        if let Ok(wins) = xcap::Window::all() {
            #[cfg(not(target_os = "macos"))]
            for w in wins {
                if w.id().ok().map(|i| i as isize) == Some(fg_hwnd) && !w.is_minimized().unwrap_or(false) {
                    if let Ok(i) = w.capture_image() {
                        if i.width() > 200 && i.height() > 100 {
                            img = Some(i);
                        }
                    }
                    break;
                }
            }
            // macOS：fg_hwnd 存的是进程号。一个程序有很多"窗口"（菜单栏图标、浮层…），列表按从前到后排，
            // 取这个进程最靠前的、够大的那个
            #[cfg(target_os = "macos")]
            for w in wins {
                if w.pid().ok().map(|i| i as isize) != Some(fg_hwnd) || w.is_minimized().unwrap_or(false) {
                    continue;
                }
                if w.width().unwrap_or(0) <= 200 || w.height().unwrap_or(0) <= 100 {
                    continue;
                }
                if let Ok(i) = w.capture_image() {
                    if i.width() > 200 && i.height() > 100 {
                        img = Some(i);
                        break;
                    }
                }
            }
        }
    }
    if img.is_none() {
        let mon = xcap::Monitor::from_point(cursor.0, cursor.1).map_err(|e| format!("找不到显示器: {e}"))?;
        img = Some(mon.capture_image().map_err(|e| format!("截屏失败: {e}"))?);
    }
    let mut img = img.unwrap();
    let max = 1920u32;
    if img.width().max(img.height()) > max {
        let (w, h) = (img.width(), img.height());
        let s = max as f32 / w.max(h) as f32;
        img = image::imageops::resize(&img, (w as f32 * s) as u32, (h as f32 * s) as u32, image::imageops::FilterType::Triangle);
    }
    Ok(img)
}

/// 保存成 shots/shot-<ms>.png，只留最近 12 张
pub fn save_image(img: &image::RgbaImage, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = format!("shot-{}.png", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
    let p = dir.join(name);
    img.save(&p).map_err(|e| format!("保存截图失败: {e}"))?;
    // 只留最近 12 张
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut v: Vec<_> = rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("shot-")).collect();
        v.sort_by_key(|e| e.file_name());
        if v.len() > 12 {
            for e in &v[..v.len() - 12] {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(p)
}

pub fn screenshot(fg_hwnd: isize, cursor: (i32, i32), dir: &Path) -> Result<PathBuf, String> {
    save_image(&grab_image(fg_hwnd, cursor)?, dir)
}


// ───────────────────────── Office：前台打开的是哪份文档 ─────────────────────────

/// 把"问到的文档信息"拼成一行可直接放进提示词的描述
fn describe_doc(prog: &str, v: &serde_json::Value) -> Option<String> {
    let path = v["path"].as_str().unwrap_or("");
    let mut parts: Vec<String> = vec![];
    if let Some(n) = v["slides"].as_i64() {
        parts.push(format!("共 {n} 页"));
    }
    if let Some(n) = v["slide"].as_i64() {
        parts.push(format!("光标在第 {n} 页"));
    }
    if let Some(n) = v["pages"].as_i64() {
        parts.push(format!("共 {n} 页"));
    }
    if let Some(a) = v["shapes"].as_array() {
        let names: Vec<&str> = a.iter().filter_map(|x| x.as_str()).collect();
        if !names.is_empty() {
            parts.push(format!("选中了形状：{}", names.join("、")));
        }
    }
    if let Some(t) = v["text"].as_str() {
        let t: String = t.chars().take(200).collect();
        if !t.trim().is_empty() {
            parts.push(format!("选中的文字：「{}」", t.trim()));
        }
    }
    if let Some(sh) = v["sheet"].as_str() {
        parts.push(format!("当前工作表：{sh}"));
    }
    if let Some(r) = v["range"].as_str() {
        parts.push(format!("选中区域：{r}"));
    }
    if v["saved"] == false {
        parts.push("有未保存的修改".into());
    }
    let others: Vec<String> = v["others"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if !others.is_empty() {
        parts.push(format!("同时还开着另外 {} 份：{}。这些不是用户正在用的，除非用户点名，不要动", others.len(), others.iter().map(|o| format!("`{o}`")).collect::<Vec<_>>().join("、")));
    }
    let shown = if path.is_empty() { "（还没保存过的新文档）".to_string() } else { format!("`{path}`") };
    Some(format!("- 前台是 {prog}，用户正在编辑的文档：{shown}（{}）\n", parts.join("；")))
}