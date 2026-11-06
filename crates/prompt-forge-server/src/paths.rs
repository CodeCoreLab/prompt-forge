//! 配置路径解析：按平台约定定位用户配置目录。
//!
//! 不引入 `dirs` 等依赖，手写规则保持依赖树最小：
//! - 覆盖入口：`PROMPT_FORGE_CONFIG_DIR`（任意平台，指向目录本身，非目录内的子目录）
//! - Windows: `%APPDATA%\prompt-forge\config.json`
//! - macOS  : `~/Library/Application Support/prompt-forge/config.json`
//! - Linux  : `$XDG_CONFIG_HOME/prompt-forge/config.json`（缺省 `~/.config/prompt-forge`）
//!
//! 目录名与历史版本保持一致（`prompt-forge`），用户既有配置（含 DPAPI 密文 API Key）
//! 无需迁移即可被新版本读取。

use std::path::PathBuf;

use prompt_forge_core::config::CONFIG_FILE;

const APP_DIR: &str = "prompt-forge";

/// 覆盖配置目录的环境变量名。
///
/// 为什么需要它：默认路径落在用户真实配置目录，任何自动化测试（哪怕只是想
/// 「启动一下看看」）都会读写用户的真实 `config.json` —— 实测已发生过一次：
/// 审计脚本为验证写入侧闸门而启动真实服务，把用户真实配置覆盖成测试假档位，
/// 且原档位与 DPAPI 密文不可逆丢失（原子写无 `.bak` 残留）。
///
/// 提供覆盖入口后，测试只需给子进程注入该变量即可完全隔离，不必再靠
/// 逐平台伪造 `APPDATA` / `XDG_CONFIG_HOME` / `HOME`（那只覆盖了目录的**父级**，
/// 仍然共享同一个 `prompt-forge` 子目录名，多个测试并行时仍会互相踩）。
pub const CONFIG_DIR_ENV: &str = "PROMPT_FORGE_CONFIG_DIR";

/// 返回配置目录（不保证存在）
pub fn config_dir() -> PathBuf {
    // 覆盖入口优先于一切平台默认规则。空值视为未设置（与 XDG_CONFIG_HOME 同规则）。
    if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join(APP_DIR);
        }
        // APPDATA 缺失（极少见）：退化到用户目录
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(home).join("AppData").join("Roaming").join(APP_DIR);
        }
        // APPDATA 与 USERPROFILE 都缺失（极少见）：退到系统临时目录。
        // 必须用绝对路径——若退化成 `prompt-forge` 这样的相对路径，
        // 会随进程工作目录变化，导致从不同目录启动时读写到不同的配置目录（配置分裂）。
        std::env::temp_dir().join(APP_DIR)
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join(APP_DIR);
        }
        // HOME 缺失（极少见）：退到系统临时目录（绝对路径，避免相对路径导致配置分裂）
        std::env::temp_dir().join(APP_DIR)
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
        // HOME 缺失（极少见）：退到系统临时目录（绝对路径，避免相对路径导致配置分裂）
        std::env::temp_dir().join(APP_DIR)
    }
}

/// 配置文件完整路径
pub fn config_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// 环境变量是**进程级**的：Rust 测试默认多线程并行，同一进程内多个测试同时
    /// `set_var` / `remove_var` 同一个 key 会互相踩（实测表现为断言读到别的测试
    /// 留下的值，flaky 且难复现）。
    ///
    /// 这里用一把进程内互斥锁把「改环境变量 + 断言」整段串行化；
    /// `OnceLock` 保证锁本身只初始化一次（`Mutex::new` 不是 const，不能用于 static）。
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 覆盖入口必须**直接**指向配置目录本身（不再追加 `prompt-forge` 子目录），
    /// 否则自动化测试会写到 `<临时目录>/prompt-forge/` 下，与调用方预期不符。
    #[test]
    fn config_dir_env_overrides_platform_default_verbatim() {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join("pf-cfgdir-test-override");
        std::env::set_var(CONFIG_DIR_ENV, &dir);
        let got = config_dir();
        std::env::remove_var(CONFIG_DIR_ENV);
        assert_eq!(got, dir, "覆盖入口必须原样使用，不得追加子目录");
    }

    /// 空值视为未设置，退回平台默认规则（与 XDG_CONFIG_HOME 的空值处理一致）。
    #[test]
    fn config_dir_env_empty_falls_back_to_platform_default() {
        let _guard = env_lock();
        std::env::set_var(CONFIG_DIR_ENV, "");
        let got = config_dir();
        std::env::remove_var(CONFIG_DIR_ENV);
        // 平台默认一定带 `prompt-forge` 子目录；覆盖入口则不会
        assert!(
            got.ends_with(APP_DIR),
            "空覆盖值应退回平台默认（应含 prompt-forge 子目录），实际：{:?}",
            got
        );
    }

    /// `config_path()` 必须在覆盖目录之下，且文件名固定。
    #[test]
    fn config_path_uses_overridden_dir_and_fixed_filename() {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join("pf-cfgdir-test-path");
        std::env::set_var(CONFIG_DIR_ENV, &dir);
        let got = config_path();
        std::env::remove_var(CONFIG_DIR_ENV);
        assert_eq!(got, dir.join(CONFIG_FILE));
    }
}
