//! 对话：每一段"可以接着聊"的任务。
//!
//! - 每次按热键开一个新任务 = 一段新对话；结果卡还开着时按热键 = 接着当前这段（见 session.rs）。
//! - 每段对话只记最近一轮 (你说的话, 回答)，接着聊时作为上下文交给 Agent，和追问用的是同一份。
//! - 可以在面板里随时切回别的对话；也可以"结束"一段对话（从切换列表里消失），
//!   以后在 设置 → 对话 里点"继续对话"再把它请回来。
//! - 存在 convos.json，最多保留 40 段（先淘汰最久没动的已结束对话）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::AppHandle;

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Convo {
    pub id: u64,
    pub title: String,
    /// 开这段对话时的前台程序
    pub app: String,
    pub ts: u64,
    pub turns: u32,
    pub closed: bool,
    /// 最近一轮：你说的话 / 回答
    pub said: String,
    pub text: String,
}

const MAX_CONVOS: usize = 40;

static STORE: OnceLock<Mutex<Option<Vec<Convo>>>> = OnceLock::new();
fn store() -> &'static Mutex<Option<Vec<Convo>>> {
    STORE.get_or_init(|| Mutex::new(None))
}

fn path(app: &AppHandle) -> std::path::PathBuf {
    crate::config::config_dir(app).join("convos.json")
}

/// 取出（必要时先从磁盘读入）对话列表，在锁里跑 f；f 返回后若 dirty 为真则写回磁盘
fn with<R>(app: &AppHandle, f: impl FnOnce(&mut Vec<Convo>) -> (R, bool)) -> R {
    let mut g = store().lock().unwrap();
    if g.is_none() {
        let v: Vec<Convo> = std::fs::read_to_string(path(app)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        *g = Some(v);
    }
    let v = g.as_mut().unwrap();
    let (r, dirty) = f(v);
    if dirty {
        v.sort_by(|a, b| b.ts.cmp(&a.ts));
        while v.len() > MAX_CONVOS {
            // 先丢最久没动的已结束对话，没有已结束的再丢最旧的
            match v.iter().rposition(|c| c.closed) {
                Some(i) => {
                    v.remove(i);
                }
                None => {
                    v.pop();
                }
            }
        }
        let _ = std::fs::write(path(app), serde_json::to_string(&*v).unwrap_or_default());
    }
    r
}

/// 新对话的编号：毫秒时间戳，保证单调递增（也不超过 JS 能精确表示的整数范围）
pub fn new_id() -> u64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(1);
    let mut prev = LAST.load(Ordering::SeqCst);
    loop {
        let next = ms.max(prev + 1);
        match LAST.compare_exchange(prev, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return next,
            Err(p) => prev = p,
        }
    }
}

fn short(s: &str, n: usize) -> String {
    let t: String = s.trim().chars().filter(|c| !c.is_control()).collect();
    if t.chars().count() > n {
        format!("{}…", t.chars().take(n).collect::<String>())
    } else {
        t
    }
}

/// 一轮结束：把这段对话的最新状态记下来（没有就新建；被结束过的会自动恢复）
pub fn record(app: &AppHandle, id: u64, app_name: &str, said: &str, answer: &str) {
    if id == 0 {
        return;
    }
    let (said, text) = (said.to_string(), answer.chars().take(6000).collect::<String>());
    let app_name = app_name.to_string();
    with(app, move |v| {
        let ts = crate::history::now();
        if let Some(i) = v.iter().position(|c| c.id == id) {
            let c = &mut v[i];
            c.ts = ts;
            c.turns += 1;
            c.closed = false;
            c.said = said;
            c.text = text;
        } else {
            v.push(Convo { id, title: short(&said, 24), app: app_name, ts, turns: 1, closed: false, said, text });
        }
        ((), true)
    });
}

pub fn get(app: &AppHandle, id: u64) -> Option<Convo> {
    with(app, |v| (v.iter().find(|c| c.id == id).cloned(), false))
}

pub fn set_closed(app: &AppHandle, id: u64, closed: bool) {
    with(app, |v| {
        let mut hit = false;
        if let Some(c) = v.iter_mut().find(|c| c.id == id) {
            c.closed = closed;
            hit = true;
        }
        ((), hit)
    });
}

// ───────────────────────── 给前端的命令 ─────────────────────────

/// { current: 正在聊的那段的编号（没有则 0）, items: 全部对话（新的在前，含已结束的） }
#[tauri::command]
pub fn convo_list(app: AppHandle) -> Value {
    let items = with(&app, |v| (v.clone(), false));
    json!({ "current": crate::session::current_convo(), "items": items })
}

/// 切到某段对话（或把已结束的请回来）：面板显示它最近一轮的结果，接着按热键说话就是继续这段
#[tauri::command]
pub fn convo_resume(app: AppHandle, id: u64) -> Result<(), String> {
    let c = get(&app, id).ok_or_else(|| "找不到这段对话".to_string())?;
    if crate::ui::panel_visible(&app) && !crate::ui::result_showing() {
        return Err("有任务正在执行，等它结束再切换".into());
    }
    set_closed(&app, id, false);
    crate::session::resume(&app, &c);
    Ok(())
}

/// 结束一段对话：从切换列表里拿掉。如果它正是当前这段，面板也一并收起，下次按热键就是全新的对话
#[tauri::command]
pub fn convo_close(app: AppHandle, id: u64) {
    set_closed(&app, id, true);
    if crate::session::current_convo() == id {
        crate::session::end_current(&app);
        crate::ui::hide_panel(&app);
        crate::ui::emit(&app, json!({ "phase": "idle" }));
    }
}

/// 彻底删除一段对话记录（设置里用）
#[tauri::command]
pub fn convo_delete(app: AppHandle, id: u64) {
    if crate::session::current_convo() == id {
        crate::session::end_current(&app);
    }
    with(&app, |v| {
        v.retain(|c| c.id != id);
        ((), true)
    });
}
