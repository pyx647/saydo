//! 配置：存 %APPDATA%/com.user.saydo/config.json，进程内用 RwLock 缓存，保存时立即生效。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use tauri::{AppHandle, Manager};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CliCfg {
    /// 留空 = 自动在 PATH 里找
    pub path: String,
    pub model: String,
    /// 额外命令行参数（空格分隔）
    pub extra_args: String,
    /// 思考强度（目前只用于 Codex 常驻模式：none/minimal/low/medium/high/xhigh）；留空 = low（快）
    pub effort: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct ApiCfg {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}
impl Default for ApiCfg {
    fn default() -> Self {
        Self { base_url: "https://api.openai.com/v1".into(), api_key: String::new(), model: "gpt-5-mini".into() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// "Ctrl+Alt+Space" | "Mouse:XButton1" | "Mouse:XButton2"
    pub hotkey: String,
    /// "claude" | "codex" | "agy" | "openai"
    pub backend: String,
    /// 语音语言 auto|zh|en|ja|ko|yue
    pub lang: String,
    /// "local"(SenseVoice 离线) | "cloud"(OpenAI 兼容 /audio/transcriptions)
    pub stt_engine: String,
    /// 短按（<0.3s）弹出文字输入框
    pub tap_opens_input: bool,
    /// 结果弹窗自动关闭秒数，0 = 不自动关
    pub auto_close_sec: u32,
    /// 审批强度 careful | balanced | auto（仅 Claude Code 能逐项拦截）
    pub approval: String,
    /// CLI 是否拥有整机权限（Codex sandbox=danger-full-access）
    pub full_access: bool,
    /// 保留用户在 ~/.claude 里配置的 hooks（默认关：hooks 会让每次调用慢十几秒）
    pub keep_hooks: bool,
    /// 常驻预热 Claude Code 进程（首条约 10s，之后 4~6s；代价约 300MB 内存）
    pub warm_claude: bool,
    /// 额外的常驻指令（追加到每次提示词末尾）
    pub extra_prompt: String,
    /// 关于我：一段自由文本（我是谁、在哪、做什么），每次都会告诉 Agent
    pub profile: String,
    /// 记住的零碎事实（语音说"记住……"，或在设置 → 记忆里手动加）
    pub memories: Vec<String>,
    pub claude: CliCfg,
    pub codex: CliCfg,
    pub agy: CliCfg,
    pub openai: ApiCfg,
    pub cloud_stt: ApiCfg,
    pub recent_folders: Vec<String>,
}

impl Config {
    /// SayDo 的默认模型与思考强度（设置里留空就用这些）：Claude Code = Sonnet 5.5 / medium，Codex = gpt-6.1-sol / medium
    pub fn claude_model(&self) -> String {
        let m = self.claude.model.trim();
        if m.is_empty() { "claude-sonnet-5-5".into() } else { m.into() }
    }
    pub fn claude_effort(&self) -> String {
        let m = self.claude.effort.trim();
        if m.is_empty() { "medium".into() } else { m.into() }
    }
    pub fn codex_model(&self) -> String {
        let m = self.codex.model.trim();
        if m.is_empty() { "gpt-6.1-sol".into() } else { m.into() }
    }
    pub fn codex_effort(&self) -> String {
        let m = self.codex.effort.trim();
        if m.is_empty() { "medium".into() } else { m.into() }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // macOS 上 Ctrl+Option+Space 是系统的"切换输入法"，默认改用 Option+Space
            hotkey: if cfg!(target_os = "macos") { "Alt+Space" } else { "Ctrl+Alt+Space" }.into(),
            backend: "claude".into(),
            lang: "auto".into(),
            stt_engine: "local".into(),
            tap_opens_input: true,
            auto_close_sec: 0,
            approval: "balanced".into(),
            full_access: true,
            keep_hooks: false,
            warm_claude: true,
            extra_prompt: String::new(),
            profile: String::new(),
            memories: vec![],
            claude: CliCfg::default(),
            codex: CliCfg::default(),
            agy: CliCfg::default(),
            openai: ApiCfg::default(),
            cloud_stt: ApiCfg { base_url: "https://api.openai.com/v1".into(), api_key: String::new(), model: "gpt-4o-mini-transcribe".into() },
            recent_folders: vec![],
        }
    }
}

static CFG: OnceLock<RwLock<Config>> = OnceLock::new();
fn cell() -> &'static RwLock<Config> {
    CFG.get_or_init(|| RwLock::new(Config::default()))
}

