//! 协议纯函数：endpoint 拼接 / 错误分类 / 响应体截断。
//!
//! HTTP 调用本身留在主 crate（依赖 reqwest/tokio）；本模块只含可独立测试的判定逻辑。
//! reqwest::StatusCode 由主 crate 传入 u16，避免 core 依赖 reqwest。

/// OpenAI 兼容端点拼接：host → /v1/chat/completions；容忍 /v1、尾斜杠、完整端点。
pub fn openai_endpoint(base_url: &str) -> String {
    let mut u = base_url.trim().trim_end_matches('/').to_string();
    if u.ends_with("/chat/completions") {
        return u; // 用户填了完整端点
    }
    let base = u
        .split("/chat/completions")
        .next()
        .unwrap_or(&u)
        .to_string();
    u = base;
    if !u.ends_with("/v1") && !u.contains("/v1/") {
        u = format!("{u}/v1");
    }
    format!("{u}/chat/completions")
}

/// HTTP 层错误分类：429/5xx 可重试（RETRYABLE: 前缀），其余 4xx 不重试。
/// status 用 u16，避免 core 依赖 reqwest。
pub fn classify_http_error_u16(status: u16, msg: String) -> String {
    if status == 429 || (500..600).contains(&status) {
        format!("RETRYABLE:HTTP {status}: {msg}")
    } else {
        format!("HTTP {status}: {msg}")
    }
}

const ERR_BODY_SNIPPET: usize = 300;

/// 截断错误响应体，防止超长内容冲垮 UI
pub fn body_snippet_str(body: &str) -> String {
    if body.len() <= ERR_BODY_SNIPPET {
        body.to_string()
    } else {
        format!("{}…(截断)", &body[..ERR_BODY_SNIPPET])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_variants() {
        assert_eq!(
            openai_endpoint("https://api.deepseek.com"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            openai_endpoint("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            openai_endpoint("https://x.com/"),
            "https://x.com/v1/chat/completions"
        );
        assert_eq!(
            openai_endpoint("https://proxy.example.com/v1/chat/completions"),
            "https://proxy.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn error_classification() {
        assert!(classify_http_error_u16(502, "x".into()).starts_with("RETRYABLE:"));
        assert!(classify_http_error_u16(429, "x".into()).starts_with("RETRYABLE:"));
        assert!(!classify_http_error_u16(401, "x".into()).starts_with("RETRYABLE:"));
        assert!(!classify_http_error_u16(404, "x".into()).starts_with("RETRYABLE:"));
    }

    #[test]
    fn snippet_truncates() {
        let big = "x".repeat(1000);
        let s = body_snippet_str(&big);
        assert!(s.len() < ERR_BODY_SNIPPET + 20);
        assert!(s.ends_with("…(截断)"));
        assert_eq!(body_snippet_str("short"), "short");
    }
}
