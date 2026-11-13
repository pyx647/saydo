//! 子进程相关的跨平台小工具：隐藏控制台窗口（Windows）、杀掉整棵进程树。

/// Windows：CREATE_NO_WINDOW，避免每起一个子进程闪一下黑框；其它系统无操作
pub fn hide(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

pub fn hide_tokio(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// 杀掉 pid 以及它的所有子孙进程（CLI 会再起 node / MCP 子进程，只杀父进程会留下孤儿）
#[cfg(windows)]
pub fn kill_tree(pid: u32) {
    let _ = hide(std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"])).output();
}

#[cfg(not(windows))]
pub fn kill_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    // 先收集整棵树（父进程一死，子进程会被 launchd/init 收养，就找不到了），再统一杀
    let mut all = vec![pid];
    let mut i = 0;
    while i < all.len() && all.len() < 512 {
        if let Ok(o) = std::process::Command::new("pgrep").args(["-P", &all[i].to_string()]).output() {
            for l in String::from_utf8_lossy(&o.stdout).lines() {
                if let Ok(c) = l.trim().parse::<u32>() {
                    if !all.contains(&c) {
                        all.push(c);
                    }
                }
            }
        }
        i += 1;
    }
    let mut cmd = std::process::Command::new("kill");
    cmd.arg("-9");
    for p in &all {
        cmd.arg(p.to_string());
    }
    let _ = cmd.stderr(std::process::Stdio::null()).output();
}

/// macOS：从 Finder / Dock 启动的 App 拿不到终端里的 PATH（只有 /usr/bin:/bin:/usr/sbin:/sbin），
/// 于是找不到 claude / codex / node。启动时问一次登录 shell 要完整 PATH，再补上常见安装目录。
#[cfg(target_os = "macos")]
pub fn fix_path_env() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut parts: Vec<String> = vec![];
    let add = |p: &str, parts: &mut Vec<String>| {
        for d in p.split(':') {
            let d = d.trim();
            if !d.is_empty() && !parts.iter().any(|x| x == d) {
                parts.push(d.to_string());
            }
        }
    };
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    if let Ok(mut child) = Command::new(&shell)
        .args(["-ilc", "printf '__SAYDO_PATH__%s__SAYDO_END__' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        let pid = child.id();
        let mut out = child.stdout.take();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(o) = out.as_mut() {
                let _ = o.read_to_string(&mut s);
            }
            let _ = tx.send(s);
        });
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(s) => {
                if let Some(a) = s.find("__SAYDO_PATH__") {
                    let rest = &s[a + "__SAYDO_PATH__".len()..];
                    if let Some(b) = rest.find("__SAYDO_END__") {
                        add(&rest[..b], &mut parts);
                    }
                }
                let _ = child.wait();
            }
            Err(_) => {
                kill_tree(pid);
                let _ = child.wait();
            }
        }
    }
    if let Ok(cur) = std::env::var("PATH") {
        add(&cur, &mut parts);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for d in ["/opt/homebrew/bin", "/usr/local/bin"] {
        add(d, &mut parts);
    }
    if !home.is_empty() {
        for sub in [".local/bin", ".claude/local", ".npm-global/bin", ".bun/bin", ".cargo/bin", ".volta/bin"] {
            add(&format!("{home}/{sub}"), &mut parts);
        }
    }
    std::env::set_var("PATH", parts.join(":"));
}
