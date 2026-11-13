//! 全局"按住说话"热键：键盘组合键（如 "Ctrl+Alt+Space"）或鼠标侧键（"Mouse:XButton1/2"）。
//! Windows：WH_KEYBOARD_LL / WH_MOUSE_LL 底层钩子，装在主线程（复用 Tauri 主事件循环的消息泵）。
//! macOS：CGEventTap，跑在专门的线程里（需要「辅助功能」权限）；macOS 键码先换算成 Windows 的 VK 码，
//!        之后的判定逻辑（key_logic / mouse_logic）两边共用。配置里的 "Win" 在 macOS 上写作 "Cmd"，"Alt" 就是 Option。
//! 钩子回调是裸 C 函数指针，状态全部走进程级静态变量。
//!
//! 相比 PromptPop 的版本做了两处加固：
//! 1. 忽略"注入事件"(LLKHF_INJECTED)：我们自己为了抓选区而模拟的 Ctrl+C / 临时松开 Alt，
//!    不能被当成用户真的松开了热键。
//! 2. 热键主键(如 Space)被"吞掉"：否则按住时系统自动连发会往前台程序里打出一串空格。

use std::sync::atomic::{AtomicBool, Ordering};
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
#[cfg(windows)]
static KB_HOOK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
#[cfg(windows)]
static MOUSE_HOOK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

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
            "Win" | "Cmd" | "Meta" | "Super" => win = true,
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
    if MOD_WIN.load(Ordering::Relaxed) { p.push(if cfg!(target_os = "macos") { "Cmd" } else { "Win" }.into()); }
    p.push(vk_to_name(vk).unwrap_or_else(|| format!("VK{vk}")));
    p.join("+")
}

fn fire_down() {
    if PAUSED.load(Ordering::Relaxed) {
        HELD.store(false, Ordering::SeqCst);
        return;
    }
    crate::log::log("hotkey: down");
    if let Some(app) = APP.get() {
        crate::session::on_down(app.clone());
    }
    spawn_watchdog();
}
fn fire_up() {
    crate::log::log("hotkey: up");
    if let Some(app) = APP.get() {
        crate::session::on_up(app.clone());
    }
}

/// 热键里的**修饰键**（Ctrl/Alt/Shift/Win）现在是否都还按着（GetAsyncKeyState）。
/// 注意：主键（如 Space）和鼠标侧键被我们的钩子吞掉了（return 1），系统根本不会记录它们的按下状态，
/// GetAsyncKeyState 永远返回"没按"——所以只能查没被吞的修饰键；没有修饰键的热键不做兜底（靠 60 秒录音上限）。
fn physically_down() -> bool {
    #[cfg(windows)]
    let (c, a, s, w) = {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        let d = |vk: i32| unsafe { (GetAsyncKeyState(vk) as u16) & 0x8000 != 0 };
        (d(0x11), d(0x12), d(0x10), d(0x5B) || d(0x5C))
    };
    #[cfg(target_os = "macos")]
    let (c, a, s, w) = {
        use crate::mac::{modifier_flags, FLAG_ALT, FLAG_CMD, FLAG_CTRL, FLAG_SHIFT};
        let f = modifier_flags();
        (f & FLAG_CTRL != 0, f & FLAG_ALT != 0, f & FLAG_SHIFT != 0, f & FLAG_CMD != 0)
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let (c, a, s, w) = (true, true, true, true);
    match spec_cell().lock().ok().and_then(|g| g.clone()) {
        Some(Spec::Keyboard { ctrl, alt, shift, win, .. }) => (!ctrl || c) && (!alt || a) && (!shift || s) && (!win || w),
        _ => true,
    }
}


/// 兜底：低级钩子偶尔会漏掉"松开"事件（钩子超时、锁屏/UAC 期间松键等），录音就会一直不停。
/// 按住期间每 100ms 看一次修饰键，连续 500ms 都没按着就当作已松开。
/// （抓选区时我们会临时模拟松开 Alt 等，不到 500ms，不会误判。）
fn spawn_watchdog() {
    std::thread::spawn(|| {
        let mut up_since: Option<std::time::Instant> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if !HELD.load(Ordering::SeqCst) {
                return;
            }
            if physically_down() {
                up_since = None;
            } else {
                let t = *up_since.get_or_insert_with(std::time::Instant::now);
                if t.elapsed() > std::time::Duration::from_millis(500) {
                    if HELD.swap(false, Ordering::SeqCst) {
                        crate::log::log("hotkey: watchdog — missed key-up, releasing");
                        MAIN_DOWN.store(false, Ordering::Relaxed);
                        fire_up();
                    }
                    return;
                }
            }
        }
    });
}

