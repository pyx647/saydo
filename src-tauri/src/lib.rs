mod agent;
mod audio;
mod bridge;
mod capture;
mod config;
mod convo;
mod history;
mod hotkey;
mod log;
#[cfg(target_os = "macos")]
mod mac;
mod mcp;
mod proc;
mod router;
mod session;
mod stt;
mod tools;
mod ui;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

/// 设置窗口按需创建、关掉即销毁（不用时不占内存）
fn show_settings(app: &AppHandle) {
    let w = match app.get_webview_window("settings") {
        Some(w) => w,
        None => {
            let built = tauri::WebviewWindowBuilder::new(app, "settings", tauri::WebviewUrl::App("index.html?w=settings".into()))
                .title("SayDo · 设置")
                .inner_size(880.0, 700.0)
                .min_inner_size(760.0, 560.0)
                .center()
                .build();
            match built {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("[ui] 创建设置窗口失败: {e}");
                    return;
                }
            }
        }
    };
    let _ = w.show();
    let _ = w.unminimize();
    let _ = w.set_focus();
}

#[tauri::command]
fn show_settings_cmd(app: AppHandle) {
    show_settings(&app);
}

#[tauri::command]
fn get_config() -> config::Config {
    config::get()
}

#[tauri::command]
fn save_config(app: AppHandle, config: config::Config) -> Result<(), String> {
    hotkey::set_spec(&config.hotkey);
    config::save(&app, config)?;
    let app2 = app.clone();
    std::thread::spawn(move || agent::warm::prewarm(&app2));
    let _ = app.emit("config-changed", json!({}));
    Ok(())
}

#[derive(Serialize)]
struct CliInfo {
    id: String,
    name: String,
    found: bool,
    path: String,
    version: String,
}

#[tauri::command]
async fn detect_clis() -> Vec<CliInfo> {
    let cfg = config::get();
    let items = [("claude", "Claude Code", cfg.claude.path), ("codex", "Codex CLI", cfg.codex.path), ("agy", "Antigravity (agy)", cfg.agy.path)];
    let mut out = vec![];
    for (id, name, ov) in items {
        let found = agent::cli::find_exe(id, &ov);
        let mut version = String::new();
        if let Some(p) = &found {
            let p2 = p.clone();
            let r = tokio::time::timeout(
                std::time::Duration::from_secs(6),
                tokio::task::spawn_blocking(move || {
                    proc::hide(&mut std::process::Command::new(p2)).arg("--version").output()
                }),
            )
            .await;
            if let Ok(Ok(Ok(o))) = r {
                version = String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string();
            }
        }
        out.push(CliInfo { id: id.into(), name: name.into(), found: found.is_some(), path: found.map(|p| p.to_string_lossy().to_string()).unwrap_or_default(), version });
    }
    out
}

#[tauri::command]
fn stt_status(app: AppHandle) -> Value {
    json!({ "ready": stt::model_ready(&app), "dir": stt::model_dir(&app).to_string_lossy() })
}

/// 下载 SenseVoice-Small（中/英/日/韩/粤，约 230MB），进度通过 stt-download-progress 事件推送
#[tauri::command]
async fn stt_download_model(app: AppHandle) -> Result<(), String> {
    use futures_util::StreamExt;
    use std::io::Write;
    let dir = stt::model_dir(&app);
    let base = "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main";
    let client = reqwest::Client::new();
    for name in ["tokens.txt", "model.int8.onnx"] {
        let dest = dir.join(name);
        if dest.exists() {
            continue;
        }
        let resp = client.get(format!("{base}/{name}")).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("下载 {name} 失败：HTTP {}", resp.status()));
        }
        let total = resp.content_length().unwrap_or(0);
        let mut done = 0u64;
        let tmp = dir.join(format!("{name}.tmp"));
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            f.write_all(&chunk).map_err(|e| e.to_string())?;
            done += chunk.len() as u64;
            let _ = app.emit("stt-download-progress", json!({ "file": name, "done": done, "total": total }));
        }
        drop(f);
        std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    }
    let _ = app.emit("stt-download-done", json!({}));
    Ok(())
}

#[tauri::command]
fn app_info(app: AppHandle) -> Value {
    json!({
        "exe": std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "configDir": config::config_dir(&app).to_string_lossy(),
        "version": env!("CARGO_PKG_VERSION"),
        "paused": hotkey::is_paused(),
    })
}

/// 仅测试：SAYDO_TEST=1 时让外部脚本（经 WebView 调试端口）直接驱动弹窗，便于做界面验证
fn test_only() -> Result<(), String> {
    if std::env::var("SAYDO_TEST").as_deref() == Ok("1") { Ok(()) } else { Err("仅测试模式可用".into()) }
}

