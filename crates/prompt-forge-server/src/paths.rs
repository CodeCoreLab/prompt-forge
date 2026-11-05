//! 配置路径解析：按平台约定定位用户配置目录。
//!
//! 不引入 `dirs` 等依赖，手写规则保持依赖树最小：
//! - Windows: `%APPDATA%\prompt-forge\config.json`
//! - macOS  : `~/Library/Application Support/prompt-forge/config.json`
//! - Linux  : `$XDG_CONFIG_HOME/prompt-forge/config.json`（缺省 `~/.config/prompt-forge`）
//!
//! 目录名与历史版本保持一致（`prompt-forge`），用户既有配置（含 DPAPI 密文 API Key）
//! 无需迁移即可被新版本读取。

use std::path::PathBuf;

use prompt_forge_core::config::CONFIG_FILE;

const APP_DIR: &str = "prompt-forge";

/// 返回配置目录（不保证存在）
pub fn config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join(APP_DIR);
        }
        // APPDATA 缺失（极少见）：退化到用户目录
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(home).join("AppData").join("Roaming").join(APP_DIR);
        }
        PathBuf::from(APP_DIR)
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join(APP_DIR);
        }
        PathBuf::from(APP_DIR)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join(APP_DIR);
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".config").join(APP_DIR);
        }
        PathBuf::from(APP_DIR)
    }
}

/// 配置文件完整路径
pub fn config_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}
