//! Prompt Forge — 轻量级提示词快速增强工具
//!
//! 模块划分：
//! - config   配置持久化（JSON，系统应用数据目录）
//! - modes    增强模式库（模板驱动，可扩展）
//! - llm      多协议 LLM 客户端（OpenAI 兼容 / Ollama / Gemini）
//! - tray     系统托盘
//! - commands Tauri command 层（前端 IPC 边界）

pub mod commands;
pub mod config;
pub mod llm;
pub mod modes;
pub mod normalize;
mod tray;

use crate::config::ConfigIo;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 二次启动 → 唤起已有主窗口
            tray::show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(tokio::sync::Mutex::new(Vec::<commands::HistoryEntry>::new()))
        .manage(commands::CancelState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            // 按配置决定是否注册自启动 —— 默认开启
            let cfg = config::AppConfig::load(&handle).unwrap_or_default();
            if cfg.autostart {
                let _ = handle.autolaunch().enable();
            }
            tray::setup_tray(&handle)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_modes,
            commands::get_config,
            commands::save_config,
            commands::test_llm,
            commands::enhance_prompt,
            commands::get_history,
            commands::cancel_enhance,
            commands::copy_to_clipboard,
            commands::close_main_window
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                use tauri::Manager;
                let app = window.app_handle();
                let cfg = config::AppConfig::load(app).unwrap_or_default();
                if cfg.close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