/// 键盘事件的判定（两个平台共用）。调用前修饰键状态（MOD_*）必须已经更新好。
/// vk 是 Windows 虚拟键码；返回 true = 吞掉这个按键，不让前台程序收到。
fn key_logic(vk: u32, down: bool) -> bool {
    // 聆听 / 转写 / 执行中按 Esc：取消整个任务（此时面板不抢焦点，收不到按键，所以放在全局钩子里）
    if down && vk == 0x1B && crate::ui::esc_cancellable() {
        if let Some(app) = APP.get() {
            let app = app.clone();
            std::thread::spawn(move || crate::ui::panel_hide(app));
        }
        return true;
    }

    let recording = record_cell().lock().ok().and_then(|g| g.clone());
    if let Some(tx) = recording {
        if down && !is_modifier_vk(vk) {
            let spec = build_kb_spec(vk);
            if let Ok(mut g) = record_cell().lock() {
                *g = None;
            }
            let _ = tx.send(spec);
            return true;
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
        return swallow;
    }
    false
}

/// 鼠标侧键的判定（button：1 = 后退键，2 = 前进键）。返回 true = 吞掉。
fn mouse_logic(button: u16, down: bool) -> bool {
    let recording = record_cell().lock().ok().and_then(|g| g.clone());
    if let Some(tx) = recording {
        if down {
            let spec = format!("Mouse:{}", if button == 1 { "XButton1" } else { "XButton2" });
            if let Ok(mut g) = record_cell().lock() {
                *g = None;
            }
            let _ = tx.send(spec);
            return true;
        }
    } else if matches!(spec_cell().lock().ok().and_then(|g| g.clone()), Some(Spec::Mouse(b)) if b == button) {
        if down {
            if !HELD.swap(true, Ordering::SeqCst) {
                fire_down();
            }
        } else if HELD.swap(false, Ordering::SeqCst) {
            fire_up();
        }
        return true;
    }
    false
}

// ───────────────────────── Windows：低级钩子 ─────────────────────────

#[cfg(windows)]
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
        if key_logic(vk, down) {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

#[cfg(windows)]
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
            if mouse_logic(button, msg == WM_XBUTTONDOWN) {
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

// ───────────────────────── macOS：CGEventTap ─────────────────────────

/// macOS 虚拟键码（按 ANSI 键盘的物理位置）→ Windows VK 码
#[cfg(target_os = "macos")]
fn mac_vk(keycode: u16) -> Option<u32> {
    const LETTERS: [(u16, char); 36] = [
        (0, 'A'), (1, 'S'), (2, 'D'), (3, 'F'), (4, 'H'), (5, 'G'), (6, 'Z'), (7, 'X'), (8, 'C'), (9, 'V'), (11, 'B'), (12, 'Q'),
        (13, 'W'), (14, 'E'), (15, 'R'), (16, 'Y'), (17, 'T'), (18, '1'), (19, '2'), (20, '3'), (21, '4'), (22, '6'), (23, '5'), (25, '9'),
        (26, '7'), (28, '8'), (29, '0'), (31, 'O'), (32, 'U'), (34, 'I'), (35, 'P'), (37, 'L'), (38, 'J'), (40, 'K'), (45, 'N'), (46, 'M'),
    ];
    const FKEYS: [u16; 20] = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79, 80, 90];
    if let Some((_, c)) = LETTERS.iter().find(|(k, _)| *k == keycode) {
        return Some(*c as u32);
    }
    if let Some(i) = FKEYS.iter().position(|k| *k == keycode) {
        return Some(0x70 + i as u32);
    }
    match keycode {
        49 => Some(0x20), // Space
        36 => Some(0x0D), // Return
        48 => Some(0x09), // Tab
        50 => Some(0xC0), // `
        53 => Some(0x1B), // Esc
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn on_tap(ev: crate::mac::TapEvent) -> bool {
    use crate::mac::{TapEvent, FLAG_ALT, FLAG_CMD, FLAG_CTRL, FLAG_SHIFT};
    let set_mods = |f: u64| {
        MOD_CTRL.store(f & FLAG_CTRL != 0, Ordering::Relaxed);
        MOD_ALT.store(f & FLAG_ALT != 0, Ordering::Relaxed);
        MOD_SHIFT.store(f & FLAG_SHIFT != 0, Ordering::Relaxed);
        MOD_WIN.store(f & FLAG_CMD != 0, Ordering::Relaxed);
    };
    match ev {
        TapEvent::Key { keycode, down, flags } => {
            set_mods(flags);
            match mac_vk(keycode) {
                Some(vk) => key_logic(vk, down),
                None => false,
            }
        }
        TapEvent::Flags { flags } => {
            set_mods(flags);
            // 修饰键变了：让共用逻辑重新判断"热键是否还按着"（传任意一个修饰键的 VK 即可）
            key_logic(0x11, false);
            false
        }
        TapEvent::Mouse { button, down } => match button {
            3 => mouse_logic(1, down),
            4 => mouse_logic(2, down),
            _ => false,
        },
    }
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
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL};
        if let Ok(h) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), None, 0) {
            KB_HOOK.store(h.0 as isize, Ordering::Relaxed);
        }
        if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) {
            MOUSE_HOOK.store(h.0 as isize, Ordering::Relaxed);
        }
    }
    #[cfg(target_os = "macos")]
    {
        // 没有「辅助功能」权限就弹系统提示；监听线程会一直重试，用户勾选后自动生效，不用重启
        if !crate::mac::ax_trusted() {
            crate::mac::ax_prompt();
        }
        std::thread::spawn(|| crate::mac::run_event_tap(on_tap, crate::log::log));
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
