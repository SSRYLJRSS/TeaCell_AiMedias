// 防止发布版在 Windows 上弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    teacell_ai_media_manager_lib::run()
}
