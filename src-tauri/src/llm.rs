//! LLM 调用层：四协议适配（OpenAI 兼容 / Anthropic / Ollama / Gemini）。
//!
//! 单轮非流式调用，直接返回增强结果 —— 与快速增强定位一致。
//! 重试策略：429 与 5xx 指数退避（500ms 起，最多 3 次尝试）；
//! 4xx（除 408/429）属请求缺陷，不重试。
//! 纯函数（endpoint 拼接/错误分类/截断）在 prompt-forge-core::llm，此处只做 HTTP I/O。

use crate::config::LlmConfig;
use prompt_forge_core::llm::{body_snippet_str, openai_endpoint};
use prompt_forge_core::normalize::normalize_output;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

const CONNECT_TIMEOUT_SECS: u64 = 20;
const MAX_ATTEMPTS: u32 = 3;

/// 连接池复用：reqwest Client 内部维护连接池，避免每次重建
fn http_client(timeout_secs: u64) -> Client {
    Client::builder()
        .timeout(Duration::from_secs(if timeout_secs == 0 { 120 } else { timeout_secs }))
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .build()
        .expect("构建 HTTP 客户端失败")
}

/// 协议分发。未知协议显式报错，不静默兜底。
pub async fn enhance(cfg: &LlmConfig, prompt: &str) -> Result<String, String> {
    match cfg.protocol.as_str() {
        "openai" => call_with_retry(cfg, prompt, Protocol::OpenAI).await,
        "anthropic" => call_with_retry(cfg, prompt, Protocol::Anthropic).await,
        "ollama" => call_with_retry(cfg, prompt, Protocol::Ollama).await,
        "gemini" => call_with_retry(cfg, prompt, Protocol::Gemini).await,
        other => Err(format!(
            "config:不支持的协议: {other}（可选: openai / anthropic / ollama / gemini）"
        )),
    }
}

/// 可取消版本：token 触发即中止（含重试等待期），返回 `cancelled:已取消`。
pub async fn enhance_cancellable(
    cfg: &LlmConfig,
    prompt: &str,
    token: tokio_util::sync::CancellationToken,
) -> Result<String, String> {
    tokio::select! {
        r = enhance(cfg, prompt) => r,
        _ = token.cancelled() => Err("cancelled:已取消".into()),
    }
}

enum Protocol {
    OpenAI,
    Anthropic,
    Ollama,
    Gemini,
}

/// 重试包装：仅对标记为可重试的错误做指数退避（500ms 起，翻倍）。
async fn call_with_retry(cfg: &LlmConfig, prompt: &str, proto: Protocol) -> Result<String, String> {
    let mut last_err = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let result = match proto {
            Protocol::OpenAI => call_openai(cfg, prompt).await,
            Protocol::Anthropic => call_anthropic(cfg, prompt).await,
            Protocol::Ollama => call_ollama(cfg, prompt).await,
            Protocol::Gemini => call_gemini(cfg, prompt).await,
        };
        match result {
            Ok(text) => return Ok(text),
            Err(e) if e.starts_with("RETRYABLE:") => {
                last_err = e.trim_start_matches("RETRYABLE:").to_string();
                if attempt < MAX_ATTEMPTS {
                    let backoff = 500u64 * (1 << (attempt - 1));
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
            }
            Err(e) => return Err(e), // 不可重试，立即返回
        }
    }
    Err(last_err)
}

/// HTTP 层错误分类（转发 core 纯函数），并映射稳定错误码前缀。
/// 错误串格式：`CODE:message`（前端按 code 渲染动作，message 展示）。
/// code: unauthorized / forbidden / rate_limited / timeout / server / network / http
fn classify_http_error(status: reqwest::StatusCode, msg: String) -> String {
    let retry = prompt_forge_core::llm::classify_http_error_u16(status.as_u16(), msg.clone());
    let code = match status.as_u16() {
        401 => "unauthorized",
        403 => "forbidden",
        429 => "rate_limited",
        s if (500..600).contains(&s) => "server",
        _ => "http",
    };
    let clean = retry.trim_start_matches("RETRYABLE:");
    // 保留 RETRYABLE: 前缀语义给重试层，同时给前端稳定码
    let retry_mark = if retry.starts_with("RETRYABLE:") { "RETRYABLE:" } else { "" };
    format!("{retry_mark}{code}:{clean}")
}

/// 网络层错误（DNS/TCP/TLS/超时）可重试
fn classify_reqwest_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        format!("RETRYABLE:timeout:请求超时: {e}")
    } else if e.is_connect() || e.is_request() {
        format!("RETRYABLE:network:网络错误: {e}")
    } else {
        format!("http:请求失败: {e}")
    }
}

/// 截断错误响应体（转发 core 纯函数）
fn body_snippet(body: &Value) -> String {
    body_snippet_str(&body.to_string())
}

// ── OpenAI 兼容协议 ────────────────────────────────────────────
// 覆盖 OpenAI/DeepSeek/Moonshot/GLM/Qwen compatible-mode/Mistral/中转站等

