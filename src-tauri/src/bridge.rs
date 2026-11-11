//! 本地桥：监听 127.0.0.1 随机端口，接收内置 MCP 子进程（saydo.exe --mcp）转来的工具调用。
//! 端口 + 令牌写进 %APPDATA%/com.user.saydo/server.json，MCP 子进程按它连回来。令牌挡住其它本机进程乱调。

use serde_json::{json, Value};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use tauri::AppHandle;

fn rand_hex() -> String {
    let mut s = String::new();
    for _ in 0..2 {
        s.push_str(&format!("{:016x}", RandomState::new().build_hasher().finish()));
    }
    s
}

pub fn start(app: &AppHandle) {
    let token = rand_hex();
    let server = match tiny_http::Server::http("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[bridge] bind failed: {e}");
            return;
        }
    };
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
    let info = json!({ "port": port, "token": token, "pid": std::process::id() });
    let _ = std::fs::write(crate::config::config_dir(app).join("server.json"), info.to_string());
    eprintln!("[bridge] listening on 127.0.0.1:{port}");

    let app = app.clone();
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let app = app.clone();
            let token = token.clone();
            // 每个请求一个线程：提问类工具会阻塞到用户点完
            std::thread::spawn(move || {
                let ok_token = req.headers().iter().any(|h| h.field.equiv("X-Token") && h.value.as_str() == token);
                if !ok_token || req.url() != "/call" {
                    let _ = req.respond(tiny_http::Response::from_string("forbidden").with_status_code(403));
                    return;
                }
                let mut body = String::new();
                let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
                let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let tool = v["tool"].as_str().unwrap_or("").to_string();
                let out = match crate::tools::dispatch(&app, &tool, &v["args"]) {
                    Ok(content) => json!({ "ok": true, "content": content }),
                    Err(e) => json!({ "ok": false, "error": e }),
                };
                let resp = tiny_http::Response::from_string(out.to_string())
                    .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
                let _ = req.respond(resp);
            });
        }
    });
}
