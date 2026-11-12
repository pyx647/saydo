//! Agent 调度：把"语音指令 + 当前上下文"交给后端（Claude Code / Codex / agy / OpenAI API），
//! 流式把进度和结果推给弹窗，最后解析出结果文件。
//!
//! 为什么主力是 CLI：Claude Code / Codex / agy 在本机已经配好了 Google 连接器、各种 MCP 服务器和 Skills，
//! 无头模式下原样继承。直接调 OpenAI API 则只有我们自己实现的那点工具，所以它只作"快速模式"（翻译/看屏幕等）。

pub mod cli;
pub mod openai;
pub mod codex_warm;
pub mod warm;

use crate::capture::{FgInfo, Selection};
use crate::config::Config;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::AppHandle;

pub struct RunInput {
    pub instruction: String,
    pub fg: FgInfo,
    pub sel: Selection,
    pub shot: Option<PathBuf>,
    /// 按下热键时自动保存的截图：只告诉 agent 有这张图，需要时再读
    pub shot_saved: Option<PathBuf>,
    /// 前台是 Office 时，正在打开的那份文档的描述（路径 / 当前页 / 选区）
    pub doc_ctx: Option<String>,
    /// 这个任务专用的临时文件夹：所有中间产物放这里，任务结束后 SayDo 自动删除
    pub scratch: Option<PathBuf>,
    /// 用户在弹窗里预先选定的目标文件夹
    pub dest: Option<String>,
    /// 上一轮 (用户指令, 回答)，用于追问
    pub prev: Option<(String, String)>,
}

pub struct RunOut {
    pub text: String,
    pub files: Vec<String>,
}

// ───────────────────────── 进度汇报 ─────────────────────────

struct SinkState {
    steps: Vec<String>,
    text: String,
    last_emit: Instant,
}

pub struct Sink {
    app: AppHandle,
    st: Mutex<SinkState>,
    started: Instant,
    pub cancel: Arc<AtomicBool>,
    /// 静默：预热用，不向界面推送任何状态
    silent: bool,
}