async fn call_openai(cfg: &LlmConfig, prompt: &str) -> Result<String, String> {
    let url = openai_endpoint(&cfg.base_url);
    let payload = json!({
        "model": cfg.model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": cfg.temperature,
        "max_tokens": cfg.effective_max_tokens(prompt.len()),
        "stream": false
    });
    let mut req = http_client(cfg.effective_timeout_secs()).post(&url).json(&payload);
    if !cfg.api_key.trim().is_empty() {
        req = req.bearer_auth(cfg.api_key.trim());
    }
    let resp = req.send().await.map_err(|e| classify_reqwest_error(&e))?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| format!("server:响应解析失败: {e}"))?;
    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or(&format!("响应体: {}", body_snippet(&body)))
            .to_string();
        return Err(classify_http_error(status, msg));
    }
    extract_openai_text(&body)
}

/// 兼容 reasoning 模型（content 为空时取 reasoning_content）；
/// 提取后过 normalize 管道（剥 think 块/围栏/引号）
fn extract_openai_text(body: &Value) -> Result<String, String> {
    let text = body
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            body.pointer("/choices/0/message/reasoning_content")
                .and_then(|v| v.as_str())
        })
        .unwrap_or("");
    if text.trim().is_empty() {
        return Err(format!("响应中无文本内容: {}", body_snippet(body)));
    }
    normalize_output(text)
}

// ── Anthropic 协议 ─────────────────────────────────────────────
// POST {base}/v1/messages，x-api-key + anthropic-version 头，max_tokens 必填

async fn call_anthropic(cfg: &LlmConfig, prompt: &str) -> Result<String, String> {
    let mut base = cfg.base_url.trim().trim_end_matches('/').to_string();
    if base.ends_with("/v1") {
        base = base.trim_end_matches("/v1").to_string();
    }
    let url = format!("{base}/v1/messages");
    if cfg.api_key.trim().is_empty() {
        return Err("config:Anthropic 协议必须填写 API Key".into());
    }
    let payload = json!({
        "model": cfg.model,
        "max_tokens": cfg.effective_max_tokens(prompt.len()),
        "messages": [{"role": "user", "content": prompt}]
    });
    let req = http_client(cfg.effective_timeout_secs())
        .post(&url)
        .header("x-api-key", cfg.api_key.trim())
        .header("anthropic-version", "2023-06-01")
        .json(&payload);
    let resp = req.send().await.map_err(|e| classify_reqwest_error(&e))?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| format!("server:响应解析失败: {e}"))?;
    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or(&format!("响应体: {}", body_snippet(&body)))
            .to_string();
        return Err(classify_http_error(status, msg));
    }
    // 成功响应: content 数组 [{type:"text", text:"..."}]，取所有 text 块拼接
    let mut text = String::new();
    if let Some(blocks) = body["content"].as_array() {
        for block in blocks {
            if block["type"] == "text" {
                if let Some(t) = block["text"].as_str() {
                    text.push_str(t);
                }
            }
        }
    }
    if text.trim().is_empty() {
        return Err(format!("响应中无文本内容: {}", body_snippet(&body)));
    }
    normalize_output(&text)
}

// ── Ollama 协议 ────────────────────────────────────────────────

async fn call_ollama(cfg: &LlmConfig, prompt: &str) -> Result<String, String> {
    let base = cfg.base_url.trim().trim_end_matches('/');
    let url = format!("{base}/api/generate");
    let payload = json!({
        "model": cfg.model,
        "prompt": prompt,
        "stream": false,
        "options": {"temperature": cfg.temperature, "num_predict": cfg.effective_max_tokens(prompt.len())}
    });
    let mut req = http_client(cfg.effective_timeout_secs()).post(&url).json(&payload);
    if !cfg.api_key.trim().is_empty() {
        req = req.bearer_auth(cfg.api_key.trim());
    }
    let resp = req.send().await.map_err(|e| classify_reqwest_error(&e))?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| format!("server:响应解析失败: {e}"))?;
    if !status.is_success() {
        let msg = body["error"]
            .as_str()
            .unwrap_or(&format!("响应体: {}", body_snippet(&body)))
            .to_string();
        return Err(classify_http_error(status, msg));
    }
    body["response"]
        .as_str()
        .ok_or_else(|| format!("响应中无文本内容: {}", body_snippet(&body)))
        .and_then(normalize_output)
}

// ── Gemini 协议 ────────────────────────────────────────────────
// key 走 x-goog-api-key 头（避免拼进 URL 泄漏到日志）

async fn call_gemini(cfg: &LlmConfig, prompt: &str) -> Result<String, String> {
    if cfg.api_key.trim().is_empty() {
        return Err("config:Gemini 协议必须填写 API Key".into());
    }
    let base = cfg.base_url.trim().trim_end_matches('/');
    let url = format!("{base}/v1beta/models/{}:generateContent", cfg.model);
    let payload = json!({
        "contents": [{"parts": [{"text": prompt}]}],
        "generationConfig": {"temperature": cfg.temperature, "maxOutputTokens": cfg.effective_max_tokens(prompt.len())}
    });
    let resp = http_client(cfg.effective_timeout_secs())
        .post(&url)
        .header("x-goog-api-key", cfg.api_key.trim())
        .json(&payload)
        .send()
        .await
        .map_err(|e| classify_reqwest_error(&e))?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| format!("server:响应解析失败: {e}"))?;
    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or(&format!("响应体: {}", body_snippet(&body)))
            .to_string();
        return Err(classify_http_error(status, msg));
    }
    body.pointer("/candidates/0/content/parts/0/text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("响应中无文本内容: {}", body_snippet(&body)))
        .and_then(normalize_output)
}
