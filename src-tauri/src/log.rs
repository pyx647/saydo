//! 极简文件日志：%APPDATA%\\com.user.saydo\\saydo-rust.log（超过 1MB 轮转）。
//! 用来事后查"卡住了 / 没反应"这类问题：热键按下/松开、录音、识别、执行各阶段都会留一行。

use std::io::Write;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static T0: OnceLock<Instant> = OnceLock::new();

pub fn log(msg: &str) {
    let t0 = T0.get_or_init(Instant::now);
    let dir = crate::config::config_dir_static();
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join("saydo-rust.log");
    if std::fs::metadata(&p).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = std::fs::rename(&p, dir.join("saydo-rust.old.log"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let epoch = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "[epoch {epoch} | +{:.1}s] {msg}", t0.elapsed().as_secs_f32());
    }
}
