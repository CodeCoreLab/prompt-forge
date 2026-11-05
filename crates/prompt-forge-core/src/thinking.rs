//! 思考（推理）参数的协议适配与降级判定。
//!
//! 背景：各家协议表达"多想一会儿"的参数完全不同，且**大量中转站/自建网关并不支持**
//! 这些参数。若直接硬传，遇到不支持的网关会收到 400/422，整个增强流程直接失败——
//! 这违背了"功能等价、行为不变"的原则（旧版根本没有这些参数，必然能跑通）。
//!
//! 策略：
//! 1. 按协议生成对应的推理参数（OpenAI / Anthropic / Gemini 各自不同）
//! 2. 首次请求携带该参数
//! 3. 若响应明确表示"不支持该参数"，**降级为不携带参数重试一次**（只降级一次，避免死循环）
//!
//! 判定"参数不被支持"依据错误体中的特征词（unsupported / unknown parameter /
//! not supported / invalid parameter 等），纯函数实现，可独立测试。

use serde_json::{json, Value};

use crate::methods::ThinkingLevel;

/// 判定错误响应是否属于"参数不被支持"（可安全降级重试）。
///
/// 只在 400 / 422 状态下判定——其他状态（401/403/429/5xx）与参数无关，重试无意义。
pub fn is_unsupported_param_error(status: u16, body: &Value) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let text = body
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("message").and_then(|v| v.as_str()))
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_ascii_lowercase();

    if text.is_empty() {
        return false;
    }
    // 明确的"参数不被支持"信号
    const SIGNALS: &[&str] = &[
        "unsupported",
        "not supported",
        "unknown parameter",
        "unknown field",
        "unrecognized",
        "invalid parameter",
        "unexpected parameter",
        "extra inputs are not permitted",
        "not permitted",
    ];
    // 参数名线索（提高判定精度，避免把"模型不存在"误判为参数问题）
    const PARAM_HINTS: &[&str] = &[
        "reasoning_effort",
        "reasoning",
        "thinking",
        "budget_tokens",
        "enable_thinking",
        "thinkingconfig",
        "thinking_budget",
    ];

    let has_signal = SIGNALS.iter().any(|s| text.contains(s));
    let has_param = PARAM_HINTS.iter().any(|s| text.contains(s));
    has_signal && has_param
}

/// OpenAI 兼容协议：`reasoning_effort` 字段（部分模型支持）。
/// 同时兼容 Qwen/GLM 风格的 `enable_thinking` 布尔开关。
pub fn openai_thinking_params(level: ThinkingLevel) -> Option<Value> {
    if !level.is_on() {
        return None;
    }
    let mut map = serde_json::Map::new();
    if let Some(effort) = level.openai_reasoning_effort() {
        map.insert("reasoning_effort".into(), json!(effort));
    }
    map.insert("enable_thinking".into(), json!(true));
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

/// Anthropic 协议：`thinking: {type:"enabled", budget_tokens:N}`。
/// 注意 Anthropic 要求 max_tokens > budget_tokens，调用方需保证。
pub fn anthropic_thinking_params(level: ThinkingLevel) -> Option<Value> {
    if !level.is_on() {
        return None;
    }
    let budget = level.budget_tokens().max(1024);
    Some(json!({
        "thinking": {
            "type": "enabled",
            "budget_tokens": budget
        }
    }))
}

/// Gemini 协议：`generationConfig.thinkingConfig.thinkingBudget`（-1 = 动态思考）。
pub fn gemini_thinking_params(level: ThinkingLevel) -> Option<Value> {
    if !level.is_on() {
        return None;
    }
    let budget = level.budget_tokens() as i64;
    Some(json!({
        "thinkingConfig": {
            "thinkingBudget": budget,
            "includeThoughts": false
        }
    }))
}

/// Ollama 协议：新一代 Ollama 支持 `think: true/false` 开关。
pub fn ollama_thinking_params(level: ThinkingLevel) -> Option<Value> {
    if !level.is_on() {
        return None;
    }
    Some(json!({ "think": true }))
}

/// Anthropic 下思考开启时，max_tokens 必须大于 thinking 预算。
/// 返回修正后的 max_tokens。
pub fn anthropic_max_tokens(level: ThinkingLevel, requested: u32) -> u32 {
    if !level.is_on() {
        return requested;
    }
    let budget = level.budget_tokens().max(1024);
    // Anthropic 要求 max_tokens > budget_tokens，留 1 倍余量给正式输出
    requested.max(budget.saturating_mul(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_produces_no_params() {
        assert!(openai_thinking_params(ThinkingLevel::Off).is_none());
        assert!(anthropic_thinking_params(ThinkingLevel::Off).is_none());
        assert!(gemini_thinking_params(ThinkingLevel::Off).is_none());
        assert!(ollama_thinking_params(ThinkingLevel::Off).is_none());
    }

    #[test]
    fn on_produces_params() {
        let o = openai_thinking_params(ThinkingLevel::Deep).unwrap();
        assert_eq!(o["reasoning_effort"], "high");
        let a = anthropic_thinking_params(ThinkingLevel::Deep).unwrap();
        assert_eq!(a["thinking"]["type"], "enabled");
        assert_eq!(a["thinking"]["budget_tokens"], 16384);
        let g = gemini_thinking_params(ThinkingLevel::Low).unwrap();
        assert_eq!(g["thinkingConfig"]["thinkingBudget"], 1024);
        let ol = ollama_thinking_params(ThinkingLevel::Low).unwrap();
        assert_eq!(ol["think"], true);
    }

    #[test]
    fn anthropic_max_tokens_exceeds_budget() {
        // 预算 32768 → max_tokens 至少 65536
        assert_eq!(anthropic_max_tokens(ThinkingLevel::Max, 1024), 65536);
        // 关闭时原样返回
        assert_eq!(anthropic_max_tokens(ThinkingLevel::Off, 1024), 1024);
        // 关闭时请求值已够大也原样返回
        assert_eq!(anthropic_max_tokens(ThinkingLevel::Off, 99999), 99999);
    }

    #[test]
    fn detects_unsupported_param_error() {
        let body = json!({"error": {"message": "Unsupported parameter: 'reasoning_effort' is not supported"}});
        assert!(is_unsupported_param_error(400, &body));
        let body2 = json!({"error": {"message": "unknown parameter enable_thinking"}});
        assert!(is_unsupported_param_error(422, &body2));
    }

    #[test]
    fn ignores_non_param_errors() {
        // 401 与参数无关
        let body = json!({"error": {"message": "Invalid API key"}});
        assert!(!is_unsupported_param_error(401, &body));
        // 200/500 不判定
        assert!(!is_unsupported_param_error(500, &body));
        // 无参数线索的 400 不误判
        let body3 = json!({"error": {"message": "model not found"}});
        assert!(!is_unsupported_param_error(400, &body3));
        // 空体不判定
        assert!(!is_unsupported_param_error(400, &json!({})));
    }
}
