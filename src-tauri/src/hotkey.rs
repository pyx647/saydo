//! 全局"按住说话"热键：键盘组合键（如 "Ctrl+Alt+Space"）或鼠标侧键（"Mouse:XButton1/2"）。
//! 用 WH_KEYBOARD_LL / WH_MOUSE_LL 底层钩子，装在主线程（复用 Tauri 主事件循环的消息泵）。
//! 钩子回调是裸 C 函数指针，状态全部走进程级静态变量。
//!
//! 相比 PromptPop 的版本做了两处加固：
//! 1. 忽略"注入事件"(LLKHF_INJECTED)：我们自己为了抓选区而模拟的 Ctrl+C / 临时松开 Alt，
//!    不能被当成用户真的松开了热键。
//! 2. 热键主键(如 Space)被"吞掉"：否则按住时系统自动连发会往前台程序里打出一串空格。

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::AppHandle;

#[derive(Clone, Debug, PartialEq)]
enum Spec {
    Mouse(u16),
    Keyboard { ctrl: bool, alt: bool, shift: bool, win: bool, vk: u32 },
}

static SPEC: OnceLock<Mutex<Option<Spec>>> = OnceLock::new();
fn spec_cell() -> &'static Mutex<Option<Spec>> {
    SPEC.get_or_init(|| Mutex::new(None))
}

static HELD: AtomicBool = AtomicBool::new(false);
static MAIN_DOWN: AtomicBool = AtomicBool::new(false);
static MOD_CTRL: AtomicBool = AtomicBool::new(false);
static MOD_ALT: AtomicBool = AtomicBool::new(false);
static MOD_SHIFT: AtomicBool = AtomicBool::new(false);
static MOD_WIN: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);

static RECORD_TX: OnceLock<Mutex<Option<std::sync::mpsc::Sender<String>>>> = OnceLock::new();
fn record_cell() -> &'static Mutex<Option<std::sync::mpsc::Sender<String>>> {
    RECORD_TX.get_or_init(|| Mutex::new(None))
}

static APP: OnceLock<AppHandle> = OnceLock::new();
static KB_HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);

fn vk_to_name(vk: u32) -> Option<String> {
    Some(match vk {
        0x30..=0x39 | 0x41..=0x5A => ((vk as u8) as char).to_string(),
        0x20 => "Space".into(),
        0x0D => "Enter".into(),
        0x09 => "Tab".into(),
        0xC0 => "`".into(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        _ => return None,
    })
}

fn name_to_vk(name: &str) -> Option<u32> {
    match name {
        "Space" => Some(0x20),
        "Enter" => Some(0x0D),
        "Tab" => Some(0x09),
        "`" => Some(0xC0),
        s if s.len() == 1 => {
            let c = s.chars().next()?.to_ascii_uppercase();
            c.is_ascii_alphanumeric().then_some(c as u32)
        }
        s if s.starts_with('F') => {
            let n: u32 = s[1..].parse().ok()?;
            (1..=24).contains(&n).then_some(0x70 + n - 1)
        }
        _ => None,
    }
}

fn is_modifier_vk(vk: u32) -> bool {
    matches!(vk, 0x10 | 0xA0 | 0xA1 | 0x11 | 0xA2 | 0xA3 | 0x12 | 0xA4 | 0xA5 | 0x5B | 0x5C)
}

fn parse_spec(s: &str) -> Option<Spec> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix("Mouse:") {
        return match rest {
            "XButton1" => Some(Spec::Mouse(1)),
            "XButton2" => Some(Spec::Mouse(2)),
            _ => None,
        };
    }
    let parts: Vec<&str> = s.split('+').collect();
    let (mods, key) = parts.split_at(parts.len() - 1);
    let (mut ctrl, mut alt, mut shift, mut win) = (false, false, false, false);
    for m in mods {
        match *m {
            "Ctrl" => ctrl = true,
            "Alt" => alt = true,
            "Shift" => shift = true,
            "Win" => win = true,
            _ => {}
        }
    }
    Some(Spec::Keyboard { ctrl, alt, shift, win, vk: name_to_vk(key.first()?)? })
}

fn build_kb_spec(vk: u32) -> String {
    let mut p: Vec<String> = vec![];
    if MOD_CTRL.load(Ordering::Relaxed) { p.push("Ctrl".into()); }
    if MOD_ALT.load(Ordering::Relaxed) { p.push("Alt".into()); }
    if MOD_SHIFT.load(Ordering::Relaxed) { p.push("Shift".into()); }
    if MOD_WIN.load(Ordering::Relaxed) { p.push("Win".into()); }
    p.push(vk_to_name(vk).unwrap_or_else(|| format!("VK{vk}")));
    p.join("+")
}

fn fire_down() {
    if PAUSED.load(Ordering::Relaxed) {
        HELD.store(false, Ordering::SeqCst);
        return;
    }
    if let Some(app) = APP.get() {
        crate::session::on_down(app.clone());
    }
}
fn fire_up() {
    if let Some(app) = APP.get() {
        crate::session::on_up(app.clone());
    }
}

