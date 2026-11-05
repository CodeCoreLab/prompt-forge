//! 增强方式（Enhance Method）与深度思考强度（Thinking Level）。
//!
//! 对标 DSH prompt-enhancer 的「5 种模式：基础 / 轻量 / 标准 / 专家 / 一键发布」，
//! 但本作采用**正交分解**而非单一列表：
//!
//! - `EnhanceMethod`（增强方式）= **做什么**：通用 / 深度 / 代码 / 创意 / 严谨
//!   每种方式是一段策略段，叠加在「模式」策略段之后，共同决定增强的取向。
//! - `ThinkingLevel`（深度思考强度）= **想多深**：关闭 / 轻 / 标准 / 深度 / 极致
//!   通过协议级参数（reasoning_effort / thinking.budget_tokens / thinkingConfig）
//!   控制模型的推理预算，而非靠提示词措辞。
//!
//! 两者与 `modes`（增强模式：产出什么结构）三者正交，用户可任意组合。
//! 关键约束：**思考强度只影响模型推理预算，不改变保真纪律**——COMMON 硬规则始终置顶。

use serde::{Deserialize, Serialize};

// ── 增强方式 ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhanceMethod {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// 策略段，追加在模式策略之后、raw_prompt 之前
    pub strategy: &'static str,
}

pub const METHODS: &[EnhanceMethod] = &[
    EnhanceMethod {
        id: "general",
        name: "通用增强",
        description: "默认方式：结构清晰、要素完整、无冗余",
        strategy: r#"# 本次增强方式：通用增强

保持结构完整但不过度铺陈：补齐执行所需的关键要素（对象、范围、输出形态），
去除歧义与重复表述，不强行添加原文没有的要求。"#,
    },
    EnhanceMethod {
        id: "deep",
        name: "深度增强",
        description: "充分展开：补齐背景、约束、边界与验收标准",
        strategy: r#"# 本次增强方式：深度增强

进行充分展开：主动补齐执行前必须知道但原文未写的背景、约束、边界条件、
失败情形与验收标准。原文明确写出的内容一律原样保留，不因补充而改变其语义。

补充的信息若属于合理推断，用「如无特别说明」引出；无法推断的关键项用
「[待补充:……]」标注，不编造具体数值、日期、接口或人名。"#,
    },
    EnhanceMethod {
        id: "code",
        name: "代码增强",
        description: "面向编码任务：明确技术栈、输入输出与验收",
        strategy: r#"# 本次增强方式：代码增强

面向编码任务改写：明确目标行为、技术栈与版本约束、输入输出契约、
关键算法或流程、异常与边界处理、性能与安全注意事项、可验证的验收方式
（如测试用例或运行结果）。原文中已指定的语言、框架、函数名、文件路径一律原样保留。"#,
    },
    EnhanceMethod {
        id: "creative",
        name: "创意增强",
        description: "发散取向：提供多个思路与差异化的表达",
        strategy: r#"# 本次增强方式：创意增强

在忠实于原文意图的前提下，提供有差异的多个思路或表达方式，
让选择空间显式可见。不得为了多样性而偏离原文明确指定的对象、范围与硬性约束。"#,
    },
    EnhanceMethod {
        id: "rigorous",
        name: "严谨增强",
        description: "学术取向：强调精确、术语一致与可证伪",
        strategy: r#"# 本次增强方式：严谨增强

强调精确性：术语前后一致、量纲与单位明确、条件与结论的适用范围明确、
区分事实与推断、避免「大概」「可能」这类模糊表述替代具体条件。"#,
    },
];

pub fn find_method(id: &str) -> Option<&'static EnhanceMethod> {
    METHODS.iter().find(|m| m.id == id)
}

pub const DEFAULT_METHOD_ID: &str = "general";

// ── 深度思考强度 ────────────────────────────────────────────────

/// 思考强度分级。数值越大，模型分配的推理预算越多。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// 关闭：不传任何推理参数
    Off,
    /// 轻：适合简单改写，节省 token 与时延
    Low,
    /// 标准：默认平衡档
    Standard,
    /// 深度：适合复杂任务，显著增加推理预算
    Deep,
    /// 极致：最高预算，用于高难度任务
    Max,
}

