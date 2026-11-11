//! "快速模式"：直接调 OpenAI 兼容的 /chat/completions（流式）。
//! 没有 MCP、没有 Skills、不能动文件，只适合翻译 / 总结 / 解释 / 看屏幕 这类一问一答。

use super::{RunInput, Sink};
use crate::capture::Selection;
use crate::config::Config;
use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tauri::AppHandle;

fn is_texty(ext: &str) -> bool {
    ["txt", "md", "csv", "json", "log", "py", "js", "ts", "tsx", "rs", "html", "css", "xml", "yaml", "yml", "ini", "toml", "tex", "bib", "fasta", "fa", "sh", "ps1", "sql"].contains(&ext)
}

pub async fn run(_app: &AppHandle, cfg: &Config, inp: &RunInput, sink: &Sink) -> Result<String, String> {
    let c = &cfg.openai;
    if c.api_key.trim().is_empty() {
        return Err("还没有配置 OpenAI API Key（设置 → 后端 → OpenAI 快速模式）".into());
    }
    let mut ctx = format!("前台程序：{}（{}）\n", inp.fg.exe, inp.fg.title);
    let mut parts: Vec<Value> = vec![];

    if let Some(shot) = &inp.shot {
        let bytes = std::fs::read(shot).map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        parts.push(json!({ "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{b64}") } }));
        ctx.push_str("用户说的是当前屏幕/页面，已附上截图。\n");
    } else {
        match &inp.sel {
            Selection::None => ctx.push_str("选中内容：无\n"),
            Selection::Text(t) => ctx.push_str(&format!("选中文字：\n\"\"\"\n{}\n\"\"\"\n", t.chars().take(30000).collect::<String>())),
            Selection::Files(fs) => {
                ctx.push_str("选中文件：\n");
                for f in fs {
                    let v = crate::ui::file_view(f);
                    ctx.push_str(&format!("- {} ({} 字节)\n", f, v.size));
                    if is_texty(&v.ext) && v.size < 200_000 {
                        if let Ok(t) = std::fs::read_to_string(f) {
                            ctx.push_str(&format!("  内容：\n\"\"\"\n{}\n\"\"\"\n", t.chars().take(20000).collect::<String>()));
                        }
                    }
                }
            }
        }
    }
    if let Some((pi, pa)) = &inp.prev {
        ctx.push_str(&format!("\n上一轮：用户说「{pi}」；你答：{}\n", pa.chars().take(1200).collect::<String>()));
    }
    let system = "你是 SayDo，一个桌面快捷助手。回答显示在小弹窗里：简洁，只给结论或成品，不寒暄。用户说什么语言就用什么语言。你在快速模式下没有任何工具，不能操作文件或访问外部服务；如果任务需要，请直接告诉用户\"请在设置里切换到 Claude Code / Codex 后端\"。";
    parts.insert(0, json!({ "type": "text", "text": format!("## 上下文\n{ctx}\n## 用户的语音指令\n「{}」", inp.instruction.trim()) }));

    let body = json!({
        "model": c.model,
        "stream": true,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": parts }
        ]
    });
    let url = format!("{}/chat/completions", c.base_url.trim_end_matches('/'));
    sink.step(&format!("调用 {}…", c.model));
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(c.api_key.trim())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求失败：{e}"))?;
    if !resp.status().is_success() {
        let code = resp.status();
        let t = resp.text().await.unwrap_or_default();
        return Err(format!("OpenAI 接口 {code}：{}", t.chars().take(300).collect::<String>()));
    }

    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut out = String::new();
    while let Some(chunk) = stream.next().await {
        if sink.cancelled() {
            return Err("已取消".into());
        }
        let chunk = chunk.map_err(|e| e.to_string())?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    if let Some(d) = v["choices"][0]["delta"]["content"].as_str() {
                        out.push_str(d);
                        sink.push_delta(d);
                    }
                }
            }
        }
    }
    if out.trim().is_empty() {
        return Err("模型没有返回内容".into());
    }
    Ok(out)
}
