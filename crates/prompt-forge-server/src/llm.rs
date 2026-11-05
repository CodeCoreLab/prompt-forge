//! LLM 调用层：四协议适配（OpenAI 兼容 / Anthropic / Ollama / Gemini）。
//!
//! 与历史版本（Tauri/CLI）行为完全一致：
//! - 单轮非流式调用，直接返回增强结果
//! - 重试策略：429 与 5xx 指数退避（500ms 起，最多 3 次尝试）；其余 4xx 不重试
//! - 错误串格式 `CODE:message`（前端按 code 渲染动作）
//! - 纯函数（endpoint 拼接/错误分类/截断）复用 prompt-forge-core::llm
//!
//! 本层是本 crate 唯一依赖 reqwest 的模块，与 HTTP 服务层解耦。
//!
//! 新增能力（2026-11-05）：
//! - **角色注入**：system_prompt 作为独立的 system 消息前置，不混入用户提示词
//! - **深度思考强度**：按协议注入推理参数（reasoning_effort / thinking.budget_tokens
//!   / thinkingConfig / think），并在网关不支持该参数时**自动降级重试一次**，
//!   保证旧版（无思考参数）能跑通的行为不被破坏。

use std::time::Duration;

use prompt_forge_core::config::LlmConfig;
use prompt_forge_core::llm::{body_snippet_str, openai_endpoint};
use prompt_forge_core::methods::ThinkingLevel;
use prompt_forge_core::normalize::normalize_output;
use prompt_forge_core::thinking;
use reqwest::Client;
use serde_json::{json, Value};

const CONNECT_TIMEOUT_SECS: u64 = 20;
const MAX_ATTEMPTS: u32 = 3;

/// 一次出站调用的完整参数（角色 + 思考强度 + 用户提示词）
pub struct EnhanceRequest<'a> {
    /// 角色设定，作为 system 消息前置；空串表示不注入
    pub system_prompt: &'a str,
    /// 深度思考强度
    pub thinking: ThinkingLevel,
    /// 用户提示词正文
    pub prompt: &'a str,
}

impl<'a> EnhanceRequest<'a> {
    pub fn new(prompt: &'a str) -> Self {
        Self {
            system_prompt: "",
            thinking: ThinkingLevel::Off,
            prompt,
        }
    }

    pub fn with_system(mut self, system: &'a str) -> Self {
        self.system_prompt = system;
        self
    }

    pub fn with_thinking(mut self, level: ThinkingLevel) -> Self {
        self.thinking = level;
        self
    }
}

/// 连接池复用：Client 内部维护连接池，避免每次重建
fn http_client(timeout_secs: u64) -> Client {
    Client::builder()
        .timeout(Duration::from_secs(if timeout_secs == 0 {
            120
        } else {
            timeout_secs
        }))
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .build()
        .expect("构建 HTTP 客户端失败")
}

/// 协议分发。未知协议显式报错，不静默兜底。
pub async fn enhance(cfg: &LlmConfig, req: &EnhanceRequest<'_>) -> Result<String, String> {
    match cfg.protocol.as_str() {
        "openai" => call_with_retry(cfg, req, Protocol::OpenAI).await,
        "anthropic" => call_with_retry(cfg, req, Protocol::Anthropic).await,
        "ollama" => call_with_retry(cfg, req, Protocol::Ollama).await,
        "gemini" => call_with_retry(cfg, req, Protocol::Gemini).await,
        other => Err(format!(
            "config:不支持的协议: {other}（可选: openai / anthropic / ollama / gemini）"
        )),
    }
}

enum Protocol {
    OpenAI,
    Anthropic,
    Ollama,
    Gemini,
}

