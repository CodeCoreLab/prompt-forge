//! 配置模型与校验（纯逻辑，无 I/O）。
//!
//! 文件读写（load/save/原子写/quarantine）留在主 crate——它们依赖 Tauri 路径解析；
//! 本模块只承载结构与校验规则，使校验逻辑可独立测试。

use serde::{Deserialize, Serialize};
use crate::methods::{DEFAULT_METHOD_ID, DEFAULT_THINKING_ID, ThinkingLevel};
use crate::modes::CustomMode;
use crate::roles::{CustomRole, DEFAULT_ROLE_ID};

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

/// 增强选项：增强方式 + 深度思考强度 + 当前生效角色。
///
/// 三者与「增强模式」正交，构成完整的增强指令：
/// - `method_id`：增强方式（通用/深度/代码/创意/严谨）
/// - `thinking`：深度思考强度（off/low/standard/deep/max）
/// - `role_id`：当前生效角色（builtin-* 或 custom-*）
///
/// 全部带 `serde(default)`，旧配置文件缺字段时自动回落默认值（向后兼容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhanceOptions {
    #[serde(default = "default_method_id")]
    pub method_id: String,
    #[serde(default = "default_thinking_id")]
    pub thinking: String,
    #[serde(default = "default_role_id")]
    pub role_id: String,
}

fn default_method_id() -> String {
    DEFAULT_METHOD_ID.to_string()
}
fn default_thinking_id() -> String {
    DEFAULT_THINKING_ID.to_string()
}
fn default_role_id() -> String {
    DEFAULT_ROLE_ID.to_string()
}

impl Default for EnhanceOptions {
    fn default() -> Self {
        Self {
            method_id: default_method_id(),
            thinking: default_thinking_id(),
            role_id: default_role_id(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 当前生效的配置索引
    pub active_index: usize,
    pub profiles: Vec<LlmConfig>,
    #[serde(default)]
    pub close_to_tray: bool,
    /// 开机自启开关。
    ///
    /// **serde 缺省值与 `Default::default()` 初值故意不同**：
    /// - `#[serde(default)]` → 缺字段时反序列化为 `false`：第三方调用 / 旧版前端 /
    ///   DNS rebinding 场景提交的配置**不得**因为「没写这个字段」就静默开启开机自启
    ///   （server 层 `save_config` 会据此写注册表 Run 键）。
    /// - `Default::default()` → 首次启动（磁盘上还没有配置文件）时为 `true`：
    ///   产品预期的新装默认开启自启，属既有行为，保持不变。
    #[serde(default)]
    pub autostart: bool,
    /// 用户自定义增强模式（P1-1 模板外置）
    #[serde(default)]
    pub custom_modes: Vec<CustomMode>,
    /// 用户自定义角色（角色设定外置）
    #[serde(default)]
    pub custom_roles: Vec<CustomRole>,
    /// 增强选项（增强方式 / 思考强度 / 生效角色）
    #[serde(default)]
    pub enhance: EnhanceOptions,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_index: 0,
            profiles: Vec::new(),
            close_to_tray: true,
            autostart: true,
            custom_modes: Vec::new(),
            custom_roles: Vec::new(),
            enhance: EnhanceOptions::default(),
        }
    }
}

impl AppConfig {
    /// 入口校验：协议合法性 + active_index 越界 + 自定义模式/角色合法性
    ///
    /// 协议校验覆盖**全部** profile，而不只是当前激活的那一个 ——
    /// server 层 `save_config` 是拿整份配置落盘的，非激活档位的 protocol 拼错
    /// 会一直潜伏，直到用户切到那一档才 400。
    pub fn validate(&self) -> Result<(), String> {
        // 1) 全部 profile 的协议合法性（含非激活档位）
        for (i, p) in self.profiles.iter().enumerate() {
            if !LlmConfig::PROTOCOLS.contains(&p.protocol.as_str()) {
                return Err(format!(
                    "配置[{}]「{}」协议非法: {}（可选: {}）",
                    i,
                    p.name,
                    p.protocol,
                    LlmConfig::PROTOCOLS.join(" / ")
                ));
            }
        }
        // 2) active_index 越界检查
        //    空列表 + index 0 = 未配置态（合法）；空列表 + index>0 = 越界（非法）
        if self.profiles.is_empty() {
            if self.active_index != 0 {
                return Err("active_index 越界".into());
            }
        } else if self.active_index >= self.profiles.len() {
            return Err("active_index 越界".into());
        }
        // 3) 自定义模式 id 全局唯一 + id 前缀规则
        let mut seen = std::collections::HashSet::new();
        for m in &self.custom_modes {
            m.validate()?;
            validate_custom_id("自定义模式", &m.id)?;
            if !seen.insert(m.id.clone()) {
                return Err(format!("自定义模式 id 重复: {}", m.id));
            }
        }
        // 4) 自定义角色 id 全局唯一 + id 前缀规则
        let mut seen_roles = std::collections::HashSet::new();
        for r in &self.custom_roles {
            r.validate()?;
            validate_custom_id("自定义角色", &r.id)?;
            if !seen_roles.insert(r.id.clone()) {
                return Err(format!("自定义角色 id 重复: {}", r.id));
            }
        }
        // 5) 增强选项合法性
        if crate::methods::find_method(&self.enhance.method_id).is_none() {
            return Err(format!(
                "增强方式非法: {}（可选: {}）",
                self.enhance.method_id,
                crate::methods::METHODS
                    .iter()
                    .map(|m| m.id)
                    .collect::<Vec<_>>()
                    .join(" / ")
            ));
        }
        // 6) 全部 profile 的接口地址合法性
        //    复用 core 内同一闸门（endpoint::validate_base_url），避免两套规则漂移。
        //    空地址放行：未填写的空档位不该在「保存整份配置」时被误判非法
        //    （真正的出站前拦截在 server 侧 checked_base_url 与写入侧闸门）。
        for (i, p) in self.profiles.iter().enumerate() {
            if p.base_url.trim().is_empty() {
                continue;
            }
            crate::endpoint::validate_base_url(&p.base_url)
                .map_err(|e| format!("配置[{}]「{}」接口地址非法: {}", i, p.name, e))?;
        }
        // 7) 深度思考强度必须可解析（未知值不得静默回落标准档）
        ThinkingLevel::parse_strict(&self.enhance.thinking)
            .map_err(|e| format!("深度思考强度非法: {}", e))?;
        // 8) 生效角色必须存在（内置或自定义）
        crate::roles::resolve_strict(&self.enhance.role_id, &self.custom_roles)
            .map_err(|_| format!("生效角色不存在: {}", self.enhance.role_id))?;
        Ok(())
    }

