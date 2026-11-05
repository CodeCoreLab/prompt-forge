//! 配置与历史的持久化层（唯一写入方）。
//!
//! 设计要点：
//! - **单一真源**：只有本模块读写 `config.json`，其他层一律通过 API 拿结构体。
//! - **原子写**：先写 `.tmp` 再 rename，避免写一半崩溃损坏配置。
//! - **损坏留档**：解析失败时复制为 `config.json.corrupt`，返回默认值让应用仍可用，
//!   用户可从留档人工找回（绝不静默丢弃数据）。
//! - **DPAPI 闭环**：落盘前 `store_key` 加密（`enc:` 前缀），读取时 `load_key` 解密；
//!   已是 `enc:` 形态的原样保留，避免重复加密。
//!
//! 历史记录独立存 `history.json`，与配置解耦：清空配置不影响历史。

use std::fs;
use std::path::{Path, PathBuf};

use prompt_forge_core::config::AppConfig;
use prompt_forge_core::crypto;

use crate::paths::config_dir;

const HISTORY_FILE: &str = "history.json";
/// 历史容量上限（与前端提示「最近 20 条」一致）
pub const HISTORY_CAP: usize = 20;

/// 一条历史记录
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    pub input: String,
    pub mode_id: String,
    pub output: String,
    /// unix 秒
    pub ts: u64,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 配置目录（确保存在）
fn ensure_config_dir() -> Result<PathBuf, String> {
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    Ok(dir)
}

/// 读取配置：不存在返回默认值；解析失败留档并返回默认值
pub fn load_config() -> Result<AppConfig, String> {
    let path = config_dir().join(prompt_forge_core::config::CONFIG_FILE);
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let raw = fs::read_to_string(&path).map_err(|e| format!("读取配置失败: {e}"))?;
    match serde_json::from_str::<AppConfig>(&raw) {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            // 损坏数据留档 quarantine：绝不静默丢弃
            let corrupt = path.with_extension("json.corrupt");
            let _ = fs::copy(&path, &corrupt);
            eprintln!("config.json 解析失败（已留档 {}）：{e}", corrupt.display());
            Ok(AppConfig::default())
        }
    }
}

/// 保存配置：入口校验一次 → key 加密 → 原子写
pub fn save_config(cfg: &AppConfig) -> Result<(), String> {
    cfg.validate()?; // 入口校验
    let mut owned = cfg.clone();
    // 落盘前把明文 key 加密（已是 enc: 形态的原样保留，避免重复加密）
    for p in &mut owned.profiles {
        if !crypto::is_encrypted(&p.api_key) {
            p.api_key = crypto::store_key(&p.api_key);
        }
    }
    let dir = ensure_config_dir()?;
    let path = dir.join(prompt_forge_core::config::CONFIG_FILE);
    let json = serde_json::to_string_pretty(&owned).map_err(|e| e.to_string())?;
    atomic_write(&path, json.as_bytes())
}

/// 读取配置（返回给前端的形态：API Key 解密为明文，供前端编辑显示）
pub fn load_config_for_client() -> Result<AppConfig, String> {
    let mut cfg = load_config()?;
    for p in &mut cfg.profiles {
        p.api_key = crypto::load_key(&p.api_key);
    }
    Ok(cfg)
}

/// 读取历史（最新在前）
pub fn load_history() -> Vec<HistoryEntry> {
    let path = config_dir().join(HISTORY_FILE);
    let Ok(raw) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<HistoryEntry>>(&raw) {
        Ok(mut v) => {
            v.reverse(); // 落盘为旧→新，读出转为新→旧
            v
        }
        Err(_) => Vec::new(),
    }
}

/// 追加一条历史并落盘（超限驱逐最旧）
pub fn push_history(entry: HistoryEntry) {
    let mut list = load_history(); // 新→旧
    list.insert(0, entry);
    list.truncate(HISTORY_CAP);
    // 落盘为旧→新（append 语义）
    let mut stored = list;
    stored.reverse();
    let path = config_dir().join(HISTORY_FILE);
    if let Ok(dir) = ensure_config_dir() {
        let path = dir.join(HISTORY_FILE);
        if let Ok(json) = serde_json::to_string_pretty(&stored) {
            let _ = atomic_write(&path, json.as_bytes());
        }
    }
    let _ = path; // 目录不存在时上面的 ensure_config_dir 已兜底
}

/// 原子写入：先写同目录临时文件，再 rename 覆盖
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| "配置路径无父目录".to_string())?;
    fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("写入临时文件失败: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("提交配置文件失败: {e}"))
}
