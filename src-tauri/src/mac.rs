//! macOS 系统接口：直接用 C / Objective-C 运行时 FFI，不额外引入 crate。
//!
//! - 全局热键：CGEventTap（需要「辅助功能」权限；能吞掉按键）
//! - 模拟 ⌘C / ⌘V：CGEventPost（同样需要「辅助功能」）
//! - 剪贴板：NSPasteboard（备份 / 还原全部格式、changeCount 判断是否复制成功）
//! - 前台程序：NSWorkspace；窗口标题 / 选中文字：Accessibility (AX) API
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::OnceLock;

type Id = *mut c_void;
type Sel = *const c_void;
type CFTypeRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "Foundation", kind = "framework")]
extern "C" {}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: CFTypeRef;
    static kCFBooleanTrue: CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFRunLoopGetCurrent() -> CFTypeRef;
    fn CFRunLoopAddSource(rl: CFTypeRef, source: CFTypeRef, mode: CFTypeRef);
    fn CFRunLoopRun();
    fn CFMachPortCreateRunLoopSource(alloc: CFTypeRef, port: CFTypeRef, order: isize) -> CFTypeRef;
}

type TapCallback = extern "C" fn(proxy: *mut c_void, ty: u32, event: CFTypeRef, info: *mut c_void) -> CFTypeRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreate(source: CFTypeRef) -> CFTypeRef;
    fn CGEventGetLocation(event: CFTypeRef) -> CGPoint;
    fn CGEventSourceCreate(state: i32) -> CFTypeRef;
    fn CGEventCreateKeyboardEvent(source: CFTypeRef, keycode: u16, down: bool) -> CFTypeRef;
    fn CGEventSetFlags(event: CFTypeRef, flags: u64);
    fn CGEventGetFlags(event: CFTypeRef) -> u64;
    fn CGEventSetIntegerValueField(event: CFTypeRef, field: u32, value: i64);
    fn CGEventGetIntegerValueField(event: CFTypeRef, field: u32) -> i64;
    fn CGEventPost(tap: u32, event: CFTypeRef);
    fn CGEventSourceFlagsState(state: i32) -> u64;
    fn CGEventSourceKeyState(state: i32, keycode: u16) -> bool;
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, cb: TapCallback, info: *mut c_void) -> CFTypeRef;
    fn CGEventTapEnable(tap: CFTypeRef, enable: bool);
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(element: CFTypeRef, attr: CFTypeRef, value: *mut CFTypeRef) -> i32;
    fn AXUIElementSetMessagingTimeout(element: CFTypeRef, seconds: f32) -> i32;
}

// ───────────────────────── Objective-C 小工具 ─────────────────────────

fn sel(name: &str) -> Sel {
    let c = CString::new(name).unwrap();
    unsafe { sel_registerName(c.as_ptr()) }
}

fn class(name: &str) -> Id {
    let c = CString::new(name).unwrap();
    unsafe { objc_getClass(c.as_ptr()) }
}

/// objc_msgSend 必须先转成"真实签名"的函数指针再调用（arm64 上不能按可变参数调）
unsafe fn send0<R>(obj: Id, s: &str) -> R {
    let f: extern "C" fn(Id, Sel) -> R = std::mem::transmute(objc_msgSend as *const c_void);
    f(obj, sel(s))
}
unsafe fn send1<A, R>(obj: Id, s: &str, a: A) -> R {
    let f: extern "C" fn(Id, Sel, A) -> R = std::mem::transmute(objc_msgSend as *const c_void);
    f(obj, sel(s), a)
}
unsafe fn send2<A, B, R>(obj: Id, s: &str, a: A, b: B) -> R {
    let f: extern "C" fn(Id, Sel, A, B) -> R = std::mem::transmute(objc_msgSend as *const c_void);
    f(obj, sel(s), a, b)
}
unsafe fn send3<A, B, C, R>(obj: Id, s: &str, a: A, b: B, c: C) -> R {
    let f: extern "C" fn(Id, Sel, A, B, C) -> R = std::mem::transmute(objc_msgSend as *const c_void);
    f(obj, sel(s), a, b, c)
}