/// 重试包装：仅对标记为可重试的错误做指数退避（500ms 起，翻倍）。
async fn call_with_retry(
    cfg: &LlmConfig,
    req: &EnhanceRequest<'_>,
    proto: Protocol,
) -> Result<String, String> {
    let mut last_err = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let result = match proto {
            Protocol::OpenAI => call_openai(cfg, req).await,
            Protocol::Anthropic => call_anthropic(cfg, req).await,
            Protocol::Ollama => call_ollama(cfg, req).await,
            Protocol::Gemini => call_gemini(cfg, req).await,
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

/// 出站请求统一封装：发送 → 若判定为"参数不被支持"则降级重试一次。
///
/// `build` 接收 `with_thinking` 标志，据此决定是否携带推理参数。
/// 降级只做一次（避免无限重试），降级后仍失败则返回原始错误。
async fn send_with_degrade<F>(
    cfg: &LlmConfig,
    build: F,
) -> Result<(reqwest::StatusCode, Value), String>
where
    F: Fn(bool) -> (String, Value, Vec<(String, String)>),
{
    // 第一次：携带思考参数
    let (url, payload, headers) = build(true);
    let client = http_client(cfg.effective_timeout_secs());
    let mut req = client.post(&url).json(&payload);
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = req.send().await.map_err(|e| classify_reqwest_error(&e))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("server:响应解析失败: {e}"))?;

    // 参数不被支持 → 去掉思考参数重试一次
    if thinking::is_unsupported_param_error(status.as_u16(), &body) {
        let (url2, payload2, headers2) = build(false);
        let mut req2 = client.post(&url2).json(&payload2);
        for (k, v) in &headers2 {
            req2 = req2.header(k.as_str(), v.as_str());
        }
        let resp2 = req2.send().await.map_err(|e| classify_reqwest_error(&e))?;
        let status2 = resp2.status();
        let body2: Value = resp2
            .json()
            .await
            .map_err(|e| format!("server:响应解析失败: {e}"))?;
        return Ok((status2, body2));
    }

    Ok((status, body))
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
    let retry_mark = if retry.starts_with("RETRYABLE:") {
        "RETRYABLE:"
    } else {
        ""
    };
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

async fn call_openai(cfg: &LlmConfig, req: &EnhanceRequest<'_>) -> Result<String, String> {
    let url = openai_endpoint(&cfg.base_url);
    let system = req.system_prompt.trim();
    let body = send_with_degrade(cfg, |with_thinking| {
        // messages：system（角色）前置 → user（提示词）
        let mut messages: Vec<Value> = Vec::new();
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.push(json!({"role": "user", "content": req.prompt}));

        let mut payload = json!({
            "model": cfg.model,
            "messages": messages,
            "temperature": cfg.temperature,
            "max_tokens": cfg.effective_max_tokens(req.prompt.len()),
            "stream": false
        });
        if with_thinking {
            if let Some(t) = thinking::openai_thinking_params(req.thinking) {
                if let Some(obj) = payload.as_object_mut() {
                    for (k, v) in t.as_object().unwrap() {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        let mut headers: Vec<(String, String)> = Vec::new();
        if !cfg.api_key.trim().is_empty() {
            headers.push(("Authorization".into(), format!("Bearer {}", cfg.api_key.trim())));
        }
        (url.clone(), payload, headers)
    })
    .await?;

    let (status, body) = body;
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

async fn call_anthropic(cfg: &LlmConfig, req: &EnhanceRequest<'_>) -> Result<String, String> {
    let mut base = cfg.base_url.trim().trim_end_matches('/').to_string();
    if base.ends_with("/v1") {
        base = base.trim_end_matches("/v1").to_string();
    }
    let url = format!("{base}/v1/messages");
    if cfg.api_key.trim().is_empty() {
        return Err("config:Anthropic 协议必须填写 API Key".into());
    }
    let system = req.system_prompt.trim();
    let body = send_with_degrade(cfg, |with_thinking| {
        let mut payload = json!({
            "model": cfg.model,
            "max_tokens": thinking::anthropic_max_tokens(
                req.thinking,
                cfg.effective_max_tokens(req.prompt.len())
            ),
            "messages": [{"role": "user", "content": req.prompt}]
        });
        if !system.is_empty() {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("system".into(), json!(system));
            }
        }
        if with_thinking {
            if let Some(t) = thinking::anthropic_thinking_params(req.thinking) {
                if let (Some(obj), Some(t)) = (payload.as_object_mut(), t.as_object()) {
                    for (k, v) in t {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        let headers = vec![
            ("x-api-key".to_string(), cfg.api_key.trim().to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ];
        (url.clone(), payload, headers)
    })
    .await?;

    let (status, body) = body;
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

async fn call_ollama(cfg: &LlmConfig, req: &EnhanceRequest<'_>) -> Result<String, String> {
    let base = cfg.base_url.trim().trim_end_matches('/');
    let url = format!("{base}/api/generate");
    let system = req.system_prompt.trim();
    let body = send_with_degrade(cfg, |with_thinking| {
        let mut payload = json!({
            "model": cfg.model,
            "prompt": req.prompt,
            "stream": false,
            "options": {"temperature": cfg.temperature, "num_predict": cfg.effective_max_tokens(req.prompt.len())}
        });
        if !system.is_empty() {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("system".into(), json!(system));
            }
        }
        if with_thinking {
            if let Some(t) = thinking::ollama_thinking_params(req.thinking) {
                if let (Some(obj), Some(t)) = (payload.as_object_mut(), t.as_object()) {
                    for (k, v) in t {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        let mut headers: Vec<(String, String)> = Vec::new();
        if !cfg.api_key.trim().is_empty() {
            headers.push(("Authorization".into(), format!("Bearer {}", cfg.api_key.trim())));
        }
        (url.clone(), payload, headers)
    })
    .await?;

    let (status, body) = body;
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

async fn call_gemini(cfg: &LlmConfig, req: &EnhanceRequest<'_>) -> Result<String, String> {
    if cfg.api_key.trim().is_empty() {
        return Err("config:Gemini 协议必须填写 API Key".into());
    }
    let base = cfg.base_url.trim().trim_end_matches('/');
    let url = format!("{base}/v1beta/models/{}:generateContent", cfg.model);
    let system = req.system_prompt.trim();
    let body = send_with_degrade(cfg, |with_thinking| {
        let mut gen_config = json!({
            "temperature": cfg.temperature,
            "maxOutputTokens": cfg.effective_max_tokens(req.prompt.len())
        });
        if with_thinking {
            if let Some(t) = thinking::gemini_thinking_params(req.thinking) {
                if let (Some(g), Some(t)) = (gen_config.as_object_mut(), t.as_object()) {
                    for (k, v) in t {
                        g.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        let mut payload = json!({
            "contents": [{"parts": [{"text": req.prompt}]}],
            "generationConfig": gen_config
        });
        if !system.is_empty() {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "systemInstruction".into(),
                    json!({"parts": [{"text": system}]}),
                );
            }
        }

        let headers = vec![(
            "x-goog-api-key".to_string(),
            cfg.api_key.trim().to_string(),
        )];
        (url.clone(), payload, headers)
    })
    .await?;

    let (status, body) = body;
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
