//! 角色系统：内置角色 + 用户自定义角色。
//!
//! 设计对标 DSH prompt-enhancer 的「角色」概念：一个角色 = 一段 System Prompt 前置，
//! 与「增强模式」(modes.rs)、「增强方式」(presets.rs) 三者正交组合：
//! - 模式决定「增强成什么结构」
//! - 方式决定「增强到什么程度」
//! - 角色决定「以谁的身份与口吻产出」
//!
//! id 约定：内置角色 `builtin-<name>`；自定义角色 `custom-<id>`
//! （存取时自动加前缀，避免与内置 id 冲突）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinRole {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// 作为 System Prompt 前置注入出站请求
    pub system_prompt: &'static str,
}

/// 内置角色表。第一个为系统默认角色。
pub const ROLES: &[BuiltinRole] = &[
    BuiltinRole {
        id: "builtin-general",
        name: "通用助手",
        description: "系统默认角色：中性克制，不预设行业背景",
        system_prompt: "你是一名专业的提示词工程助手。改写用户提示词时保持中立客观：不臆造用户未提供的行业背景、团队规模或技术栈；输出即为最终可直接使用的提示词本体，不附加解释、前后缀或 Markdown 围栏。",
    },
    BuiltinRole {
        id: "builtin-expert",
        name: "资深技术专家",
        description: "面向工程场景，强调技术约束与可落地性",
        system_prompt: "你是一名资深技术专家，为工程团队编写可落地的提示词。你会主动关注：技术栈与版本约束、输入输出契约、异常与边界情况、性能与安全影响、依赖与兼容性。产出偏向工程规范语言，结构清晰、术语准确，不做臆测性的技术选型。",
    },
    BuiltinRole {
        id: "builtin-reviewer",
        name: "代码评审官",
        description: "面向代码质量与缺陷发现，强调可验证性",
        system_prompt: "你是一名严格的代码评审官。你的产出关注正确性、边界条件、错误处理、并发与资源释放、可测试性与可读性。评审倾向指出具体问题位置与触发条件，而不是笼统评价。",
    },
    BuiltinRole {
        id: "builtin-product",
        name: "产品经理",
        description: "面向需求描述，强调用户价值与验收标准",
        system_prompt: "你是一名资深产品经理。你的产出关注用户场景与真实诉求、功能优先级、范围边界、验收标准与衡量指标。语言面向跨团队协作，避免技术实现细节堆砌。",
    },
    BuiltinRole {
        id: "builtin-writer",
        name: "技术文档写手",
        description: "面向文档与教程，强调可读性与步骤完整性",
        system_prompt: "你是一名技术文档写手。你的产出追求表述准确、层次分明、示例可复现、步骤可跟随。避免长句堆砌与营销化措辞，术语首次出现时给出简短解释。",
    },
];

/// 系统默认角色 id
pub const DEFAULT_ROLE_ID: &str = "builtin-general";

pub fn find_builtin(id: &str) -> Option<&'static BuiltinRole> {
    ROLES.iter().find(|r| r.id == id)
}

/// 用户自定义角色（序列化进 config.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomRole {
    /// 不含 "custom-" 前缀（存取时自动加，避免与内置 id 冲突）
    pub id: String,
    pub name: String,
    /// System Prompt 正文
    pub system_prompt: String,
}

impl CustomRole {
    pub fn full_id(&self) -> String {
        format!("custom-{}", self.id)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("自定义角色 id 不能为空".into());
        }
        if ROLES.iter().any(|r| r.id == self.id) {
            return Err(format!("自定义角色 id「{}」与内置角色冲突", self.id));
        }
        if self.name.trim().is_empty() {
            return Err("自定义角色名称不能为空".into());
        }
        if self.system_prompt.trim().is_empty() {
            return Err("自定义角色的角色设定不能为空".into());
        }
        Ok(())
    }
}

/// 解析后的角色（内置与自定义统一形态，供前端展示与后端注入）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedRole {
    pub id: String,
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    /// true = 内置角色，false = 用户自定义
    pub builtin: bool,
}

/// 解析角色 id → 具体角色。
/// 优先级：内置 id → 自定义 id → 默认角色（未知 id 静默回落到默认，不报错，保证增强链路不中断）。
pub fn resolve(role_id: &str, customs: &[CustomRole]) -> ResolvedRole {
    if let Some(r) = find_builtin(role_id) {
        return ResolvedRole {
            id: r.id.to_string(),
            name: r.name.to_string(),
            description: r.description.to_string(),
            system_prompt: r.system_prompt.to_string(),
            builtin: true,
        };
    }
    if let Some(custom_id) = role_id.strip_prefix("custom-") {
        if let Some(c) = customs.iter().find(|c| c.id == custom_id) {
            return ResolvedRole {
                id: c.full_id(),
                name: c.name.clone(),
                description: "[自定义角色]".to_string(),
                system_prompt: c.system_prompt.clone(),
                builtin: false,
            };
        }
    }
    // 未知 id → 回落默认角色
    let d = find_builtin(DEFAULT_ROLE_ID).expect("默认内置角色必须存在");
    ResolvedRole {
        id: d.id.to_string(),
        name: d.name.to_string(),
        description: d.description.to_string(),
        system_prompt: d.system_prompt.to_string(),
        builtin: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_role_exists() {
        assert!(find_builtin(DEFAULT_ROLE_ID).is_some());
    }

    #[test]
    fn resolve_builtin() {
        let r = resolve("builtin-expert", &[]);
        assert_eq!(r.id, "builtin-expert");
        assert!(r.builtin);
        assert!(!r.system_prompt.is_empty());
    }

    #[test]
    fn resolve_custom() {
        let customs = vec![CustomRole {
            id: "sec".into(),
            name: "安全审计".into(),
            system_prompt: "关注注入与越权".into(),
        }];
        let r = resolve("custom-sec", &customs);
        assert_eq!(r.id, "custom-sec");
        assert!(!r.builtin);
        assert_eq!(r.system_prompt, "关注注入与越权");
    }

    #[test]
    fn unknown_id_falls_back_to_default() {
        let r = resolve("custom-not-exist", &[]);
        assert_eq!(r.id, DEFAULT_ROLE_ID);
        assert!(r.builtin);
    }

    #[test]
    fn custom_role_validate() {
        let ok = CustomRole {
            id: "a".into(),
            name: "n".into(),
            system_prompt: "p".into(),
        };
        assert!(ok.validate().is_ok());
        let conflict = CustomRole {
            id: "builtin-general".into(),
            name: "n".into(),
            system_prompt: "p".into(),
        };
        assert!(conflict.validate().is_err());
        let empty = CustomRole {
            id: "b".into(),
            name: "n".into(),
            system_prompt: "  ".into(),
        };
        assert!(empty.validate().is_err());
    }
}