/// 在自动释放池里跑一段代码（后台线程没有现成的池，不包会泄漏）
fn pool<R>(f: impl FnOnce() -> R) -> R {
    unsafe {
        let p = objc_autoreleasePoolPush();
        let r = f();
        objc_autoreleasePoolPop(p);
        r
    }
}

/// &str → 自动释放的 NSString
unsafe fn ns(s: &str) -> Id {
    let alloc: Id = send0(class("NSString"), "alloc");
    let obj: Id = send3(alloc, "initWithBytes:length:encoding:", s.as_ptr(), s.len(), 4usize);
    send0(obj, "autorelease")
}

/// NSString（或桥接的 CFString）→ String
unsafe fn from_ns(s: Id) -> String {
    if s.is_null() {
        return String::new();
    }
    let p: *const c_char = send0(s, "UTF8String");
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p).to_string_lossy().to_string()
}

// ───────────────────────── 权限 ─────────────────────────

pub fn ax_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// 没有「辅助功能」权限时弹系统提示（引导用户去 系统设置 → 隐私与安全性 → 辅助功能 打勾）
pub fn ax_prompt() -> bool {
    pool(|| unsafe {
        let opts: Id = send2(class("NSDictionary"), "dictionaryWithObject:forKey:", kCFBooleanTrue as Id, ns("AXTrustedCheckOptionPrompt"));
        AXIsProcessTrustedWithOptions(opts as CFTypeRef)
    })
}

/// 现在有没有「屏幕录制」权限（没有的话截图只有桌面壁纸）。授权后要重新打开应用才会变成 true。
pub fn screen_capture_allowed() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// 没有「屏幕录制」权限时请求一次（否则截图只有桌面壁纸）
pub fn request_screen_capture() {
    unsafe {
        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess();
        }
    }
}

// ───────────────────────── 光标 / 修饰键 ─────────────────────────

/// 光标位置（全局坐标，单位是"点"，原点在主屏左上角）
pub fn cursor() -> (i32, i32) {
    unsafe {
        let e = CGEventCreate(std::ptr::null());
        if e.is_null() {
            return (0, 0);
        }
        let p = CGEventGetLocation(e);
        CFRelease(e);
        (p.x as i32, p.y as i32)
    }
}

pub const FLAG_SHIFT: u64 = 1 << 17;
pub const FLAG_CTRL: u64 = 1 << 18;
pub const FLAG_ALT: u64 = 1 << 19;
pub const FLAG_CMD: u64 = 1 << 20;

/// 物理键盘此刻的修饰键状态（HID 层，不受我们吞键 / 注入的影响）
pub fn modifier_flags() -> u64 {
    unsafe { CGEventSourceFlagsState(1) }
}

/// 某个键此刻是否物理按着（HID 层：即使这个键被我们的监听吞掉了也查得到）
pub fn key_down(keycode: u16) -> bool {
    unsafe { CGEventSourceKeyState(1, keycode) }
}

// ───────────────────────── 模拟按键 ─────────────────────────

/// 我们自己注入的事件带这个标记，热键监听据此忽略
const MAGIC: i64 = 0x5A7D0;
const FIELD_USER_DATA: u32 = 42; // kCGEventSourceUserData
const FIELD_KEYCODE: u32 = 9; // kCGKeyboardEventKeycode
const FIELD_MOUSE_BUTTON: u32 = 3; // kCGMouseEventButtonNumber

const KEY_C: u16 = 8;
const KEY_V: u16 = 9;