pub fn get() -> Config {
    cell().read().map(|g| g.clone()).unwrap_or_default()
}

pub fn config_dir(app: &AppHandle) -> PathBuf {
    let d = app.path().app_config_dir().unwrap_or_else(|_| std::env::temp_dir().join("saydo"));
    let _ = std::fs::create_dir_all(&d);
    d
}

/// 不依赖 AppHandle 的版本（MCP 子进程用）。必须和 Tauri 的 app_config_dir 是同一个目录：
/// Windows = %APPDATA%/com.user.saydo，macOS = ~/Library/Application Support/com.user.saydo
pub fn config_dir_static() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    #[cfg(target_os = "macos")]
    let base = std::env::var("HOME").map(|h| PathBuf::from(h).join("Library/Application Support")).unwrap_or_else(|_| std::env::temp_dir());
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("com.user.saydo")
}

pub fn load(app: &AppHandle) {
    let _ = std::fs::create_dir_all(config_dir(app).join("backups"));
    let p = config_dir(app).join("config.json");
    if let Ok(s) = std::fs::read_to_string(&p) {
        if let Ok(c) = serde_json::from_str::<Config>(s.trim_start_matches('\u{feff}')) {
            *cell().write().unwrap() = c;
        }
    }
}

/// 「关于我」或「记住的事」要被改动 / 清空之前，先把旧内容存一份到 backups/（只存这两项，不含 API Key；保留最近 30 份）
fn backup_memory_if_changed(dir: &std::path::Path, file: &std::path::Path, new: &Config) {
    let Ok(old) = std::fs::read_to_string(file) else { return };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(old.trim_start_matches('\u{feff}')) else { return };
    let old_profile = v["profile"].as_str().unwrap_or("").to_string();
    let old_mem: Vec<String> = v["memories"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if (old_profile == new.profile && old_mem == new.memories) || (old_profile.trim().is_empty() && old_mem.is_empty()) {
        return;
    }
    let bdir = dir.join("backups");
    if std::fs::create_dir_all(&bdir).is_err() {
        return;
    }
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let body = serde_json::json!({ "profile": old_profile, "memories": old_mem });
    let _ = std::fs::write(bdir.join(format!("memory-{ts}.json")), serde_json::to_string_pretty(&body).unwrap_or_default());
    if let Ok(rd) = std::fs::read_dir(&bdir) {
        let mut names: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("memory-")).unwrap_or(false)).collect();
        names.sort();
        if names.len() > 30 {
            for p in &names[..names.len() - 30] {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

pub fn save(app: &AppHandle, c: Config) -> Result<(), String> {
    let p = config_dir(app).join("config.json");
    backup_memory_if_changed(&config_dir(app), &p, &c);
    let s = serde_json::to_string_pretty(&c).map_err(|e| e.to_string())?;
    std::fs::write(&p, s).map_err(|e| e.to_string())?;
    *cell().write().unwrap() = c;
    Ok(())
}

/// 记录最近用过的文件夹（最多 8 个，最新在前）
pub fn remember_folder(app: &AppHandle, folder: &str) {
    let mut c = get();
    c.recent_folders.retain(|f| !f.eq_ignore_ascii_case(folder));
    c.recent_folders.insert(0, folder.to_string());
    c.recent_folders.truncate(8);
    let _ = save(app, c);
}

/// 记住一条事实；已有相同的返回 false
pub fn add_memory(app: &AppHandle, text: &str) -> bool {
    let mut c = get();
    let t = text.trim();
    if t.is_empty() || c.memories.iter().any(|m| m == t) {
        return false;
    }
    c.memories.push(t.to_string());
    if c.memories.len() > 100 {
        c.memories.remove(0);
    }
    save(app, c).is_ok()
}

/// 忘掉所有包含关键词的记忆，返回删掉了几条；关键词为 "*" 表示全部
pub fn forget_memory(app: &AppHandle, keyword: &str) -> usize {
    let mut c = get();
    let before = c.memories.len();
    let k = keyword.trim().to_lowercase();
    if k == "*" {
        c.memories.clear();
    } else {
        c.memories.retain(|m| !m.to_lowercase().contains(&k));
    }
    let n = before - c.memories.len();
    if n > 0 {
        let _ = save(app, c);
    }
    n
}