#[tauri::command]
fn debug_show(app: AppHandle, x: Option<i32>, y: Option<i32>) -> Result<(), String> {
    test_only()?;
    ui::show_panel_at(&app, (x.unwrap_or(700), y.unwrap_or(300)));
    Ok(())
}

#[tauri::command]
fn debug_emit(app: AppHandle, event: String, payload: Value) -> Result<(), String> {
    test_only()?;
    app.emit_to("panel", &event, payload).map_err(|e| e.to_string())
}

#[tauri::command]
fn autostart_get(app: AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn autostart_set(app: AppHandle, on: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let m = app.autolaunch();
    if on { m.enable() } else { m.disable() }.map_err(|e| e.to_string())
}

#[tauri::command]
fn set_paused(app: AppHandle, paused: bool) {
    hotkey::set_paused(paused);
    let _ = app.emit("paused-changed", paused);
}

pub fn run() {
    // 内置 MCP 服务器模式：由 Claude Code / Codex 拉起，走 stdio，不启动任何界面
    if std::env::args().any(|a| a == "--mcp") {
        mcp::run_stdio();
        return;
    }

    // macOS：从访达 / 程序坞启动时 PATH 里没有 homebrew、~/.local/bin 等，先补全，否则找不到 claude / codex / node
    #[cfg(target_os = "macos")]
    proc::fix_path_env();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_settings(app)))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            detect_clis,
            stt_status,
            stt_download_model,
            app_info,
            set_paused,
            show_settings_cmd,
            hotkey::record_hotkey,
            history::history_list,
            history::history_clear,
            convo::convo_list,
            convo::convo_resume,
            convo::convo_close,
            convo::convo_delete,
            session::panel_submit,
            session::panel_replace,
            session::test_backend,
            ui::panel_layout,
            ui::panel_drag,
            ui::panel_cursor_inside,
            ui::panel_focus,
            debug_show,
            debug_emit,
            autostart_get,
            autostart_set,
            ui::panel_hide,
            ui::ask_reply,
            ui::folder_suggestions,
            ui::browse_folder,
            ui::open_path,
            ui::reveal_path,
        ])
        .setup(|app| {
            // macOS：菜单栏常驻的辅助程序——不进程序坞、不抢前台；面板在所有桌面（含全屏应用）上都能出现
            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                if let Some(w) = app.get_webview_window("panel") {
                    let _ = w.set_visible_on_all_workspaces(true);
                }
                // 截屏要「屏幕录制」权限：启动时请求一次（「辅助功能」在 hotkey::install 里请求，麦克风在第一次录音时系统自己问）
                std::thread::spawn(mac::request_screen_capture);
            }
            let handle = app.handle().clone();
            let first_run = !config::config_dir(&handle).join("config.json").exists();
            config::load(&handle);
            let cfg = config::get();
            if first_run {
                let _ = config::save(&handle, cfg.clone());
            }
            hotkey::set_spec(&cfg.hotkey);
            hotkey::install(&handle);
            bridge::start(&handle);

            // 托盘
            let m_settings = MenuItem::with_id(app, "settings", "设置 / 历史", true, None::<&str>)?;
            let m_pause = MenuItem::with_id(app, "pause", "暂停热键", true, None::<&str>)?;
            let m_quit = MenuItem::with_id(app, "quit", "退出 SayDo", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&m_settings, &m_pause, &m_quit])?;
            let pause_item = m_pause.clone();
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("SayDo — 选中 + 按住热键 + 说话")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, ev| match ev.id().as_ref() {
                    "settings" => show_settings(app),
                    "pause" => {
                        let p = !hotkey::is_paused();
                        hotkey::set_paused(p);
                        let _ = pause_item.set_text(if p { "恢复热键" } else { "暂停热键" });
                        let _ = app.emit("paused-changed", p);
                    }
                    "quit" => {
                        agent::warm::kill();
                        app.exit(0)
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        show_settings(tray.app_handle());
                    }
                })
                .build(app)?;

            // 面板 / 设置窗口：关闭 = 隐藏
            for label in ["panel"] {
                if let Some(w) = app.get_webview_window(label) {
                    let w2 = w.clone();
                    w.on_window_event(move |ev| {
                        if let WindowEvent::CloseRequested { api, .. } = ev {
                            api.prevent_close();
                            let _ = w2.hide();
                        }
                    });
                }
            }

            // 预热 Claude Code 常驻进程
            let h3 = handle.clone();
            std::thread::spawn(move || agent::warm::prewarm(&h3));

            // 语音模型按需加载（按下热键时 preload），闲置 5 分钟自动卸载：不说话时不占几百 MB 内存
            std::thread::spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
                stt::unload_if_idle(5 * 60 * 1000);
            });

            if first_run {
                show_settings(&handle);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running SayDo");
}
