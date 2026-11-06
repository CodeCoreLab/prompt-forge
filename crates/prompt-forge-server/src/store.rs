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
use std::sync::Mutex;

use prompt_forge_core::config::AppConfig;
use prompt_forge_core::crypto;

use crate::paths::config_dir;

const HISTORY_FILE: &str = "history.json";
/// 历史容量上限（与前端提示「最近 20 条」一致）
pub const HISTORY_CAP: usize = 20;

/// 历史读-改-写互斥锁。
///
/// `push_history` 是「读全量 → 插入头部 → 整体写回」的复合操作，
/// 没有锁时两个并发请求会互相覆盖：后写入者基于自己读到的旧快照落盘，
/// 前者的那条历史凭空消失（丢历史，且用户无感知）。
static HISTORY_LOCK: Mutex<()> = Mutex::new(());

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

/// 把 core 层缓冲的告警排空并打到 stderr。
///
/// core 层遵守「零日志 I/O」约束（不写 stdout/stderr），
/// DPAPI 相关的告警（如「DPAPI 缓冲区未释放」）都进内存缓冲，
/// **必须由 server 层取走**，否则用户永远看不到这些告警。
fn flush_core_warnings() {
    for w in crypto::drain_warnings() {
        eprintln!("[prompt-forge] {w}");
    }
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
        Ok(cfg) => {
            flush_core_warnings();
            Ok(cfg)
        }
        Err(e) => {
            // 损坏数据留档 quarantine：绝不静默丢弃
            // 留档文件名带时间戳：否则每次启动都会覆盖同一份留档，
            // 第二次损坏时第一次的现场证据已被冲掉。
            let corrupt = corrupt_archive_path(&path);
            if let Err(e2) = fs::copy(&path, &corrupt) {
                eprintln!(
                    "config.json 解析失败（留档失败 {}: {e2}）：{e}",
                    corrupt.display()
                );
            } else {
                eprintln!("config.json 解析失败（已留档 {}）：{e}", corrupt.display());
            }
            flush_core_warnings();
            Ok(AppConfig::default())
        }
    }
}

/// 损坏留档的目标路径：`config.json.corrupt-<unix时间戳>`。
///
/// 带时间戳而不是固定名，避免后续损坏覆盖前一次现场。
fn corrupt_archive_path(path: &Path) -> PathBuf {
    let stamp = now_secs();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.json".to_string());
    path.with_file_name(format!("{name}.corrupt-{stamp}"))
}

/// 保存配置：入口校验一次 → key 加密 → 原子写
///
/// 加密失败**整体失败**（不写盘）：宁可让用户重试，也不把 API Key 明文落盘。
pub fn save_config(cfg: &AppConfig) -> Result<(), String> {
    cfg.validate()?; // 入口校验
    let mut owned = cfg.clone();
    // 落盘前把明文 key 加密（已是 enc: 形态的原样保留，避免重复加密）
    for p in &mut owned.profiles {
        if !crypto::is_encrypted(&p.api_key) {
            p.api_key = crypto::store_key(&p.api_key)
                .map_err(|e| format!("配置「{}」的 API Key 加密失败，已中止保存: {e}", p.name))?;
        }
    }
    let dir = ensure_config_dir()?;
    let path = dir.join(prompt_forge_core::config::CONFIG_FILE);
    let json = serde_json::to_string_pretty(&owned).map_err(|e| e.to_string())?;
    atomic_write(&path, json.as_bytes())?;
    flush_core_warnings();
    Ok(())
}

/// 读取配置（返回给前端的形态：API Key 解密为明文，供前端编辑显示）
///
/// 解密失败**显式报错**（不静默变空串）：密文损坏通常意味着配置被拷贝到别的机器/用户，
/// 必须让用户看到原因，而不是面对一个莫名其妙的 401。
pub fn load_config_for_client() -> Result<AppConfig, String> {
    let mut cfg = load_config()?;
    for p in &mut cfg.profiles {
        if crypto::is_encrypted(&p.api_key) {
            p.api_key = crypto::load_key(&p.api_key).map_err(|e| {
                format!("配置「{}」的 API Key 解密失败: {e}", p.name)
            })?;
        }
    }
    flush_core_warnings();
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
///
/// 整个「读 → 插入 → 写回」在 `HISTORY_LOCK` 内完成：
/// 没有锁时并发请求会各自基于旧快照写回，**互相覆盖导致丢历史**。
pub fn push_history(entry: HistoryEntry) {
    let _guard = HISTORY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = load_history(); // 新→旧
    list.insert(0, entry);
    list.truncate(HISTORY_CAP);
    // 落盘为旧→新（append 语义）
    let mut stored = list;
    stored.reverse();
    if let Ok(dir) = ensure_config_dir() {
        let path = dir.join(HISTORY_FILE);
        if let Ok(json) = serde_json::to_string_pretty(&stored) {
            let _ = atomic_write(&path, json.as_bytes());
        }
    }
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
