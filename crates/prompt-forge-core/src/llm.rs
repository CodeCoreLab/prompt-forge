//! 协议纯函数：错误分类 / 响应体截断。
//!
//! HTTP 调用本身留在主 crate（依赖 reqwest/tokio）；本模块只含可独立测试的判定逻辑。
//! reqwest::StatusCode 由主 crate 传入 u16，避免 core 依赖 reqwest。
//!
//! **端点拼接不在本模块** —— 权威实现在 [`crate::endpoint::resolve_endpoint`]。
//! 那里曾经另有一份 `openai_endpoint`，两份语义不一致（旧的会把路径中间的
//! `/chat/completions` 静默吞掉、且不归一大小写），已删除，避免「同一语义两处
//! 各写一份 → 必然漂移」。

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
///
/// 按**字符**而非字节截断：字节切片落在 UTF-8 字符中间会 panic，
/// 而 release profile 是 `panic = "abort"` —— 整个进程直接崩掉。
pub fn body_snippet_str(body: &str) -> String {
    if body.len() <= ERR_BODY_SNIPPET {
        body.to_string()
    } else {
        // 回退到不超过字节预算的最后一个字符边界，保证不切碎多字节字符
        let mut end = ERR_BODY_SNIPPET;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…(截断)", &body[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 回归：中文错误响应体在字节边界处截断不得 panic
    ///
    /// 字节切片落在 UTF-8 字符中间会 panic；release profile 是 `panic = "abort"`，
    /// 触发即整个服务进程崩溃。构造 299 字节 ASCII + 中文，字符边界在 299/302。
    #[test]
    fn snippet_multibyte_boundary_does_not_panic() {
        let body = format!("{}{}", "x".repeat(299), "错".repeat(10));
        let s = body_snippet_str(&body);
        assert!(s.ends_with("…(截断)"));
        // 截断结果必须是合法 UTF-8 且不含半个字符
        assert!(s.chars().all(|c| c != '\u{FFFD}'));
    }

    /// 全中文响应体（每字 3 字节）同样不得 panic
    #[test]
    fn snippet_all_chinese_does_not_panic() {
        let body = "错".repeat(500);
        let s = body_snippet_str(&body);
        assert!(s.ends_with("…(截断)"));
        assert!(!s.contains('\u{FFFD}'));
    }
}