/// 发一次 ⌘+键。显式把修饰键设成"只有 Command"，所以用户此刻还按着 Option / Control 也不影响。
fn send_cmd_key(keycode: u16) {
    unsafe {
        // 私有事件源（kCGEventSourceStatePrivate = -1）：注入的事件不会改写系统记录的"当前按着哪些修饰键"。
        // 用 HID 状态源的话，这一发"只带 Command"的按键会把用户正按着的 Option / Control 记成已松开，
        // 之后的按键事件和修饰键查询都跟着错 → 热键被误判为松开，录音不到一秒就停。
        let src = CGEventSourceCreate(-1);
        for down in [true, false] {
            let e = CGEventCreateKeyboardEvent(src, keycode, down);
            if e.is_null() {
                continue;
            }
            CGEventSetFlags(e, FLAG_CMD);
            CGEventSetIntegerValueField(e, FIELD_USER_DATA, MAGIC);
            CGEventPost(0, e); // kCGHIDEventTap
            CFRelease(e);
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        if !src.is_null() {
            CFRelease(src);
        }
    }
}

pub fn send_copy() {
    send_cmd_key(KEY_C);
}
pub fn send_paste() {
    send_cmd_key(KEY_V);
}

// ───────────────────────── 剪贴板 ─────────────────────────

const UTI_TEXT: &str = "public.utf8-plain-text";
const UTI_FILE_URL: &str = "public.file-url";

unsafe fn pasteboard() -> Id {
    send0(class("NSPasteboard"), "generalPasteboard")
}

/// 每次剪贴板内容变化都会加一：用它判断"⌘C 有没有真的复制到东西"
pub fn pb_change_count() -> isize {
    pool(|| unsafe { send0::<isize>(pasteboard(), "changeCount") })
}

/// 剪贴板完整备份：每个条目的每种格式的原始数据
pub struct PbBackup(Vec<Vec<(String, Vec<u8>)>>);

pub fn pb_backup() -> PbBackup {
    pool(|| unsafe {
        let mut out = vec![];
        let mut total = 0usize;
        let items: Id = send0(pasteboard(), "pasteboardItems");
        if items.is_null() {
            return PbBackup(out);
        }
        let n: usize = send0(items, "count");
        for i in 0..n {
            let item: Id = send1(items, "objectAtIndex:", i);
            let types: Id = send0(item, "types");
            if types.is_null() {
                continue;
            }
            let tn: usize = send0(types, "count");
            let mut one = vec![];
            for j in 0..tn {
                let ty: Id = send1(types, "objectAtIndex:", j);
                let data: Id = send1(item, "dataForType:", ty);
                if data.is_null() {
                    continue;
                }
                let len: usize = send0(data, "length");
                let ptr: *const u8 = send0(data, "bytes");
                if ptr.is_null() || len == 0 {
                    continue;
                }
                total += len;
                // 太大的内容（比如复制了一张巨图）不备份，免得卡住
                if total > 64 << 20 {
                    return PbBackup(vec![]);
                }
                one.push((from_ns(ty), std::slice::from_raw_parts(ptr, len).to_vec()));
            }
            if !one.is_empty() {
                out.push(one);
            }
        }
        PbBackup(out)
    })
}

pub fn pb_restore(b: &PbBackup) {
    pool(|| unsafe {
        let pb = pasteboard();
        let _: isize = send0(pb, "clearContents");
        if b.0.is_empty() {
            return;
        }
        let arr: Id = send0(class("NSMutableArray"), "array");
        for one in &b.0 {
            let alloc: Id = send0(class("NSPasteboardItem"), "alloc");
            let item: Id = send0(alloc, "init");
            let item: Id = send0(item, "autorelease");
            for (ty, bytes) in one {
                let data: Id = send2(class("NSData"), "dataWithBytes:length:", bytes.as_ptr(), bytes.len());
                let _: i8 = send2(item, "setData:forType:", data, ns(ty));
            }
            let _: () = send1(arr, "addObject:", item);
        }
        let _: i8 = send1(pb, "writeObjects:", arr);
    })
}

pub fn pb_string() -> Option<String> {
    pool(|| unsafe {
        let s: Id = send1(pasteboard(), "stringForType:", ns(UTI_TEXT));
        if s.is_null() { None } else { Some(from_ns(s)) }
    })
}

/// 剪贴板里的文件（在访达里 ⌘C 选中的文件 / 文件夹）
pub fn pb_files() -> Vec<String> {
    pool(|| unsafe {
        let mut out = vec![];
        let items: Id = send0(pasteboard(), "pasteboardItems");
        if items.is_null() {
            return out;
        }
        let n: usize = send0(items, "count");
        for i in 0..n {
            let item: Id = send1(items, "objectAtIndex:", i);
            let s: Id = send1(item, "stringForType:", ns(UTI_FILE_URL));
            if s.is_null() {
                continue;
            }
            let url: Id = send1(class("NSURL"), "URLWithString:", s);
            if url.is_null() {
                continue;
            }
            // filePathURL 会把 file:///.file/id=… 这种"文件引用 URL"解析成真实路径
            let fp: Id = send0(url, "filePathURL");
            if fp.is_null() {
                continue;
            }
            let path: Id = send0(fp, "path");
            let p = from_ns(path);
            if !p.is_empty() {
                out.push(p);
            }
        }
        out
    })
}

pub fn pb_set_string(text: &str) -> bool {
    pool(|| unsafe {
        let pb = pasteboard();
        let _: isize = send0(pb, "clearContents");
        let ok: i8 = send2(pb, "setString:forType:", ns(text), ns(UTI_TEXT));
        ok != 0
    })
}

// ───────────────────────── 前台程序 ─────────────────────────

pub struct Front {
    pub pid: i32,
    pub name: String,
    pub bundle: String,
}

pub fn frontmost() -> Option<Front> {
    pool(|| unsafe {
        let ws: Id = send0(class("NSWorkspace"), "sharedWorkspace");
        let app: Id = send0(ws, "frontmostApplication");
        if app.is_null() {
            return None;
        }
        let pid: i32 = send0(app, "processIdentifier");
        let name: Id = send0(app, "localizedName");
        let bundle: Id = send0(app, "bundleIdentifier");
        Some(Front { pid, name: from_ns(name), bundle: from_ns(bundle) })
    })
}

/// 把某个程序切回前台（"替换原文"粘贴之前用）
pub fn activate(pid: i32) {
    pool(|| unsafe {
        let app: Id = send1(class("NSRunningApplication"), "runningApplicationWithProcessIdentifier:", pid);
        if !app.is_null() {
            let _: i8 = send1(app, "activateWithOptions:", 2usize); // NSApplicationActivateIgnoringOtherApps
        }
    })
}

/// 读一个 AX 属性；调用方负责 CFRelease 返回值
unsafe fn ax_copy(element: CFTypeRef, attr: &str) -> Option<CFTypeRef> {
    let mut v: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, ns(attr) as CFTypeRef, &mut v);
    (err == 0 && !v.is_null()).then_some(v)
}