impl ThinkingLevel {
    pub const ALL: &'static [ThinkingLevel] = &[
        ThinkingLevel::Off,
        ThinkingLevel::Low,
        ThinkingLevel::Standard,
        ThinkingLevel::Deep,
        ThinkingLevel::Max,
    ];

    pub fn id(&self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Standard => "standard",
            ThinkingLevel::Deep => "deep",
            ThinkingLevel::Max => "max",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ThinkingLevel::Off => "关闭",
            ThinkingLevel::Low => "轻",
            ThinkingLevel::Standard => "标准",
            ThinkingLevel::Deep => "深度",
            ThinkingLevel::Max => "极致",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ThinkingLevel::Off => "不传推理参数，最快最省",
            ThinkingLevel::Low => "少量推理，适合简单改写",
            ThinkingLevel::Standard => "平衡档，多数场景适用",
            ThinkingLevel::Deep => "较多推理，适合复杂任务",
            ThinkingLevel::Max => "最高推理预算，时延最长",
        }
    }

    /// 解析强度 id；未知值回落标准档（不报错，保证链路不中断）
    pub fn parse(s: &str) -> ThinkingLevel {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "disable" => ThinkingLevel::Off,
            "low" | "light" => ThinkingLevel::Low,
            "deep" | "high" => ThinkingLevel::Deep,
            "max" | "maximum" => ThinkingLevel::Max,
            _ => ThinkingLevel::Standard,
        }
    }

    /// 推理 token 预算（供 Anthropic thinking.budget_tokens 等使用）
    pub fn budget_tokens(&self) -> u32 {
        match self {
            ThinkingLevel::Off => 0,
            ThinkingLevel::Low => 1024,
            ThinkingLevel::Standard => 4096,
            ThinkingLevel::Deep => 16384,
            ThinkingLevel::Max => 32768,
        }
    }

    /// OpenAI 风格 reasoning_effort 取值（仅部分档位有对应值）
    pub fn openai_reasoning_effort(&self) -> Option<&'static str> {
        match self {
            ThinkingLevel::Off => None,
            ThinkingLevel::Low => Some("low"),
            ThinkingLevel::Standard => Some("medium"),
            ThinkingLevel::Deep => Some("high"),
            ThinkingLevel::Max => Some("high"),
        }
    }

    /// 是否需要向协议层注入推理参数
    pub fn is_on(&self) -> bool {
        !matches!(self, ThinkingLevel::Off)
    }
}

pub const DEFAULT_THINKING_ID: &str = "standard";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_lookup() {
        assert!(find_method("general").is_some());
        assert!(find_method("deep").is_some());
        assert!(find_method("nope").is_none());
    }

    #[test]
    fn thinking_parse_and_defaults() {
        assert_eq!(ThinkingLevel::parse("off"), ThinkingLevel::Off);
        assert_eq!(ThinkingLevel::parse("LOW"), ThinkingLevel::Low);
        assert_eq!(ThinkingLevel::parse("standard"), ThinkingLevel::Standard);
        assert_eq!(ThinkingLevel::parse("high"), ThinkingLevel::Deep);
        assert_eq!(ThinkingLevel::parse("garbage"), ThinkingLevel::Standard);
        assert_eq!(ThinkingLevel::parse(""), ThinkingLevel::Standard);
    }

    #[test]
    fn thinking_budget_monotonic() {
        let budgets: Vec<u32> = ThinkingLevel::ALL.iter().map(|l| l.budget_tokens()).collect();
        assert_eq!(budgets, vec![0, 1024, 4096, 16384, 32768]);
        // 档位递增 → 预算不减
        for w in budgets.windows(2) {
            assert!(w[1] >= w[0]);
        }
    }

    #[test]
    fn thinking_off_sends_no_params() {
        assert!(!ThinkingLevel::Off.is_on());
        assert!(ThinkingLevel::Off.openai_reasoning_effort().is_none());
        assert!(ThinkingLevel::Low.openai_reasoning_effort().is_some());
    }
}
