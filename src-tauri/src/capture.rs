//! 抓取"用户当前面对的东西"：前台窗口信息、选中文字/文件、截屏、把文字粘回原程序。
//!
//! 选区抓取 = 模拟 Ctrl+C 再读剪贴板（文件 = CF_HDROP，文字 = CF_UNICODETEXT），然后还原剪贴板。
//! 关键细节：
//! 1. 用户此刻正按着热键修饰键（Ctrl/Alt/Shift/Win）。直接发 Ctrl+C 会变成 Ctrl+Alt+C。
//!    所以先临时"松开"Alt/Shift/Win（注入 keyup），发完再"按回去"，让系统状态和物理状态一致。
//! 2. 终端里 Ctrl+C 是 SIGINT，会打断正在跑的程序 → 终端类程序改用 Ctrl+Insert（同样是复制，不发信号）。
//! 3. 用剪贴板序列号判断"有没有复制成功"，而不是靠哨兵值，这样不会破坏用户原剪贴板。

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FgInfo {
    pub hwnd: isize,
    pub pid: u32,
    pub title: String,
    pub exe: String,
}

#[derive(Clone, Debug, Default)]
pub enum Selection {
    #[default]
    None,
    Text(String),
    Files(Vec<String>),
}

pub fn cursor_xy() -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

pub fn foreground() -> FgInfo {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId};
    use windows::core::PWSTR;
    unsafe {
        let h = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(h, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let mut exe = String::new();
        if let Ok(ph) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let mut pbuf = [0u16; 520];
            let mut len = pbuf.len() as u32;
            if QueryFullProcessImageNameW(ph, PROCESS_NAME_FORMAT(0), PWSTR(pbuf.as_mut_ptr()), &mut len).is_ok() {
                let full = String::from_utf16_lossy(&pbuf[..len as usize]);
                exe = full.rsplit(['\\', '/']).next().unwrap_or("").to_string();
            }
            let _ = CloseHandle(HANDLE(ph.0));
        }
        FgInfo { hwnd: h.0 as isize, pid, title, exe }
    }
}

fn is_terminal(exe: &str) -> bool {
    let e = exe.to_ascii_lowercase();
    [
        "windowsterminal.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "conhost.exe", "openconsole.exe",
        "wezterm-gui.exe", "alacritty.exe", "mintty.exe", "putty.exe", "kitty.exe", "tabby.exe", "hyper.exe",
    ]
    .contains(&e.as_str())
}

// ───────────────────────── SendInput ─────────────────────────

fn key_input(vk: u16, up: bool) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_keys(seq: &[(u16, bool)]) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT};
    let inputs: Vec<INPUT> = seq.iter().map(|(vk, up)| key_input(*vk, *up)).collect();
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn key_down(vk: i32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    unsafe { (GetAsyncKeyState(vk) as u16) & 0x8000 != 0 }
}

const VK_CONTROL: u16 = 0x11;
const VK_C: u16 = 0x43;
const VK_V: u16 = 0x56;
const VK_INSERT: u16 = 0x2D;

/// 发一次"复制"，期间临时松开 Alt/Shift/Win
fn send_copy(terminal: bool) {
    let ctrl_down = key_down(VK_CONTROL as i32);
    let mut released: Vec<u16> = vec![];
    for vk in [0xA4u16, 0xA5, 0xA0, 0xA1, 0x5B, 0x5C] {
        if key_down(vk as i32) {
            released.push(vk);
        }
    }
    let rel: Vec<(u16, bool)> = released.iter().map(|v| (*v, true)).collect();
    send_keys(&rel);
    if !ctrl_down {
        send_keys(&[(VK_CONTROL, false)]);
    }
    let k = if terminal { VK_INSERT } else { VK_C };
    send_keys(&[(k, false), (k, true)]);
    if !ctrl_down {
        send_keys(&[(VK_CONTROL, true)]);
    }
    let rep: Vec<(u16, bool)> = released.iter().map(|v| (*v, false)).collect();
    send_keys(&rep);
}

// ───────────────────────── 剪贴板 ─────────────────────────

struct Backup(Vec<(u32, Vec<u8>)>);

fn backup_formats() -> Vec<u32> {
    use clipboard_win::raw::register_format;
    let mut v = vec![clipboard_win::formats::CF_UNICODETEXT, clipboard_win::formats::CF_HDROP, clipboard_win::formats::CF_DIB];
    for n in ["Preferred DropEffect", "HTML Format", "Rich Text Format"] {
        if let Some(id) = register_format(n) {
            v.push(id.get());
        }
    }
    v
}