unsafe fn ax_string(element: CFTypeRef, attr: &str) -> Option<String> {
    let v = ax_copy(element, attr)?;
    let r = (CFGetTypeID(v) == CFStringGetTypeID()).then(|| from_ns(v as Id));
    CFRelease(v);
    r
}

/// 前台窗口标题（需要「辅助功能」权限；拿不到返回空串）
pub fn window_title(pid: i32) -> String {
    pool(|| unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return String::new();
        }
        AXUIElementSetMessagingTimeout(app, 0.3);
        let mut title = String::new();
        if let Some(win) = ax_copy(app, "AXFocusedWindow") {
            title = ax_string(win, "AXTitle").unwrap_or_default();
            CFRelease(win);
        }
        CFRelease(app);
        title
    })
}

/// 直接问前台程序"当前选中的文字"。原生 App / Safari 支持；Chrome、Electron 多半不支持（返回 None，再退回 ⌘C）。
/// 好处：不碰剪贴板，也不会在"什么都没选"时让系统"咚"一声。
pub fn selected_text(pid: i32) -> Option<String> {
    pool(|| unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        AXUIElementSetMessagingTimeout(app, 0.3);
        let mut out = None;
        if let Some(el) = ax_copy(app, "AXFocusedUIElement") {
            out = ax_string(el, "AXSelectedText").filter(|s| !s.trim().is_empty());
            CFRelease(el);
        }
        CFRelease(app);
        out
    })
}

