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
///
/// 文本来源覆盖多家网关的不同错误体形态：
/// - `{"error":{"message":"..."}}`（OpenAI 系）
/// - `{"error":{"type":"...","message":"..."}}`
/// - `{"message":"..."}`
/// - `{"error":"..."}`
/// - `{"errors":[{"message":"..."}]}`（Gemini / 部分网关）
/// - `{"detail":"..."}`（FastAPI 风格）
pub fn is_unsupported_param_error(status: u16, body: &Value) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let text = collect_error_text(body);
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
    //
    // 注意：这里**不能**用裸 `"reasoning"` —— `"reasoning model not available"`
    // 这类错误含有 reasoning 字样但与参数无关，误判会导致无谓降级重试。
    // 只匹配具体参数名（含下划线的字段名）。
    const PARAM_HINTS: &[&str] = &[
        "reasoning_effort",
        "reasoning.content",
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

/// 从各种错误体形态中收集可搜索的文本（转小写）
fn collect_error_text(body: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();

    fn push_str(parts: &mut Vec<String>, v: &Value) {
        if let Some(s) = v.as_str() {
            parts.push(s.to_string());
        }
    }

    // OpenAI / Anthropic 系：error.message
    if let Some(err) = body.get("error") {
        push_str(&mut parts, err);
        if let Some(m) = err.get("message") {
            push_str(&mut parts, m);
        }
        if let Some(m) = err.get("type") {
            push_str(&mut parts, m);
        }
        if let Some(m) = err.get("code") {
            push_str(&mut parts, m);
        }
    }
    // 顶层 message / detail
    push_str(&mut parts, &body["message"]);
    push_str(&mut parts, &body["detail"]);
    // Gemini / 部分网关：errors[] 数组
    if let Some(arr) = body.get("errors").and_then(|v| v.as_array()) {
        for e in arr {
            push_str(&mut parts, e);
            if let Some(m) = e.get("message") {
                push_str(&mut parts, m);
            }
        }
    }
    parts.join(" ").to_ascii_lowercase()
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
    // 必须走 budget_tokens_for 收敛：某些档位的原始预算可能超出 Anthropic 上限，
    // 直接发原始值会被服务端以 400 拒绝。
    let budget = level.budget_tokens_for("anthropic").max(1024);
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
    // 必须走 budget_tokens_for 收敛：Gemini 的 thinkingBudget 上限是 24576，
    // 而「最强」档位的原始预算是 32768 —— 不收敛就会被 Gemini 以 400 拒绝。
    let budget = level.budget_tokens_for("gemini") as i64;
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
    // 与 anthropic_thinking_params 用同一个收敛后的预算，
    // 否则两者口径不一致 → max_tokens 预算算小了，输出被截断。
    let budget = level.budget_tokens_for("anthropic").max(1024);
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

    /// 回归：Gemini 的 thinkingBudget 上限是 24576，而「最强」档位的原始预算是 32768。
    ///
    /// 不做协议收敛就会把 32768 发给 Gemini → 服务端直接 400 拒绝，
    /// 用户看到「思考强度选最高档反而完全不能用」。
    #[test]
    fn gemini_budget_is_converged_to_protocol_limit() {
        let g = gemini_thinking_params(ThinkingLevel::Max).unwrap();
        let budget = g["thinkingConfig"]["thinkingBudget"].as_i64().unwrap();
        assert!(
            budget <= 24576,
            "Gemini thinkingBudget 必须收敛到 24576 以内，实际 {budget}"
        );
        assert_eq!(budget, 24576, "应恰好收敛到协议上限");
    }

    /// Anthropic 上限 32768，等于最强档原始预算 → 不应被误收敛。
    #[test]
    fn anthropic_budget_keeps_full_value() {
        let a = anthropic_thinking_params(ThinkingLevel::Max).unwrap();
        assert_eq!(a["thinking"]["budget_tokens"], 32768);
    }

    /// anthropic_max_tokens 与 anthropic_thinking_params 必须用**同一个**收敛后预算，
    /// 否则 max_tokens 会被算小 → 输出被截断。
    #[test]
    fn anthropic_max_tokens_uses_same_converged_budget_as_params() {
        for level in [
            ThinkingLevel::Low,
            ThinkingLevel::Standard,
            ThinkingLevel::Deep,
            ThinkingLevel::Max,
        ] {
            let params = anthropic_thinking_params(level).unwrap();
            let budget = params["thinking"]["budget_tokens"].as_u64().unwrap() as u32;
            let max_tokens = anthropic_max_tokens(level, 1024);
            assert!(
                max_tokens > budget,
                "{:?} 的 max_tokens({max_tokens}) 必须大于 budget({budget})",
                level
            );
        }
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
