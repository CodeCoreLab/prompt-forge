//! 配置：结构在 prompt-forge-core::config；
//! 文件 I/O（load/save/原子写/quarantine）依赖 Tauri 路径解析，以扩展 trait 形式实现
//! （Rust 不允许跨 crate 定义 inherent impl）。

pub use prompt_forge_core::config::{AppConfig, LlmConfig, CONFIG_FILE};

use std::fs;
use std::path::PathBuf;
use tauri::Manager;

/// AppConfig 的 Tauri 文件 I/O 扩展
pub trait ConfigIo {
    fn load(app: &tauri::AppHandle) -> Result<Self, String>
    where
        Self: Sized;
    fn save(&self, app: &tauri::AppHandle) -> Result<(), String>;
    fn path(app: &tauri::AppHandle) -> Result<PathBuf, String>;
}

impl ConfigIo for AppConfig {
    fn load(app: &tauri::AppHandle) -> Result<Self, String> {
        let path = Self::path(app)?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        match serde_json::from_str(&raw) {
            Ok(cfg) => Ok(cfg),
            Err(e) => {
                // 损坏数据留档 quarantine（对齐 DSH market storage 护栏：绝不静默丢弃），
                // 返回默认值让应用可用，用户可从 .corrupt 文件人工找回
                let corrupt = path.with_extension("json.corrupt");
                let _ = fs::copy(&path, &corrupt);
                eprintln!("config.json 解析失败（已留档 {}）：{e}", corrupt.display());
                Ok(Self::default())
            }
        }
    }

    fn save(&self, app: &tauri::AppHandle) -> Result<(), String> {
        self.validate()?; // 入口校验一次
        let path = Self::path(app)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // 原子写入：先写临时文件再 rename，避免写一半崩溃损坏配置
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    fn path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
        let dir = app
            .path()
            .app_config_dir()
            .map_err(|_| "无法获取配置目录".to_string())?;
        Ok(dir.join(CONFIG_FILE))
    }
}
