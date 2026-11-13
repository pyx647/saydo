//! macOS 实现。
//!
//! 选区抓取分两步：
//! 1. 先用辅助功能 API 直接问前台程序"选中的文字"——不碰剪贴板，什么都没选时也不会"咚"一声。
//! 2. 问不到（访达里选的文件、Chrome / Electron 类程序）再模拟 ⌘C 读剪贴板，读完把原剪贴板还原。
//!    注入的 ⌘C 显式只带 Command，所以用户还按着 Option / Control（热键）也不会变成别的组合键；
//!    终端里 ⌘C 就是复制，不像 Windows 的 Ctrl+C 会发 SIGINT。

use super::{FgInfo, Selection};
use crate::mac;
use std::time::{Duration, Instant};

pub fn cursor_xy() -> (i32, i32) {
    mac::cursor()
}

pub fn foreground() -> FgInfo {
    match mac::frontmost() {
        Some(f) => FgInfo { hwnd: f.pid as isize, pid: f.pid.max(0) as u32, title: mac::window_title(f.pid), exe: f.name, bundle: f.bundle },
        None => FgInfo::default(),
    }
}

/// 抓取选区。没选中任何东西返回 None。
pub fn grab_selection(fg: &FgInfo) -> Selection {
    if fg.pid != 0 && fg.bundle != "com.apple.finder" {
        if let Some(t) = mac::selected_text(fg.pid as i32) {
            return Selection::Text(t);
        }
    }
    if !mac::ax_trusted() {
        return Selection::None; // 没有辅助功能权限，发不出 ⌘C
    }
    let before = mac::pb_change_count();
    let backup = mac::pb_backup();
    mac::send_copy();

    let t0 = Instant::now();
    let mut changed = false;
    while t0.elapsed() < Duration::from_millis(700) {
        std::thread::sleep(Duration::from_millis(15));
        if mac::pb_change_count() != before {
            changed = true;
            // 给应用一点时间把多种格式都写完
            std::thread::sleep(Duration::from_millis(40));
            break;
        }
    }
    if !changed {
        return Selection::None;
    }
    let files = mac::pb_files();
    let sel = if !files.is_empty() {
        Selection::Files(files)
    } else {
        match mac::pb_string() {
            Some(t) if !t.trim().is_empty() => Selection::Text(t),
            _ => Selection::None,
        }
    };
    // 还原用户原来的剪贴板
    mac::pb_restore(&backup);
    sel
}

/// 把文字粘回原程序（替换选区）。hwnd 在 macOS 上是原程序的进程号。
pub fn paste_back(hwnd: isize, text: &str) -> Result<(), String> {
    if !mac::ax_trusted() {
        return Err("需要「辅助功能」权限才能替换原文（系统设置 → 隐私与安全性 → 辅助功能）".into());
    }
    if !mac::pb_set_string(text) {
        return Err("写剪贴板失败".into());
    }
    if hwnd > 0 {
        mac::activate(hwnd as i32);
    }
    std::thread::sleep(Duration::from_millis(250));
    mac::send_paste();
    Ok(())
}

// ───────────────────────── 前台打开的是哪份文档（AppleScript） ─────────────────────────

/// (程序名, [(字段, AppleScript 表达式)])。每个表达式单独 `run script`，某一条在这个版本的软件里不支持也不影响其它。
fn doc_queries(bundle: &str) -> Option<(&'static str, Vec<(&'static str, &'static str)>)> {
    Some(match bundle {
        "com.microsoft.Powerpoint" => (
            "PowerPoint",
            vec![
                ("path", "full name of active presentation"),
                ("saved", "saved of active presentation"),
                ("slides", "count of slides of active presentation"),
                ("slide", "slide index of slide of view of active window"),
                ("others", "full name of every presentation"),
            ],
        ),
        "com.microsoft.Word" => (
            "Word",
            vec![
                ("path", "full name of active document"),
                ("saved", "saved of active document"),
                ("text", "content of text object of selection"),
                ("others", "full name of every document"),
            ],
        ),
        "com.microsoft.Excel" => (
            "Excel",
            vec![
                ("path", "full name of active workbook"),
                ("saved", "saved of active workbook"),
                ("sheet", "name of active sheet"),
                ("range", "get address of selection"),
                ("others", "full name of every workbook"),
            ],
        ),
        "com.apple.iWork.Keynote" | "com.apple.Keynote" => (
            "Keynote",
            vec![
                ("path", "POSIX path of ((file of front document) as alias)"),
                ("modified", "modified of front document"),
                ("slides", "count of slides of front document"),
                ("slide", "slide number of current slide of front document"),
            ],
        ),
        "com.apple.iWork.Pages" | "com.apple.Pages" => ("Pages", vec![("path", "POSIX path of ((file of front document) as alias)"), ("modified", "modified of front document")]),
        "com.apple.iWork.Numbers" | "com.apple.Numbers" => (
            "Numbers",
            vec![
                ("path", "POSIX path of ((file of front document) as alias)"),
                ("modified", "modified of front document"),
                ("sheet", "name of active sheet of front document"),
            ],
        ),
        _ => return None,
    })
}

/// 前台是 PowerPoint / Word / Excel / Keynote / Pages / Numbers 时，问它"现在打开的是哪份文档、当前页 / 选区是什么"。
/// 第一次会弹"SayDo 想要控制 xxx"的系统授权。没有可用文档 / 超时返回 None。
pub fn office_context(fg: &FgInfo) -> Option<String> {
    use std::process::{Command, Stdio};
    let (prog, queries) = doc_queries(&fg.bundle)?;
    let mut script = String::from(
        "on q(src)\n  try\n    set AppleScript's text item delimiters to \"||\"\n    return (run script src) as text\n  on error\n    return \"\"\n  end try\nend q\nset out to \"\"\n",
    );
    for (key, expr) in &queries {
        let src = format!("tell application id \"{}\" to {}{}", fg.bundle, if expr.starts_with("get ") { "" } else { "get " }, expr);
        script.push_str(&format!("set out to out & \"{key}\" & tab & my q(\"{}\") & linefeed\n", src.replace('\\', "\\\\").replace('"', "\\\"")));
    }
    script.push_str("return out\n");

    let child = Command::new("/usr/bin/osascript").args(["-e", &script]).stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).spawn().ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = match rx.recv_timeout(Duration::from_secs(6)) {
        Ok(Ok(o)) => o,
        _ => {
            crate::proc::kill_tree(pid);
            return None;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut v = serde_json::json!({});
    let mut path = String::new();
    let mut others: Vec<String> = vec![];
    for line in text.lines() {
        let Some((k, val)) = line.split_once('\t') else { continue };
        let val = val.trim();
        if val.is_empty() || val == "missing value" {
            continue;
        }
        match k {
            // 还没保存过的新文档，full name 只是个名字（没有目录）
            "path" => path = val.to_string(),
            "saved" => v["saved"] = serde_json::json!(val != "false"),
            "modified" => v["saved"] = serde_json::json!(val != "true"),
            "slides" | "slide" => {
                if let Ok(n) = val.parse::<i64>() {
                    v[k] = serde_json::json!(n);
                }
            }
            "others" => others = val.split("||").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            _ => v[k] = serde_json::json!(val),
        }
    }
    if path.is_empty() && v.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return None; // 什么都没问到（没有打开的文档 / 用户没授权）
    }
    others.retain(|o| *o != path);
    v["others"] = serde_json::json!(others);
    v["path"] = serde_json::json!(if path.starts_with('/') { path } else { String::new() });
    super::describe_doc(prog, &v)
}
