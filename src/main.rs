#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(windows)]
mod windows_ui;
#[cfg(windows)]
fn main() {
    windows_ui::run();
}
#[cfg(not(windows))]
fn main() {
    eprintln!("此图形应用仅支持 Windows。其他系统可使用 avt-cli 检查或测试数据处理。");
}