fn clipboard_backup() -> Backup {
    let mut out = vec![];
    if let Ok(_c) = clipboard_win::Clipboard::new_attempts(10) {
        for f in backup_formats() {
            if clipboard_win::raw::is_format_avail(f) {
                let mut buf = vec![];
                if clipboard_win::raw::get_vec(f, &mut buf).is_ok() && !buf.is_empty() {
                    out.push((f, buf));
                }
            }
        }
    }
    Backup(out)
}

fn clipboard_restore(b: &Backup) {
    if let Ok(_c) = clipboard_win::Clipboard::new_attempts(10) {
        let _ = clipboard_win::raw::empty();
        for (f, data) in &b.0 {
            let _ = clipboard_win::raw::set_without_clear(*f, data);
        }
    }
}

fn seq() -> u32 {
    clipboard_win::raw::seq_num().map(|n| n.get()).unwrap_or(0)
}

/// 抓取选区。返回 Selection；没选中任何东西（剪贴板序列号没变）返回 None。
pub fn grab_selection(fg: &FgInfo) -> Selection {
    let before = seq();
    let backup = clipboard_backup();
    send_copy(is_terminal(&fg.exe));

    let t0 = Instant::now();
    let mut changed = false;
    while t0.elapsed() < Duration::from_millis(700) {
        std::thread::sleep(Duration::from_millis(15));
        if seq() != before {
            changed = true;
            // 给应用一点时间把多种格式都写完
            std::thread::sleep(Duration::from_millis(40));
            break;
        }
    }
    if !changed {
        return Selection::None;
    }

    let files: Vec<String> = clipboard_win::get_clipboard(clipboard_win::formats::FileList).unwrap_or_default();
    let sel = if !files.is_empty() {
        Selection::Files(files)
    } else {
        match clipboard_win::get_clipboard_string() {
            Ok(t) if !t.trim().is_empty() => Selection::Text(t.trim_end_matches('\0').to_string()),
            _ => Selection::None,
        }
    };

    // 还原用户原来的剪贴板
    if backup.0.is_empty() {
        if let Ok(_c) = clipboard_win::Clipboard::new_attempts(10) {
            let _ = clipboard_win::raw::empty();
        }
    } else {
        clipboard_restore(&backup);
    }
    sel
}

/// 把文字粘回原程序（替换选区）
pub fn paste_back(hwnd: isize, text: &str) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    clipboard_win::set_clipboard_string(text).map_err(|e| format!("写剪贴板失败: {e}"))?;
    unsafe {
        if hwnd != 0 {
            let _ = SetForegroundWindow(HWND(hwnd as *mut core::ffi::c_void));
        }
    }
    std::thread::sleep(Duration::from_millis(180));
    send_keys(&[(VK_CONTROL, false), (VK_V, false), (VK_V, true), (VK_CONTROL, true)]);
    Ok(())
}

// ───────────────────────── 截屏 ─────────────────────────

