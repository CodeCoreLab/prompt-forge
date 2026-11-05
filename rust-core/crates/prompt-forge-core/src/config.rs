//! 配置模型与校验（纯逻辑，无 I/O）。
//!
//! 文件读写（load/save/原子写/quarantine）留在主 crate——它们依赖 Tauri 路径解析；
//! 本模块只承载结构与校验规则，使校验逻辑可独立测试。

use serde::{Deserialize, Serialize};
use crate::modes::CustomMode;

pub const CONFIG_FILE: &str = "config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// 用户可读的配置名，如 "DeepSeek" / "本地 Ollama"
    pub name: String,
    /// 协议: "openai" | "anthropic" | "ollama" | "gemini"
    pub protocol: String,
    pub base_url: String,
    pub model: String,
    /// 缺省为空（Ollama 本地无需 key）
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// 单次请求墙钟上限（秒）；0 = 默认 120
    #[serde(default)]
    pub timeout_secs: u64,
    /// 输出上限（tokens）；0 = 自动（按输入长度 ×4 估算，上限 4096）
    #[serde(default)]
    pub max_tokens: u32,
}

impl LlmConfig {
    /// 已知协议集合（llm.rs 按此分发）
    pub const PROTOCOLS: &'static [&'static str] = &["openai", "anthropic", "ollama", "gemini"];

    /// 生效超时（秒）：未配置时 120
    pub fn effective_timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 { 120 } else { self.timeout_secs }
    }

    /// 生效 max_tokens：0 = 自动估算（输入字符 ≈ 每 2 字符 1 token 译文侧，
    /// 增强输出通常为输入的 2~4 倍，取 ×4 上限 4096）
    pub fn effective_max_tokens(&self, prompt_chars: usize) -> u32 {
        if self.max_tokens > 0 {
            return self.max_tokens;
        }
        let estimated = (prompt_chars as u32 / 2).saturating_mul(4);
        estimated.clamp(1024, 4096)
    }
}

fn default_temperature() -> f32 {
    0.7
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 当前生效的配置索引
    pub active_index: usize,
    pub profiles: Vec<LlmConfig>,
    #[serde(default)]
    pub close_to_tray: bool,
    #[serde(default = "default_true")]
    pub autostart: bool,
    /// 用户自定义增强模式（P1-1 模板外置）
    #[serde(default)]
    pub custom_modes: Vec<CustomMode>,
    /// 历史记录（最近 20 条）
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub original: String,
    pub enhanced: String,
    pub mode: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 当前生效的配置索引
    pub active_index: usize,
    pub profiles: Vec<LlmConfig>,
    #[serde(default)]
    pub close_to_tray: bool,
    #[serde(default = "default_true")]
    pub autostart: bool,
    /// 用户自定义增强模式（P1-1 模板外置）
    #[serde(default)]
    pub custom_modes: Vec<CustomMode>,
    /// 历史记录（最近 20 条）
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_index: 0,
            profiles: Vec::new(),
            close_to_tray: true,
            autostart: true,
            custom_modes: Vec::new(),
            history: Vec::new(),
        }
    }
}

impl AppConfig {
    /// 入口校验：协议合法性 + active_index 越界 + 自定义模式合法性
    pub fn validate(&self) -> Result<(), String> {
        if let Some(p) = self.profiles.get(self.active_index) {
            if !LlmConfig::PROTOCOLS.contains(&p.protocol.as_str()) {
                return Err(format!(
                    "配置「{}」协议非法: {}（可选: {}）",
                    p.name,
                    p.protocol,
                    LlmConfig::PROTOCOLS.join(" / ")
                ));
            }
        } else if self.active_index != 0 {
            return Err("active_index 越界".into());
        }
        // 自定义模式 id 全局唯一
        let mut seen = std::collections::HashSet::new();
        for m in &self.custom_modes {
            m.validate()?;
            if !seen.insert(m.id.clone()) {
                return Err(format!("自定义模式 id 重复: {}", m.id));
            }
        }
        Ok(())
    }

    pub fn active(&self) -> Option<&LlmConfig> {
        self.profiles.get(self.active_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(protocol: &str, index: usize, n: usize) -> AppConfig {
        AppConfig {
            active_index: index,
            profiles: (0..n)
                .map(|i| LlmConfig {
                    name: format!("p{i}"),
                    protocol: protocol.into(),
                    base_url: "https://x".into(),
                    model: "m".into(),
                    api_key: String::new(),
                    temperature: 0.7,
                    timeout_secs: 0,
                    max_tokens: 0,
                })
                .collect(),
            close_to_tray: true,
            autostart: true,
            custom_modes: Vec::new(),
        }
    }

    #[test]
    fn timeout_and_max_tokens_defaults() {
        let mut c = cfg("openai", 0, 1).profiles.remove(0);
        assert_eq!(c.effective_timeout_secs(), 120);
        assert_eq!(c.effective_max_tokens(1000), 2000); // 1000/2*4 = 2000
        assert_eq!(c.effective_max_tokens(100), 1024); // 下限 clamp
        assert_eq!(c.effective_max_tokens(100000), 4096); // 上限 clamp
        c.timeout_secs = 30;
        c.max_tokens = 2048;
        assert_eq!(c.effective_timeout_secs(), 30);
        assert_eq!(c.effective_max_tokens(100000), 2048); // 显式配置优先
    }

    #[test]
    fn validate_accepts_known_protocol() {
        assert!(cfg("openai", 0, 1).validate().is_ok());
        assert!(cfg("anthropic", 0, 1).validate().is_ok());
    }

    #[test]
    fn validate_rejects_unknown_protocol() {
        let e = cfg("grpc", 0, 1).validate().unwrap_err();
        assert!(e.contains("grpc"));
    }

    #[test]
    fn validate_rejects_out_of_range_index() {
        assert!(cfg("openai", 3, 1).validate().is_err());
        // 空列表 + index 0 合法（未配置态）
        assert!(cfg("openai", 0, 0).validate().is_ok());
    }
}