    pub fn active(&self) -> Option<&LlmConfig> {
        self.profiles.get(self.active_index)
    }
}

/// 自定义模式 / 自定义角色的 id 规则校验。
///
/// 存储约定：id **不含** `custom-` 前缀（`full_id()` 会自动加上），
/// 所以用户手填 `custom-x` 会让 `full_id()` 变成 `custom-custom-x`，
/// 前缀解析与去重判定都会出现歧义 —— 这里直接拒绝。
///
/// 其余约束（避免 id 进入 URL / HTML 上下文时引发转义与解析问题）：
/// - 不得为空、不得含空白字符
/// - 不得含路径或分隔符：`/ \ : * ? " < > |`
/// - 不得以 `-` 开头或结尾（避免 id 前缀解析歧义）
/// - 只允许字母、数字、`-`、`_`、`.`
fn validate_custom_id(kind: &str, id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err(format!("{kind} id 不能为空"));
    }
    if id != id.trim() {
        return Err(format!("{kind} id「{id}」不能包含首尾空白"));
    }
    if id.starts_with("custom-") {
        return Err(format!(
            "{kind} id「{id}」不能以 \"custom-\" 开头（前缀由系统自动添加）"
        ));
    }
    if id.starts_with('-') || id.ends_with('-') {
        return Err(format!("{kind} id「{id}」不能以 \"-\" 开头或结尾"));
    }
    if id.chars().any(|c| c.is_whitespace()) {
        return Err(format!("{kind} id「{id}」不能包含空白字符"));
    }
    if let Some(c) = id
        .chars()
        .find(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return Err(format!(
            "{kind} id「{id}」不能包含路径或分隔符字符「{c}」"
        ));
    }
    if let Some(c) = id
        .chars()
        .find(|c| !(c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.'))
    {
        return Err(format!(
            "{kind} id「{id}」只能包含字母、数字、\"-\"、\"_\"、\".\"（非法字符「{c}」）"
        ));
    }
    Ok(())
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
            custom_roles: Vec::new(),
            enhance: EnhanceOptions::default(),
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

    /// 回归：非激活档位的协议非法也必须被发现（整份配置会一起落盘）
    #[test]
    fn validate_rejects_unknown_protocol_in_inactive_profile() {
        let mut c = cfg("openai", 0, 3);
        c.profiles[2].protocol = "grpc".into();
        c.profiles[2].name = "坏档位".into();
        let e = c.validate().unwrap_err();
        assert!(e.contains("grpc"), "应指出非法协议: {e}");
        assert!(e.contains("坏档位"), "应指出是哪个配置: {e}");
        assert!(e.contains('2'), "应指出第几个配置: {e}");
    }

    #[test]
    fn validate_rejects_out_of_range_index() {
        assert!(cfg("openai", 3, 1).validate().is_err());
        // 空列表 + index 0 合法（未配置态）
        assert!(cfg("openai", 0, 0).validate().is_ok());
        // 空列表 + index>0 越界
        assert!(cfg("openai", 2, 0).validate().is_err());
        // 非空列表 + index 越界
        assert!(cfg("openai", 2, 2).validate().is_err());
    }

    /// serde 缺省：缺 `autostart` 字段 = false（不得静默开启开机自启）
    #[test]
    fn deserialized_autostart_defaults_to_false_when_field_missing() {
        let json = r#"{"active_index":0,"profiles":[]}"#;
        let c: AppConfig = serde_json::from_str(json).expect("应能反序列化");
        assert!(!c.autostart, "缺字段的 autostart 必须为 false");
    }

    /// 首次启动（无配置文件）走 Default::default() → autostart = true（产品默认）
    #[test]
    fn default_config_enables_autostart() {
        assert!(AppConfig::default().autostart);
    }

    fn custom_mode(id: &str) -> CustomMode {
        CustomMode {
            id: id.into(),
            name: "名称".into(),
            kind: "extend".into(),
            template: "模板".into(),
        }
    }

    fn custom_role(id: &str) -> CustomRole {
        CustomRole {
            id: id.into(),
            name: "名称".into(),
            system_prompt: "设定".into(),
        }
    }

    #[test]
    fn validate_accepts_normal_custom_ids() {
        let mut c = cfg("openai", 0, 1);
        c.custom_modes = vec![custom_mode("my-mode_1.v2")];
        c.custom_roles = vec![custom_role("role_a")];
        assert!(c.validate().is_ok(), "正常 id 应通过");
    }

    /// 回归：手填 `custom-` 前缀会让 full_id() 变成 `custom-custom-x`
    #[test]
    fn validate_rejects_custom_prefix_in_id() {
        let mut c = cfg("openai", 0, 1);
        c.custom_modes = vec![custom_mode("custom-x")];
        let e = c.validate().unwrap_err();
        assert!(e.contains("custom-"), "应提示前缀问题: {e}");

        let mut c2 = cfg("openai", 0, 1);
        c2.custom_roles = vec![custom_role("custom-y")];
        assert!(c2.validate().is_err());
    }

    #[test]
    fn validate_rejects_illegal_custom_id_chars() {
        for bad in ["a/b", "a\\b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a b", "a\tb"] {
            let mut c = cfg("openai", 0, 1);
            c.custom_modes = vec![custom_mode(bad)];
            assert!(c.validate().is_err(), "含非法字符的 id 应被拒: {bad:?}");
        }
        for bad in ["-lead", "trail-", ""] {
            let mut c = cfg("openai", 0, 1);
            c.custom_roles = vec![custom_role(bad)];
            assert!(c.validate().is_err(), "非法 id 应被拒: {bad:?}");
        }
    }

    #[test]
    fn validate_rejects_duplicate_custom_ids() {
        let mut c = cfg("openai", 0, 1);
        c.custom_modes = vec![custom_mode("dup"), custom_mode("dup")];
        assert!(c.validate().is_err());
        let mut c2 = cfg("openai", 0, 1);
        c2.custom_roles = vec![custom_role("dup"), custom_role("dup")];
        assert!(c2.validate().is_err());
    }

    // ── base_url / thinking / role_id 校验 ──────────────────────

    /// 回归：接口地址非法必须在**保存配置时**就被发现（不能延迟到出站执行期）
    #[test]
    fn validate_rejects_illegal_base_url() {
        // 携带凭据
        let mut c = cfg("openai", 0, 1);
        c.profiles[0].base_url = "https://user:pass@evil.com".into();
        let e = c.validate().unwrap_err();
        assert!(e.contains("接口地址"), "应指出是接口地址问题: {e}");
        assert!(e.contains("p0"), "应指出是哪个配置: {e}");

        // 非 http(s) 协议
        let mut c = cfg("openai", 0, 1);
        c.profiles[0].base_url = "file:///C:/Windows/win.ini".into();
        assert!(c.validate().is_err());

        // 缺协议
        let mut c = cfg("openai", 0, 1);
        c.profiles[0].base_url = "evil.com".into();
        assert!(c.validate().is_err());

        // 畸形端口（冒号后非纯数字）
        let mut c = cfg("openai", 0, 1);
        c.profiles[0].base_url = "https://x.com:80x".into();
        assert!(c.validate().is_err(), "畸形端口应被拒");
    }

    /// 回归：非激活档位的 base_url 非法同样要被发现（整份配置一起落盘）
    #[test]
    fn validate_rejects_illegal_base_url_in_inactive_profile() {
        let mut c = cfg("openai", 0, 3);
        c.profiles[2].base_url = "https://user:pass@evil.com".into();
        c.profiles[2].name = "坏档位".into();
        let e = c.validate().unwrap_err();
        assert!(e.contains("坏档位"), "应指出是哪个配置: {e}");
        assert!(e.contains('2'), "应指出第几个配置: {e}");
    }

    /// 回归：空 base_url 的空档位不得被误判非法（首次启动的空配置）
    #[test]
    fn validate_accepts_empty_base_url() {
        let mut c = cfg("openai", 0, 1);
        c.profiles[0].base_url = "".into();
        assert!(c.validate().is_ok(), "空接口地址应放行");
        // 空白串同样放行
        c.profiles[0].base_url = "   ".into();
        assert!(c.validate().is_ok(), "空白接口地址应放行");
    }

    /// 回归：深度思考强度拼错不得静默回落标准档
    #[test]
    fn validate_rejects_unknown_thinking() {
        let mut c = cfg("openai", 0, 1);
        c.enhance.thinking = "xx".into();
        let e = c.validate().unwrap_err();
        assert!(e.contains("深度思考强度"), "应指出是思考强度问题: {e}");
        assert!(e.contains("xx"), "应指出非法值: {e}");
    }

    #[test]
    fn validate_accepts_known_thinking() {
        for t in ["off", "low", "standard", "deep", "max", "DEEP"] {
            let mut c = cfg("openai", 0, 1);
            c.enhance.thinking = t.into();
            assert!(c.validate().is_ok(), "合法强度应通过: {t}");
        }
    }

    /// 回归：生效角色指向不存在的角色必须报错（不能静默回落默认角色）
    #[test]
    fn validate_rejects_unknown_role() {
        let mut c = cfg("openai", 0, 1);
        c.enhance.role_id = "custom-not-exist".into();
        let e = c.validate().unwrap_err();
        assert!(e.contains("生效角色"), "应指出是角色问题: {e}");
        assert!(e.contains("custom-not-exist"), "应指出非法角色 id: {e}");
    }

    #[test]
    fn validate_accepts_builtin_and_existing_custom_role() {
        // 内置角色
        let mut c = cfg("openai", 0, 1);
        c.enhance.role_id = DEFAULT_ROLE_ID.into();
        assert!(c.validate().is_ok(), "内置角色应通过");
        // 已存在的自定义角色（配置里存 full_id 形态 custom-<id>）
        let mut c = cfg("openai", 0, 1);
        c.custom_roles = vec![custom_role("role_a")];
        c.enhance.role_id = "custom-role_a".into();
        assert!(c.validate().is_ok(), "已存在的自定义角色应通过");
    }
}