/// 截"源程序窗口"，失败则截鼠标所在显示器。保存为 PNG（长边最多 1920），返回路径。
/// 抓图（前台窗口，退而求其次整个光标所在显示器），长边缩到 1920；只抓不存，方便按热键时抢在弹窗出现前完成
pub fn grab_image(fg_hwnd: isize, cursor: (i32, i32)) -> Result<image::RgbaImage, String> {
    let mut img = None;
    if fg_hwnd != 0 {
        if let Ok(wins) = xcap::Window::all() {
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

const PPT_SCRIPT: &str = r#"
[Console]::OutputEncoding=[Text.Encoding]::UTF8
$ErrorActionPreference='Stop'
try{ $app=[Runtime.InteropServices.Marshal]::GetActiveObject('PowerPoint.Application') }catch{ exit 0 }
if($app.Presentations.Count -lt 1){ exit 0 }
# 同时开着多份：用前台窗口标题里的文件名挑出用户正在用的那份（取名字最长的匹配），对不上再用活动文档
$title=[string]$env:SAYDO_FG_TITLE; $pres=$null; $best=0
foreach($x in $app.Presentations){ $base=[IO.Path]::GetFileNameWithoutExtension([string]$x.Name); if($title -and $base.Length -gt $best -and $title.Contains($base)){ $pres=$x; $best=$base.Length } }
if(-not $pres){ $pres=$app.ActivePresentation }
$o=[ordered]@{path=$pres.FullName;saved=[bool]$pres.Saved;slides=[int]$pres.Slides.Count}
$o.others=@($app.Presentations | Where-Object { $_.FullName -ne $pres.FullName } | ForEach-Object { $_.FullName })
try{ $w=$pres.Windows.Item(1); $o.slide=[int]$w.View.Slide.SlideIndex
  $sel=$w.Selection
  if($sel.Type -eq 2){ $o.shapes=@($sel.ShapeRange | ForEach-Object { $_.Name }) }
  if($sel.Type -eq 3){ $o.text=[string]$sel.TextRange.Text } }catch{}
$o | ConvertTo-Json -Compress
"#;

const WORD_SCRIPT: &str = r#"
[Console]::OutputEncoding=[Text.Encoding]::UTF8
$ErrorActionPreference='Stop'
try{ $app=[Runtime.InteropServices.Marshal]::GetActiveObject('Word.Application') }catch{ exit 0 }
if($app.Documents.Count -lt 1){ exit 0 }
$title=[string]$env:SAYDO_FG_TITLE; $d=$null; $best=0
foreach($x in $app.Documents){ $base=[IO.Path]::GetFileNameWithoutExtension([string]$x.Name); if($title -and $base.Length -gt $best -and $title.Contains($base)){ $d=$x; $best=$base.Length } }
if(-not $d){ $d=$app.ActiveDocument }
$o=[ordered]@{path=$d.FullName;saved=[bool]$d.Saved}
$o.others=@($app.Documents | Where-Object { $_.FullName -ne $d.FullName } | ForEach-Object { $_.FullName })
try{ $o.pages=[int]$d.ComputeStatistics(2) }catch{}
try{ $t=[string]$d.ActiveWindow.Selection.Text; if($t.Trim().Length -gt 0){ $o.text=$t } }catch{}
$o | ConvertTo-Json -Compress
"#;

const EXCEL_SCRIPT: &str = r#"
[Console]::OutputEncoding=[Text.Encoding]::UTF8
$ErrorActionPreference='Stop'
try{ $app=[Runtime.InteropServices.Marshal]::GetActiveObject('Excel.Application') }catch{ exit 0 }
if($app.Workbooks.Count -lt 1){ exit 0 }
$title=[string]$env:SAYDO_FG_TITLE; $b=$null; $best=0
foreach($x in $app.Workbooks){ $base=[IO.Path]::GetFileNameWithoutExtension([string]$x.Name); if($title -and $base.Length -gt $best -and $title.Contains($base)){ $b=$x; $best=$base.Length } }
if(-not $b){ $b=$app.ActiveWorkbook }
$o=[ordered]@{path=$b.FullName;saved=[bool]$b.Saved}
$o.others=@($app.Workbooks | Where-Object { $_.FullName -ne $b.FullName } | ForEach-Object { $_.FullName })
try{ $o.sheet=[string]$b.ActiveSheet.Name }catch{}
try{ if($b.FullName -eq $app.ActiveWorkbook.FullName){ $o.range=[string]$app.Selection.Address($false,$false) } }catch{}
$o | ConvertTo-Json -Compress
"#;

/// 前台是 PowerPoint / Word / Excel 时，问一下它"现在打开的是哪份文档、当前页 / 选区是什么"，
/// 让 Agent 知道"这个 PPT / 这份文档"指的是哪一份，直接改它而不是另做一个新文件。
/// 返回一行可直接放进提示词的描述；没有可用文档 / 超时返回 None。（在按下热键时并行取，不增加等待）
pub fn office_context(exe: &str, fg_title: &str) -> Option<String> {
    use base64::Engine;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let (prog, script) = match exe.to_lowercase().as_str() {
        "powerpnt.exe" => ("PowerPoint", PPT_SCRIPT),
        "winword.exe" => ("Word", WORD_SCRIPT),
        "excel.exe" => ("Excel", EXCEL_SCRIPT),
        _ => return None,
    };
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let enc = base64::engine::general_purpose::STANDARD.encode(bytes);
    let child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &enc])
        .env("SAYDO_FG_TITLE", fg_title)
        .creation_flags(0x0800_0000)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = match rx.recv_timeout(std::time::Duration::from_secs(6)) {
        Ok(Ok(o)) => o,
        _ => {
            let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(0x0800_0000).output();
            return None;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
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
