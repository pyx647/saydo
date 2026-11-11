// Release 构建用 GUI 子系统（不弹控制台）。--mcp 模式靠继承的管道做 stdio，GUI 子系统同样可用。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    saydo_lib::run()
}