unsafe extern "system" fn kb_proc(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, KBDLLHOOKSTRUCT, WM_KEYDOWN, WM_SYSKEYDOWN};
    if code >= 0 {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        // 忽略注入事件（我们自己模拟的按键）
        if kb.flags.0 & 0x10 != 0 && std::env::var("SAYDO_TEST").as_deref() != Ok("1") {
            return CallNextHookEx(None, code, wparam, lparam);
        }
        let vk = kb.vkCode;
        let msg = wparam.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        match vk {
            0x11 | 0xA2 | 0xA3 => MOD_CTRL.store(down, Ordering::Relaxed),
            0x12 | 0xA4 | 0xA5 => MOD_ALT.store(down, Ordering::Relaxed),
            0x10 | 0xA0 | 0xA1 => MOD_SHIFT.store(down, Ordering::Relaxed),
            0x5B | 0x5C => MOD_WIN.store(down, Ordering::Relaxed),
            _ => {}
        }

        // 聆听 / 转写 / 执行中按 Esc：取消整个任务（此时面板不抢焦点，收不到按键，所以放在全局钩子里）
        if down && vk == 0x1B && crate::ui::esc_cancellable() {
            if let Some(app) = APP.get() {
                let app = app.clone();
                std::thread::spawn(move || crate::ui::panel_hide(app));
            }
            return LRESULT(1);
        }

        let recording = record_cell().lock().ok().and_then(|g| g.clone());
        if let Some(tx) = recording {
            if down && !is_modifier_vk(vk) {
                let spec = build_kb_spec(vk);
                if let Ok(mut g) = record_cell().lock() {
                    *g = None;
                }
                let _ = tx.send(spec);
                return LRESULT(1);
            }
        } else if let Some(Spec::Keyboard { ctrl, alt, shift, win, vk: target }) = spec_cell().lock().ok().and_then(|g| g.clone()) {
            let mods_ok = MOD_CTRL.load(Ordering::Relaxed) == ctrl
                && MOD_ALT.load(Ordering::Relaxed) == alt
                && MOD_SHIFT.load(Ordering::Relaxed) == shift
                && MOD_WIN.load(Ordering::Relaxed) == win;
            let was_held = HELD.load(Ordering::SeqCst);
            let mut swallow = false;
            if vk == target {
                swallow = (down && mods_ok) || (!down && was_held);
                MAIN_DOWN.store(down, Ordering::Relaxed);
            }
            if vk == target || is_modifier_vk(vk) {
                let satisfied = MAIN_DOWN.load(Ordering::Relaxed) && mods_ok;
                if satisfied && !was_held {
                    HELD.store(true, Ordering::SeqCst);
                    fire_down();
                } else if !satisfied && was_held {
                    HELD.store(false, Ordering::SeqCst);
                    fire_up();
                }
            }
            if swallow {
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, MSLLHOOKSTRUCT, WM_XBUTTONDOWN, WM_XBUTTONUP};
    if code >= 0 {
        let msg = wparam.0 as u32;
        if msg == WM_XBUTTONDOWN || msg == WM_XBUTTONUP {
            let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            let button = ((ms.mouseData >> 16) & 0xFFFF) as u16;
            let down = msg == WM_XBUTTONDOWN;
            let recording = record_cell().lock().ok().and_then(|g| g.clone());
            if let Some(tx) = recording {
                if down {
                    let spec = format!("Mouse:{}", if button == 1 { "XButton1" } else { "XButton2" });
                    if let Ok(mut g) = record_cell().lock() {
                        *g = None;
                    }
                    let _ = tx.send(spec);
                    return LRESULT(1);
                }
            } else if matches!(spec_cell().lock().ok().and_then(|g| g.clone()), Some(Spec::Mouse(b)) if b == button) {
                if down {
                    if !HELD.swap(true, Ordering::SeqCst) {
                        fire_down();
                    }
                } else if HELD.swap(false, Ordering::SeqCst) {
                    fire_up();
                }
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

pub fn set_spec(spec: &str) {
    *spec_cell().lock().unwrap() = parse_spec(spec);
    HELD.store(false, Ordering::SeqCst);
}

pub fn set_paused(p: bool) {
    PAUSED.store(p, Ordering::Relaxed);
}
pub fn is_paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

pub fn install(app: &AppHandle) {
    let _ = APP.set(app.clone());
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL};
        if let Ok(h) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), None, 0) {
            KB_HOOK.store(h.0 as isize, Ordering::Relaxed);
        }
        if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) {
            MOUSE_HOOK.store(h.0 as isize, Ordering::Relaxed);
        }
    }
}

#[tauri::command]
pub async fn record_hotkey() -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    *record_cell().lock().unwrap() = Some(tx);
    let r = tokio::task::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(8)))
        .await
        .map_err(|e| e.to_string())?;
    *record_cell().lock().unwrap() = None;
    r.map_err(|_| "超时：8 秒内没检测到按键或鼠标侧键".to_string())
}
