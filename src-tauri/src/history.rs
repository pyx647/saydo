//! 历史记录：最近 100 次（指令、结果摘要、后端、结果文件），存 history.json。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::AppHandle;

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Entry {
    pub ts: u64,
    pub instruction: String,
    pub result: String,
    pub backend: String,
    pub context: String,
    pub files: Vec<String>,
    pub ok: bool,
}

fn path(app: &AppHandle) -> std::path::PathBuf {
    crate::config::config_dir(app).join("history.json")
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn load(app: &AppHandle) -> Vec<Entry> {
    std::fs::read_to_string(path(app)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn push(app: &AppHandle, mut e: Entry) {
    e.result = e.result.chars().take(1500).collect();
    let mut v = load(app);
    v.insert(0, e);
    v.truncate(100);
    let _ = std::fs::write(path(app), serde_json::to_string(&v).unwrap_or_default());
}

#[tauri::command]
pub fn history_list(app: AppHandle) -> Value {
    serde_json::to_value(load(&app)).unwrap_or(Value::Null)
}

#[tauri::command]
pub fn history_clear(app: AppHandle) {
    let _ = std::fs::remove_file(path(&app));
}