impl Sink {
    pub fn new(app: AppHandle, cancel: Arc<AtomicBool>) -> Self {
        Self {
            app,
            st: Mutex::new(SinkState { steps: vec![], text: String::new(), last_emit: Instant::now() - std::time::Duration::from_secs(1) }),
            started: Instant::now(),
            cancel,
            silent: false,
        }
    }
    pub fn silent(app: AppHandle) -> Self {
        let mut s = Self::new(app, Arc::new(AtomicBool::new(false)));
        s.silent = true;
        s
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    fn flush(&self, st: &mut SinkState, force: bool) {
        if self.silent || crate::ui::ask_pending() {
            return;
        }
        if !force && st.last_emit.elapsed().as_millis() < 90 {
            return;
        }
        st.last_emit = Instant::now();
        let n = st.steps.len();
        let tail: Vec<&String> = st.steps.iter().skip(n.saturating_sub(5)).collect();
        crate::ui::emit(
            &self.app,
            json!({ "phase": "working", "steps": tail, "stepCount": n, "text": st.text, "elapsed": self.started.elapsed().as_secs() }),
        );
    }
    pub fn step(&self, s: &str) {
        let mut st = self.st.lock().unwrap();
        if st.steps.last().map(|l| l.as_str()) != Some(s) {
            st.steps.push(s.to_string());
        }
        self.flush(&mut st, true);
    }
    pub fn clear_text(&self) {
        let mut st = self.st.lock().unwrap();
        st.text.clear();
    }
    pub fn push_delta(&self, d: &str) {
        let mut st = self.st.lock().unwrap();
        st.text.push_str(d);
        self.flush(&mut st, false);
    }
    pub fn set_text(&self, t: &str) {
        let mut st = self.st.lock().unwrap();
        st.text = t.to_string();
        self.flush(&mut st, true);
    }
    pub fn heartbeat(&self) {
        let mut st = self.st.lock().unwrap();
        self.flush(&mut st, true);
    }
}

// ───────────────────────── 提示词 ─────────────────────────

pub fn workspace_dir(app: &AppHandle) -> PathBuf {
    let d = crate::config::config_dir(app).join("workspace");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn human_size(n: u64) -> String {
    if n >= 1 << 20 { format!("{:.1} MB", n as f64 / (1u64 << 20) as f64) } else if n >= 1 << 10 { format!("{} KB", n >> 10) } else { format!("{n} B") }
}

fn trunc(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n { format!("{t}…") } else { t }
}

/// 用户的长期记忆：「关于我」+ 记住的事实（有长度上限，防止提示词越滚越大）
fn memory_block(cfg: &Config) -> String {
    let mut o = String::new();
    let prof = cfg.profile.trim();
    if !prof.is_empty() {
        o.push_str(&trunc(prof, 1500));
        o.push('\n');
    }
    let n = cfg.memories.len();
    for m in cfg.memories.iter().skip(n.saturating_sub(60)) {
        o.push_str(&format!("- {}\n", trunc(m.trim(), 200)));
    }
    o
}

/// 构造提示词；返回 (prompt, 需要额外授权访问的目录)
pub fn build_prompt(app: &AppHandle, cfg: &Config, inp: &RunInput, with_mcp: bool) -> (String, Vec<PathBuf>) {
    let mut dirs: Vec<PathBuf> = vec![];
    let mut ctx = String::new();

    ctx.push_str(&format!("- 前台程序：{}，窗口标题：「{}」\n", if inp.fg.exe.is_empty() { "未知" } else { &inp.fg.exe }, trunc(&inp.fg.title, 120)));

    if let Some(shot) = &inp.shot {
        ctx.push_str(&format!(
            "- 屏幕截图：`{}`（用户说的是\"当前屏幕/页面\"，请先查看这张截图再回答；忽略任何选中内容）\n",
            shot.display()
        ));
        if let Some(d) = shot.parent() {
            dirs.push(d.to_path_buf());
        }
    } else {
        if let Some(sp) = &inp.shot_saved {
            ctx.push_str(&format!(
                "- 按下热键时自动保存了一张屏幕截图：`{}`（我没看过它；只有任务确实要看屏幕时才读取，用不到就别读）\n",
                sp.display()
            ));
            if let Some(d) = sp.parent() {
                dirs.push(d.to_path_buf());
            }
        }
        match &inp.sel {
            Selection::None => ctx.push_str("- 选中内容：无\n"),
            Selection::Text(t) => {
                let n = t.chars().count();
                if n > 6000 {
                    let p = workspace_dir(app).join(format!("selection-{}.txt", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)));
                    let _ = std::fs::write(&p, t);
                    ctx.push_str(&format!("- 选中文字（共 {n} 字，太长已存为文件 `{}`，开头预览如下）：\n\"\"\"\n{}\n\"\"\"\n", p.display(), trunc(t, 500)));
                } else {
                    ctx.push_str(&format!("- 选中文字（{n} 字）：\n\"\"\"\n{t}\n\"\"\"\n"));
                }
            }
            Selection::Files(fs) => {
                ctx.push_str("- 选中的文件：\n");
                for f in fs {
                    let v = crate::ui::file_view(f);
                    ctx.push_str(&format!("  - `{}`（{}，{}）\n", f, if v.is_dir { "文件夹".to_string() } else { v.ext.clone() }, human_size(v.size)));
                    if let Some(d) = Path::new(f).parent() {
                        if !dirs.iter().any(|x| x == d) {
                            dirs.push(d.to_path_buf());
                        }
                    }
                }
            }
        }
    }

    if let Some(d) = &inp.doc_ctx {
        ctx.push_str(d);
    }

    if let Some(sd) = &inp.scratch {
        dirs.push(sd.clone());
    }
    if let Some(d) = &inp.dest {
        ctx.push_str(&format!("- 目标文件夹：用户已在弹窗里选定 `{d}`，直接使用，不要再问。\n"));
        dirs.push(PathBuf::from(d));
    }

    let mut p = String::new();
    p.push_str("你是 SayDo —— 用户在 Windows 11 上通过【选中内容 + 按住热键说话】唤起的桌面快捷 Agent。\n");
    p.push_str("你的回答会显示在一个很小的弹窗里：只给结论或成品，简洁，不寒暄、不复述任务、不解释你做了哪些步骤（除非用户问）。用户说什么语言就用什么语言回答。\n\n");
    let mem = memory_block(cfg);
    if !mem.is_empty() {
        p.push_str("## 关于用户（长期记忆，是已知事实，不要再向用户询问这些）\n");
        p.push_str(&mem);
        p.push('\n');
    }
    p.push_str("## 当前上下文\n");
    p.push_str(&ctx);
    if let Some((pi, pa)) = &inp.prev {
        p.push_str(&format!("\n## 上一轮对话（用户在追问）\n用户说：{}\n你答：{}\n", trunc(pi, 300), trunc(pa, 1500)));
    }
    p.push_str(&format!("\n## 用户的语音指令\n「{}」\n\n", inp.instruction.trim()));
    p.push_str("## 规则\n");
    p.push_str("1. 指令针对\"当前页面/屏幕/界面\"时，以截图为准；否则以选中的文字/文件为操作对象。\n");
    p.push_str("2. 对文件操作（转格式、重命名、压缩、提取内容等）：直接动手做。输出文件默认放在原文件同一目录；绝不覆盖已存在的文件（重名就加 (1)）；删除只能进回收站。\n");
    if with_mcp {
        p.push_str("3. 需要用户选择文件夹时，调用 saydo 的 `pick_folder` 工具；需要用户拍板（发邮件、对外分享、覆盖、不可逆操作）时，调用 `confirm` 工具，不要自己猜。需要看屏幕时，上下文里有截图文件就直接读那个文件，没有才调用 `screenshot`。\n");
    } else {
        p.push_str("3. 需要用户选择目标位置而上下文里没有时，在回答里直接说明需要什么，不要猜。\n");
    }
    p.push_str("4. 你本机配好的所有 MCP（Gmail / Drive / 日历 / Sheets 等）和 Skills 都可以用来完成任务。\n");
    p.push_str("5. 如果你创建、移动、重命名或修改了文件，在回答的最后每个文件单独一行写：`FILE: <绝对路径>`（这些行会被弹窗渲染成\"打开 / 定位\"按钮）。\n");
    if let Some(sd) = &inp.scratch {
        p.push_str(&format!(
            "7. **文件放哪里**：这个任务有专用的临时文件夹 `{}`。所有中间产物（草稿、测试图、转换过程里的文件、临时脚本、反复修改的版本）一律放在这个文件夹里，任务结束后 SayDo 会自动清理它；**绝对不要把中间文件放到桌面、下载或文档里**。只有最终成品才放到用户要的位置：有原文件就放原文件同一目录；没有原文件时，放进 `文档\\SayDo输出\\<任务名>\\`（一个任务一个子文件夹），**不要放桌面**。成品只留用户真正要的那几个文件，不要留多个版本。\n",
            sd.display()
        ));
    }
    if inp.doc_ctx.is_some() {
        p.push_str("6. 上面的 Office 文档就是用户说的\"这个 PPT / 这份文档 / 这个表格 / 这一页\"。要修改它，就**直接改这份正在打开的文档**：用 PowerShell 的 COM（`[Runtime.InteropServices.Marshal]::GetActiveObject('PowerPoint.Application')`，Word / Excel 同理）操作已打开的对象，改动会立刻出现在用户眼前。**不要新建另一个文件，不要另存为副本，不要改磁盘上的同名文件**，除非用户明确说了\"另存 / 复制一份 / 新建\"。改完不要自动保存或关闭文档，除非用户要求。COM 连不上就如实告诉用户原因，不要悄悄改做一个新文件。\n");
    }
    if !cfg.extra_prompt.trim().is_empty() {
        p.push_str(&format!("\n## 用户的常驻偏好\n{}\n", cfg.extra_prompt.trim()));
    }
    (p, dirs)
}

/// 从回答里剥离 `FILE:` 行，返回 (正文, 文件路径)
pub fn extract_files(text: &str) -> (String, Vec<String>) {
    let mut files = vec![];
    let mut body: Vec<&str> = vec![];
    for line in text.lines() {
        let t = line.trim().trim_start_matches(['-', '*', '•', '📎', ' ']).trim();
        let up = t.to_ascii_uppercase();
        if up.starts_with("FILE:") || up.starts_with("FILE：") {
            let path = t[5..].trim().trim_matches(['`', '"', '\'', '*', ' ']).replace('/', "\\");
            if Path::new(&path).exists() && !files.contains(&path) {
                files.push(path);
            }
        } else {
            body.push(line);
        }
    }
    (body.join("\n").trim().to_string(), files)
}

// ───────────────────────── 入口 ─────────────────────────

pub async fn run(app: &AppHandle, cfg: &Config, inp: RunInput, sink: &Sink) -> Result<RunOut, String> {
    let raw = match cfg.backend.as_str() {
        "openai" => openai::run(app, cfg, &inp, sink).await?,
        "codex" | "agy" | "claude" => cli::run(app, cfg, &inp, sink).await?,
        other => return Err(format!("未知后端：{other}")),
    };
    let (text, files) = extract_files(&raw);
    Ok(RunOut { text, files })
}