// ───────────────────────── 全局按键监听（CGEventTap） ─────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum TapEvent {
    /// 普通键按下 / 松开（keycode 是 macOS 虚拟键码）
    Key { keycode: u16, down: bool, flags: u64 },
    /// 修饰键状态变化
    Flags { flags: u64 },
    /// 鼠标其它键（2 = 中键，3 / 4 = 侧键）
    Mouse { button: i64, down: bool },
}

/// 返回 true = 吞掉这个事件（前台程序收不到）
pub type TapHandler = fn(TapEvent) -> bool;

static HANDLER: OnceLock<TapHandler> = OnceLock::new();
static TAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

const EV_KEY_DOWN: u32 = 10;
const EV_KEY_UP: u32 = 11;
const EV_FLAGS_CHANGED: u32 = 12;
const EV_OTHER_MOUSE_DOWN: u32 = 25;
const EV_OTHER_MOUSE_UP: u32 = 26;
const EV_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const EV_TAP_DISABLED_BY_USER: u32 = 0xFFFF_FFFF;

extern "C" fn tap_callback(_proxy: *mut c_void, ty: u32, event: CFTypeRef, _info: *mut c_void) -> CFTypeRef {
    unsafe {
        // 回调太慢 / 系统安全输入时 tap 会被系统停掉，重新打开
        if ty == EV_TAP_DISABLED_BY_TIMEOUT || ty == EV_TAP_DISABLED_BY_USER {
            let tap = TAP.load(Ordering::Relaxed);
            if !tap.is_null() {
                CGEventTapEnable(tap as CFTypeRef, true);
            }
            return event;
        }
        if event.is_null() {
            return event;
        }
        // 忽略我们自己注入的 ⌘C / ⌘V
        if CGEventGetIntegerValueField(event, FIELD_USER_DATA) == MAGIC {
            return event;
        }
        let flags = CGEventGetFlags(event);
        let ev = match ty {
            EV_KEY_DOWN | EV_KEY_UP => TapEvent::Key { keycode: CGEventGetIntegerValueField(event, FIELD_KEYCODE) as u16, down: ty == EV_KEY_DOWN, flags },
            EV_FLAGS_CHANGED => TapEvent::Flags { flags },
            EV_OTHER_MOUSE_DOWN | EV_OTHER_MOUSE_UP => TapEvent::Mouse { button: CGEventGetIntegerValueField(event, FIELD_MOUSE_BUTTON), down: ty == EV_OTHER_MOUSE_DOWN },
            _ => return event,
        };
        match HANDLER.get() {
            Some(h) if h(ev) => std::ptr::null(),
            _ => event,
        }
    }
}

/// 安装全局按键监听并一直运行（阻塞当前线程，请放在专门的线程里）。
/// 没有「辅助功能」权限时 CGEventTapCreate 返回空 → 每 2 秒重试，用户授权后自动生效。
pub fn run_event_tap(handler: TapHandler, log: fn(&str)) {
    let _ = HANDLER.set(handler);
    let mask: u64 = (1 << EV_KEY_DOWN) | (1 << EV_KEY_UP) | (1 << EV_FLAGS_CHANGED) | (1 << EV_OTHER_MOUSE_DOWN) | (1 << EV_OTHER_MOUSE_UP);
    let mut warned = false;
    loop {
        // kCGSessionEventTap(1), kCGHeadInsertEventTap(0), kCGEventTapOptionDefault(0 = 可以改 / 吞事件)
        let tap = unsafe { CGEventTapCreate(1, 0, 0, mask, tap_callback, std::ptr::null_mut()) };
        if tap.is_null() {
            if !warned {
                warned = true;
                log("hotkey: 没有「辅助功能」权限，热键暂不可用（系统设置 → 隐私与安全性 → 辅助功能 里勾选 SayDo）");
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        }
        TAP.store(tap as *mut c_void, Ordering::Relaxed);
        unsafe {
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            log("hotkey: event tap installed");
            CFRunLoopRun();
        }
        return;
    }
}
