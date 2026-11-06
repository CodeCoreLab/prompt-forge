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

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use prompt_forge_core::config::LlmConfig;
use prompt_forge_core::llm::body_snippet_str;
use prompt_forge_core::methods::ThinkingLevel;
use prompt_forge_core::normalize::normalize_output;
use prompt_forge_core::thinking;
use reqwest::Client;
use serde_json::{json, Value};

const CONNECT_TIMEOUT_SECS: u64 = 20;
const MAX_ATTEMPTS: u32 = 3;

/// 出站响应体体积上限（8MB）。超过即中止读取并报错，防止被劫持的
/// 中转站返回超大响应体把本地服务进程内存打爆。
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// 出站输入（提示词）字符数上限。超限直接拒绝，避免构造超大请求体。
const MAX_INPUT_CHARS: usize = 20_000;

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

/// 进程级共享的 HTTP 客户端（连接池复用）。
///
/// `reqwest::Client` 内部维护连接池，`Send + Sync`，用 `OnceLock` 做进程级单例，
/// 跨请求复用同一实例，避免每次调用重建连接池（单次请求最坏情况原本会新建 6 个 Client）。
///
/// 客户端**不设**全局 timeout（全局 timeout 会覆盖逐请求设置），
/// 由调用方通过 `RequestBuilder::timeout` 逐请求设置。
static HTTP_CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();

/// 取共享客户端。构建失败走既有 `CODE:message` 通道返回错误，
/// 不使用 `expect`（release 下 `panic = "abort"` 会终止整个本地服务进程）。
fn http_client() -> Result<&'static Client, String> {
    match HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("config:构建 HTTP 客户端失败: {e}"))
    }) {
        Ok(client) => Ok(client),
        Err(msg) => Err(msg.clone()),
    }
}

/// 逐请求超时时长（覆盖连接 + 读取全过程）。
fn request_timeout(cfg: &LlmConfig) -> Duration {
    Duration::from_secs(cfg.effective_timeout_secs())
}

/// 单次增强请求允许的**总**出站预算。
///
/// 一次 attempt 最多发 **2** 个出站请求（首发 + 降级重试，见 `send_with_degrade`），
/// 而 attempt 最多 `MAX_ATTEMPTS` 次，故最坏情况是 `MAX_ATTEMPTS * 2` 个请求。
/// 外层（api.rs）的整体墙钟上限按这个倍率计算，否则降级重试会被提前掐断。
///
/// 这里以「单请求超时」为单位给出总预算倍数，供 llm 层自身与调用方共同引用，
/// 避免两处各写一个魔数而对不上。
pub const MAX_OUTBOUND_REQUESTS: u32 = MAX_ATTEMPTS * 2;

/// 总出站预算（秒）= 单请求超时 × 最坏请求数 + 退避余量。
///
/// 退避余量覆盖 3 次 attempt 之间的指数退避（500ms + 1000ms = 1.5s），留 30s 宽裕。
pub fn total_outbound_budget_secs(cfg: &LlmConfig) -> u64 {
    cfg.effective_timeout_secs()
        .saturating_mul(MAX_OUTBOUND_REQUESTS as u64)
        .saturating_add(30)
}

/// 出站总预算的截止时刻。
///
/// 一次 attempt 最多发 2 个出站请求（首发 + 降级重试），attempt 最多 `MAX_ATTEMPTS` 次。
/// 若不给整条调用链设一个共享截止时刻，降级重试会让实际出站次数翻倍、
/// 耗时突破外层按「1 attempt = 1 请求」算出的墙钟上限，导致降级重试被提前掐断。
///
/// 因此整条链共享同一个 deadline：每个请求的 timeout 取「单请求上限」与
/// 「剩余预算」的较小值，预算耗尽即不再发请求。
fn outbound_deadline(cfg: &LlmConfig) -> Instant {
    Instant::now() + Duration::from_secs(total_outbound_budget_secs(cfg))
}

/// 剩余预算（已耗尽则返回 0）。
fn remaining_budget(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// 出站输入（提示词）长度校验：超限直接拒绝，避免构造超大请求体。
fn check_input_len(req: &EnhanceRequest<'_>) -> Result<(), String> {
    let chars = req.prompt.chars().count();
    if chars > MAX_INPUT_CHARS {
        return Err(format!(
            "config:输入内容过长（超过 {MAX_INPUT_CHARS} 字符）"
        ));
    }
    Ok(())
}

/// 响应体累计体积是否超限（纯函数，便于单测）。
fn check_body_limit(current: usize, incoming: usize) -> Result<(), String> {
    if current.saturating_add(incoming) > MAX_RESPONSE_BYTES {
        return Err(format!(
            "server:响应体过大（超过 {}MB 上限）",
            MAX_RESPONSE_BYTES / 1024 / 1024
        ));
    }
    Ok(())
}

/// 响应体分块累加器：累计字节数并做体积上限检查。
///
/// 抽出为独立结构，便于在无网络环境下单测体积上限行为。
struct BodyAccumulator {
    buf: Vec<u8>,
}

impl BodyAccumulator {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// 追加一块数据；超过上限返回 `server:...` 错误。
    fn push(&mut self, chunk: &[u8]) -> Result<(), String> {
        check_body_limit(self.buf.len(), chunk.len())?;
        self.buf.extend_from_slice(chunk);
        Ok(())
    }

    fn into_json(self) -> Result<Value, String> {
        serde_json::from_slice(&self.buf).map_err(|e| format!("server:响应解析失败: {e}"))
    }
}

/// 流式读取响应体并解析为 JSON，累计超过上限即中止。
///
/// 用 `Response::chunk()` 逐块读取（reqwest 0.12，无需新增依赖），
/// 避免 `resp.json()` 把整个响应体一次性读入内存。
async fn read_body_capped(resp: reqwest::Response) -> Result<Value, String> {
    let mut resp = resp;
    let mut acc = BodyAccumulator::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("server:响应读取失败: {e}"))?
    {
        acc.push(&chunk)?;
    }
    acc.into_json()
}

/// URL path segment percent-encoding（保留 `A-Z a-z 0-9 - . _ ~`，其余全部 `%XX`）。
///
/// 手写实现，避免为一次调用引入新依赖。
fn percent_encode_path_segment(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// 归一化 Gemini model 名：去空白 + 去掉可能存在的 `models/` 前缀
/// （避免拼出 `/v1beta/models/models/xxx`）。
fn normalize_gemini_model(model: &str) -> &str {
    let trimmed = model.trim();
    trimmed.strip_prefix("models/").unwrap_or(trimmed)
}

/// 协议分发。未知协议显式报错，不静默兜底。
pub async fn enhance(cfg: &LlmConfig, req: &EnhanceRequest<'_>) -> Result<String, String> {
    // 整条调用链共享一个出站预算截止时刻（含降级重试与重试退避）
    let deadline = outbound_deadline(cfg);
    match cfg.protocol.as_str() {
        "openai" => call_with_retry(cfg, req, Protocol::OpenAI, deadline).await,
        "anthropic" => call_with_retry(cfg, req, Protocol::Anthropic, deadline).await,
        "ollama" => call_with_retry(cfg, req, Protocol::Ollama, deadline).await,
        "gemini" => call_with_retry(cfg, req, Protocol::Gemini, deadline).await,
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
///
/// `deadline` 是整条调用链共享的出站预算截止时刻（见 `outbound_deadline`），
/// 由 `enhance` 一次性生成并透传下去。
async fn call_with_retry(
    cfg: &LlmConfig,
    req: &EnhanceRequest<'_>,
    proto: Protocol,
    deadline: Instant,
) -> Result<String, String> {
    let mut last_err = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let result = match proto {
            Protocol::OpenAI => call_openai(cfg, req, deadline).await,
            Protocol::Anthropic => call_anthropic(cfg, req, deadline).await,
            Protocol::Ollama => call_ollama(cfg, req, deadline).await,
            Protocol::Gemini => call_gemini(cfg, req, deadline).await,
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
///
/// 两个请求共享 `deadline` 总预算：降级重试的 timeout 取「剩余预算」，
/// 不会因为多发一次请求而突破总预算（否则降级重试会被外层墙钟提前掐断）。
async fn send_with_degrade<F>(
    cfg: &LlmConfig,
    deadline: Instant,
    build: F,
) -> Result<(reqwest::StatusCode, Value), String>
where
    F: Fn(bool) -> (String, Value, Vec<(String, String)>),
{
    let client = http_client()?;
    let per_request = request_timeout(cfg);

    // 第一次：携带思考参数
    let (url, payload, headers) = build(true);
    let mut req = client
        .post(&url)
        .json(&payload)
        .timeout(per_request.min(remaining_budget(deadline)));
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = req.send().await.map_err(|e| classify_reqwest_error(&e))?;
    let status = resp.status();
    let body = read_body_capped(resp).await?;

    // 参数不被支持 → 去掉思考参数重试一次
    if thinking::is_unsupported_param_error(status.as_u16(), &body) {
        // 预算已耗尽则不再发降级请求，直接返回首次结果（避免无谓的等待）
        let left = remaining_budget(deadline);
        if left.is_zero() {
            return Ok((status, body));
        }
        let (url2, payload2, headers2) = build(false);
        let mut req2 = client
            .post(&url2)
            .json(&payload2)
            .timeout(per_request.min(left));
        for (k, v) in &headers2 {
            req2 = req2.header(k.as_str(), v.as_str());
        }
        let resp2 = req2.send().await.map_err(|e| classify_reqwest_error(&e))?;
        let status2 = resp2.status();
        let body2 = read_body_capped(resp2).await?;
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

/// 出站前校验 base_url（最后一道安全闸门）。
///
/// `base_url` 会带着用户的 API Key（`Authorization: Bearer <key>`）一起发出去，
/// 因此必须在**每次出站前**校验：
/// - 拒绝非 http/https 协议（`file:` / `ftp:`）
/// - 拒绝携带 userinfo 的 URL（`https://user:pass@host` —— 凭据会被写进 base_url）
/// - 拒绝非法主机名与路径字符
///
/// 前端的 `/api/parse-url` 也做白名单，但**前端校验不是安全边界**——
/// 恶意页面可绕过 UI 直接 POST `/api/config` 写入任意 base_url。
fn checked_base_url(cfg: &LlmConfig) -> Result<String, String> {
    prompt_forge_core::endpoint::validate_base_url(&cfg.base_url).map_err(|e| {
        let mut msg = format!("config:接口地址不安全或非法: {e}");
        // 点明「实际会连接的主机」：`https://api.openai.com@evil.com/v1` 这类伪装地址，
        // 用户看到「不得包含用户名/密码」未必能联想到真正连的是 evil.com。
        if let Some(host) = actual_host_hint(&cfg.base_url) {
            msg.push_str(&format!("，实际会连接的主机是 {host}"));
        }
        msg
    })
}

/// 拼接最终的出站 URL（**幂等** —— 重复调用不会重复叠后缀）。
///
/// ## 为什么必须在这里收口
///
/// 用户粘贴的往往是**完整接口 URL**（`http://127.0.0.1:11434/api/generate`），
/// 前端反推出的 base_url 可能仍带着接口后缀。若出站时再无条件拼一次后缀：
/// - ollama → `/api/generate/api/generate` → **404**
/// - anthropic → `/v1/messages/v1/messages` → **404**
/// - gemini → `/v1beta/models/x/v1beta/models/x:generateContent` → **404**
/// - openai → `/chat/completions/chat/completions` → **404**
///
/// 这是「主推输入方式（粘贴完整 URL）」的必然失败路径，且 ollama（本地部署最常见）
/// 100% 触发 —— 不是边界情况，是主路径。
///
/// ## 语义只存在于 core 一处
///
/// 「base_url → 最终出站 URL」的**完整语义**由 core 的
/// [`resolve_endpoint`](prompt_forge_core::endpoint::resolve_endpoint) 独家承担 ——
/// 它也是前端预览与 server 出站共用的**唯一出站入口**：
/// - 剥掉**已知的**接口后缀（循环剥到不再命中，因此后缀叠了两遍也会收敛到单层）；
/// - 补版本段（`/v1`、`/v1beta`）、把 `/api/chat` 换成 `/api/generate`；
/// - 换掉 gemini 的 model 段、保留网关子路径；
/// - 归一接口路径的大小写。
///
/// server 侧只保留两件 server 自己的事：
/// 1. [`checked_base_url`] 安全闸门 —— **必须先于 `resolve_endpoint` 执行**，
///    它拒绝非 http(s) / 带 userinfo / 路径含非法字符（含 `?` `#`）的 base_url；
/// 2. model 的 percent-encode（model 要进 path segment）与协议白名单 ——
///    未知协议显式报错，不静默兜底。
///
/// server 侧**零出站语义、纯转发**：一旦 server 与 core 各写一份后缀表与
/// 「路径里哪一段是版本段」的规则，就会重现 `/v1beta/v1beta` 双版本段那类缺陷
/// （剥在 core、拼在 server，两边对「gemini 的 base 语义」理解不一致）。
fn outbound_url(cfg: &LlmConfig) -> Result<String, String> {
    // 安全闸门先行：非法 base_url 必须在任何拼接之前被拒
    let raw = checked_base_url(cfg)?;
    let protocol = cfg.protocol.as_str();
    if !matches!(protocol, "openai" | "anthropic" | "ollama" | "gemini") {
        return Err(format!(
            "config:不支持的协议: {protocol}（可选: openai / anthropic / ollama / gemini）"
        ));
    }
    // model 走 path segment，必须先 percent-encode
    // （否则含 `/` `?` `#` 空格会破坏 URL 结构）
    let model = percent_encode_path_segment(normalize_gemini_model(&cfg.model));
    Ok(prompt_forge_core::endpoint::resolve_endpoint(
        &raw, protocol, &model,
    ))
}

/// 校验失败时提取「实际会连接的主机」，用于错误提示。
///
/// 只在主机符合安全字符集（字母数字与 `-` `.` `_` `:`）时才回显 —— 错误串会
/// 直接展示给用户，不做字符白名单就等于把不可信内容原样带到渲染层。
///
/// `pub(crate)` —— 写入侧（`api.rs` 的 `check_write_side_base_urls`）也复用它：
/// 写入侧是用户填错地址的**第一道反馈**，同样需要点明「真正会连的是哪个主机」。
pub(crate) fn actual_host_hint(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let rest = match trimmed.split_once("://") {
        Some((_, r)) => r,
        None => return None, // 连协议都没有，谈不上「会连到哪个主机」
    };
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("");
    // userinfo 在最后一个 `@` 之前 —— 它之后才是真正的主机
    let hostport = hostport.rsplit_once('@').map(|(_, h)| h).unwrap_or(hostport);
    let host = if hostport.starts_with('[') {
        match hostport.split_once(']') {
            Some((h, _)) => h.trim_matches(|c| c == '[' || c == ']'),
            None => return None,
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => h,
            _ => hostport,
        }
    };
    if host.is_empty() {
        return None;
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':'))
    {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

// ── OpenAI 兼容协议 ────────────────────────────────────────────
// 覆盖 OpenAI/DeepSeek/Moonshot/GLM/Qwen compatible-mode/Mistral/中转站等

async fn call_openai(
    cfg: &LlmConfig,
    req: &EnhanceRequest<'_>,
    deadline: Instant,
) -> Result<String, String> {
    check_input_len(req)?;
    // 出站 URL 统一走 outbound_url（幂等：base 已含后缀也不会重复拼接）
    let url = outbound_url(cfg)?;
    let system = req.system_prompt.trim();
    let body = send_with_degrade(cfg, deadline, |with_thinking| {
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
                // 不用 unwrap：release 下 panic = "abort" 会终止整个服务进程
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

async fn call_anthropic(
    cfg: &LlmConfig,
    req: &EnhanceRequest<'_>,
    deadline: Instant,
) -> Result<String, String> {
    check_input_len(req)?;
    // 出站 URL 统一走 outbound_url（幂等：base 已含后缀也不会重复拼接）
    let url = outbound_url(cfg)?;
    if cfg.api_key.trim().is_empty() {
        return Err("config:Anthropic 协议必须填写 API Key".into());
    }
    let system = req.system_prompt.trim();
    let body = send_with_degrade(cfg, deadline, |with_thinking| {
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

async fn call_ollama(
    cfg: &LlmConfig,
    req: &EnhanceRequest<'_>,
    deadline: Instant,
) -> Result<String, String> {
    check_input_len(req)?;
    // 出站 URL 统一走 outbound_url（幂等：base 已含后缀也不会重复拼接）
    let url = outbound_url(cfg)?;
    let system = req.system_prompt.trim();
    let body = send_with_degrade(cfg, deadline, |with_thinking| {
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

async fn call_gemini(
    cfg: &LlmConfig,
    req: &EnhanceRequest<'_>,
    deadline: Instant,
) -> Result<String, String> {
    check_input_len(req)?;
    if cfg.api_key.trim().is_empty() {
        return Err("config:Gemini 协议必须填写 API Key".into());
    }
    // 出站 URL 统一走 outbound_url（幂等：base 已含后缀或 model 段也不会重复拼接）
    // model 走 path segment，outbound_url 内部已做 percent-encode
    // （否则含 `/` `?` `#` 空格会破坏 URL 语义）
    let url = outbound_url(cfg)?;
    let system = req.system_prompt.trim();
    let body = send_with_degrade(cfg, deadline, |with_thinking| {
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── 响应体体积上限 ──────────────────────────────────────────

    #[test]
    fn check_body_limit_accepts_within_limit() {
        assert!(check_body_limit(0, 1024).is_ok());
        assert!(check_body_limit(MAX_RESPONSE_BYTES - 1, 1).is_ok());
    }

    #[test]
    fn check_body_limit_rejects_oversize() {
        let err = check_body_limit(MAX_RESPONSE_BYTES, 1).unwrap_err();
        assert!(err.starts_with("server:"), "错误码前缀应为 server: {err}");
        assert!(err.contains("8MB"), "错误信息应说明上限: {err}");
    }

    #[test]
    fn body_accumulator_rejects_oversize_chunk() {
        let mut acc = BodyAccumulator::new();
        assert!(acc.push(&[b'a'; 16]).is_ok());
        let err = acc.push(&vec![b'b'; MAX_RESPONSE_BYTES]).unwrap_err();
        assert!(err.starts_with("server:"), "{err}");
    }

    #[test]
    fn body_accumulator_parses_json() {
        let mut acc = BodyAccumulator::new();
        acc.push(br#"{"a":1,"b":"x"}"#).unwrap();
        let v = acc.into_json().unwrap();
        assert_eq!(v["a"], json!(1));
        assert_eq!(v["b"], json!("x"));
    }

    #[test]
    fn body_accumulator_invalid_json_is_server_error() {
        let mut acc = BodyAccumulator::new();
        acc.push(b"not json").unwrap();
        let err = acc.into_json().unwrap_err();
        assert!(err.starts_with("server:"), "{err}");
    }

    // ── Gemini model percent-encoding ───────────────────────────

    #[test]
    fn percent_encode_keeps_unreserved_chars() {
        assert_eq!(
            percent_encode_path_segment("gemini-2.0-pro_v1~"),
            "gemini-2.0-pro_v1~"
        );
    }

    #[test]
    fn percent_encode_escapes_specials() {
        assert_eq!(percent_encode_path_segment("a/b"), "a%2Fb");
        assert_eq!(percent_encode_path_segment("a?b"), "a%3Fb");
        assert_eq!(percent_encode_path_segment("a#b"), "a%23b");
        assert_eq!(percent_encode_path_segment("a b"), "a%20b");
        assert_eq!(percent_encode_path_segment("中文"), "%E4%B8%AD%E6%96%87");
        assert_eq!(percent_encode_path_segment("a:b"), "a%3Ab");
        assert_eq!(percent_encode_path_segment("%"), "%25");
    }

    #[test]
    fn normalize_gemini_model_strips_prefix_and_trims() {
        assert_eq!(normalize_gemini_model("  gemini-2.0-pro "), "gemini-2.0-pro");
        assert_eq!(
            normalize_gemini_model("models/gemini-2.0-pro"),
            "gemini-2.0-pro"
        );
        assert_eq!(
            normalize_gemini_model("  models/gemini-2.0-pro  "),
            "gemini-2.0-pro"
        );
    }

    #[test]
    fn gemini_model_encoded_url_keeps_single_segment() {
        let model = percent_encode_path_segment(normalize_gemini_model("models/a/b"));
        assert_eq!(model, "a%2Fb");
        assert!(!model.contains('/'));
    }

    // ── 输入长度上限 ────────────────────────────────────────────

    #[test]
    fn input_len_rejects_too_long() {
        let long = "x".repeat(MAX_INPUT_CHARS + 1);
        let req = EnhanceRequest::new(&long);
        let err = check_input_len(&req).unwrap_err();
        assert!(err.starts_with("config:"), "{err}");
        assert!(err.contains("20000"), "{err}");
    }

    #[test]
    fn input_len_accepts_boundary() {
        let exact = "x".repeat(MAX_INPUT_CHARS);
        let req = EnhanceRequest::new(&exact);
        assert!(check_input_len(&req).is_ok());
    }

    // ── 共享客户端 ──────────────────────────────────────────────

    #[test]
    fn http_client_is_shared_singleton() {
        let a = http_client().expect("客户端应构建成功");
        let b = http_client().expect("客户端应构建成功");
        assert!(std::ptr::eq(a, b), "客户端应为进程级单例");
    }

    // ── 出站 base_url 校验（凭据外泄防线）──────────────────────

    fn cfg_with_base(base_url: &str) -> LlmConfig {
        LlmConfig {
            name: "t".into(),
            protocol: "openai".into(),
            base_url: base_url.into(),
            model: "m".into(),
            api_key: "sk-test".into(),
            temperature: 0.7,
            timeout_secs: 0,
            max_tokens: 0,
        }
    }

    fn cfg_with_protocol(protocol: &str, base_url: &str) -> LlmConfig {
        LlmConfig {
            protocol: protocol.into(),
            ..cfg_with_base(base_url)
        }
    }

    #[test]
    fn checked_base_url_rejects_credential_bearing_url() {
        let err = checked_base_url(&cfg_with_base("https://user:pass@evil.com")).unwrap_err();
        assert!(err.starts_with("config:"), "{err}");
    }

    #[test]
    fn checked_base_url_rejects_non_http_scheme() {
        let err = checked_base_url(&cfg_with_base("file:///C:/Windows/win.ini")).unwrap_err();
        assert!(err.starts_with("config:"), "{err}");
    }

    /// 空地址必须被拒——否则 URL 拼接结果不可控。
    #[test]
    fn checked_base_url_rejects_empty() {
        let err = checked_base_url(&cfg_with_base("")).unwrap_err();
        assert!(err.starts_with("config:"), "{err}");
    }

    /// 缺协议的裸主机名必须被拒（否则会被当相对路径解析）。
    #[test]
    fn checked_base_url_rejects_missing_scheme() {
        let err = checked_base_url(&cfg_with_base("evil.com")).unwrap_err();
        assert!(err.starts_with("config:"), "{err}");
    }

    #[test]
    fn checked_base_url_accepts_normal_urls() {
        assert!(checked_base_url(&cfg_with_base("https://api.deepseek.com")).is_ok());
        assert!(checked_base_url(&cfg_with_base("http://localhost:11434")).is_ok());
    }

    /// 伪装地址必须在错误里点明「实际会连到哪个主机」——
    /// 只说「不得包含用户名/密码」，用户未必看得出这是钓鱼地址。
    #[test]
    fn checked_base_url_error_names_the_actual_host() {
        let err = checked_base_url(&cfg_with_base("https://api.openai.com@evil.com/v1")).unwrap_err();
        assert!(err.starts_with("config:"), "{err}");
        assert!(err.contains("evil.com"), "{err}");
        assert!(err.contains("实际会连接的主机"), "{err}");
    }

    /// 主机回显必须走白名单：不可信字符不得原样带进错误串（错误串会展示给用户）。
    #[test]
    fn actual_host_hint_only_returns_safe_hosts() {
        assert_eq!(
            actual_host_hint("https://api.openai.com@evil.com/v1"),
            Some("evil.com".to_string())
        );
        assert_eq!(
            actual_host_hint("https://evil.com:8443/v1"),
            Some("evil.com".to_string())
        );
        assert_eq!(actual_host_hint("https://[::1]:8080/v1"), Some("::1".to_string()));
        // 无协议 → 没有「会连到哪」的概念
        assert_eq!(actual_host_hint("evil.com"), None);
        // 主机含 HTML 元字符 → 不回显
        assert_eq!(actual_host_hint("https://ev<il.com/v1"), None);
        // 空
        assert_eq!(actual_host_hint(""), None);
    }

    // ── 出站总预算（降级重试不得突破总预算）──────────────────────

    /// 一次 attempt 最多 2 个出站请求（首发 + 降级重试），3 次 attempt → 6 个。
    #[test]
    fn max_outbound_requests_accounts_for_degrade_retry() {
        assert_eq!(MAX_OUTBOUND_REQUESTS, 6);
    }

    /// 总预算必须按「最坏请求数」算，而不是按 attempt 数算。
    ///
    /// 回归：外层墙钟上限曾按 `timeout * 3` 计算，但降级重试会让实际出站数翻倍，
    /// 导致降级重试被外层墙钟提前掐断。
    #[test]
    fn total_budget_scales_with_worst_case_request_count() {
        let cfg = LlmConfig {
            timeout_secs: 120,
            ..cfg_with_base("https://api.deepseek.com")
        };
        // 120s × 6 请求 + 30s 余量
        assert_eq!(total_outbound_budget_secs(&cfg), 120 * 6 + 30);
        // 必须严格大于只按 attempt 数算的旧值，否则降级重试仍会被掐断
        assert!(total_outbound_budget_secs(&cfg) > 120 * 3 + 30);
    }

    /// 预算耗尽后剩余时间必须为 0（用于跳过降级重试）。
    #[test]
    fn remaining_budget_is_zero_after_deadline() {
        let past = Instant::now() - Duration::from_secs(1);
        assert_eq!(remaining_budget(past), Duration::ZERO);
        let future = Instant::now() + Duration::from_secs(60);
        assert!(remaining_budget(future) > Duration::ZERO);
    }

    /// 回归：带凭据的 base_url 必须在**发出网络请求之前**被拒绝
    ///
    /// 否则恶意页面可 POST /api/config 写入攻击者的 base_url，
    /// 再 POST /api/enhance 让服务把用户的 API Key 发出去。
    #[tokio::test]
    async fn enhance_rejects_credential_bearing_base_url_before_request() {
        let req = EnhanceRequest::new("hi");
        let err = enhance(&cfg_with_base("https://user:pass@evil.com"), &req)
            .await
            .unwrap_err();
        assert!(err.starts_with("config:"), "{err}");
        assert!(err.contains("不安全") || err.contains("非法"), "{err}");
    }

    // ── 出站 URL 幂等（不重复拼接接口后缀）────────────────────

    fn cfg_proto(protocol: &str, base_url: &str, model: &str) -> LlmConfig {
        LlmConfig {
            protocol: protocol.into(),
            model: model.into(),
            ..cfg_with_base(base_url)
        }
    }

    /// 回归：用户粘贴完整端点 URL 时，出站不得重复拼接后缀。
    ///
    /// 这是「主推输入方式（粘贴完整 URL）」的必然失败路径：
    /// 修复前 ollama 会拼出 `/api/generate/api/generate` → 404。
    #[test]
    fn outbound_url_does_not_duplicate_endpoint_suffix() {
        // ollama：粘贴完整端点 URL
        let cfg = cfg_proto("ollama", "http://127.0.0.1:11434/api/generate", "llama3");
        assert_eq!(
            outbound_url(&cfg).unwrap(),
            "http://127.0.0.1:11434/api/generate"
        );
        // ollama：纯 base（无后缀）
        let cfg = cfg_proto("ollama", "http://127.0.0.1:11434", "llama3");
        assert_eq!(
            outbound_url(&cfg).unwrap(),
            "http://127.0.0.1:11434/api/generate"
        );

        // anthropic
        let cfg = cfg_proto("anthropic", "https://api.anthropic.com/v1/messages", "claude");
        assert_eq!(
            outbound_url(&cfg).unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        let cfg = cfg_proto("anthropic", "https://api.anthropic.com", "claude");
        assert_eq!(
            outbound_url(&cfg).unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        // base 自带 /v1 也不能拼成 /v1/v1/messages
        let cfg = cfg_proto("anthropic", "https://api.anthropic.com/v1", "claude");
        assert_eq!(
            outbound_url(&cfg).unwrap(),
            "https://api.anthropic.com/v1/messages"
        );

        // openai
        let cfg = cfg_proto("openai", "https://api.openai.com/v1/chat/completions", "gpt-4o");
        let url = outbound_url(&cfg).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(url.matches("/chat/completions").count(), 1, "后缀只应出现一次");

        // gemini：base 含 model 段时，要用配置里的 model 覆盖
        let cfg = cfg_proto(
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-1.5-pro:generateContent",
            "gemini-2.0-pro",
        );
        let url = outbound_url(&cfg).unwrap();
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent"
        );
        assert_eq!(url.matches(":generateContent").count(), 1, "方法段只应出现一次");

        // gemini：纯 base
        let cfg = cfg_proto("gemini", "https://generativelanguage.googleapis.com", "gemini-2.0-pro");
        assert_eq!(
            outbound_url(&cfg).unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent"
        );
    }

    /// 幂等性：对**出站结果**再跑一次，不得再叠后缀。
    ///
    /// 这是「幂等」的定义：无论 base 来自哪里，outbound_url 的输出再过一次仍是自身。
    #[test]
    fn outbound_url_is_idempotent() {
        for (protocol, base, model) in [
            ("ollama", "http://127.0.0.1:11434/api/generate", "llama3"),
            ("anthropic", "https://api.anthropic.com/v1/messages", "claude"),
            ("openai", "https://api.openai.com/v1/chat/completions", "gpt-4o"),
            (
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta/models/x:generateContent",
                "gemini-2.0-pro",
            ),
        ] {
            let cfg = cfg_proto(protocol, base, model);
            let once = outbound_url(&cfg).unwrap();
            let cfg2 = cfg_proto(protocol, &once, model);
            let twice = outbound_url(&cfg2).unwrap();
            assert_eq!(once, twice, "{protocol} 的 outbound_url 不幂等");
        }
    }

    /// Gemini 的 model 段必须**整段替换**，但前缀（版本段 / 网关前缀）必须保留。
    ///
    /// 剥掉前缀会打到错误的接口；沿用旧 model 名则调错模型。
    ///
    /// 特别断言：**不得出现 `/v1beta/v1beta` 双版本段** ——
    /// Google 官方文档给的就是 `…/v1beta/models/{model}:generateContent`，
    /// 用户粘完整端点是最常见用法，双版本段会让主路径 100% 失败。
    #[test]
    fn gemini_replaces_model_segment_but_keeps_prefix() {
        let cases = [
            // 有版本段 → 保留 /v1beta，不重复补版本段
            (
                "https://h/v1beta",
                "https://h/v1beta/models/m:generateContent",
            ),
            // 有 model 段 → 只换 model，保留前缀
            (
                "https://h/v1beta/models/old-model",
                "https://h/v1beta/models/m:generateContent",
            ),
            // 网关自建前缀 → 整段保留
            (
                "https://gw.example.com/gemini/v1beta/models/old",
                "https://gw.example.com/gemini/v1beta/models/m:generateContent",
            ),
            // 无版本段 → 补默认 /v1beta
            ("https://h", "https://h/v1beta/models/m:generateContent"),
            // 只剥最后一段 models（前面的同名目录不动）
            (
                "https://h/models/v1/models/x",
                "https://h/models/v1/models/m:generateContent",
            ),
            // 官方完整端点（用户最常粘贴的形态）→ 只换 model
            (
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-1.5-pro:generateContent",
                "https://generativelanguage.googleapis.com/v1beta/models/m:generateContent",
            ),
            // 网关 + 官方完整端点 → 前缀与版本段都要保留
            (
                "https://gw.example.com/gemini/v1beta/models/gemini-1.5-pro:generateContent",
                "https://gw.example.com/gemini/v1beta/models/m:generateContent",
            ),
        ];
        for (base, want) in cases {
            let cfg = cfg_proto("gemini", base, "m");
            let url = outbound_url(&cfg).unwrap();
            assert_eq!(url, want, "base={base}");
            assert!(
                !url.contains("/v1beta/v1beta"),
                "不得出现双版本段: {url}（base={base}）"
            );
        }
    }

    /// 出站 URL 的**行为锚**（与内部实现无关）。
    ///
    /// 这些断言不预设「server 是先剥一层再调 core」还是「直接调 core」，
    /// 只约束**对外可观察的最终 URL**：
    /// 1. 单层粘贴（完整端点 / 纯 base）→ 出站 URL 与 core 的
    ///    `resolve_endpoint` **逐字节相等** —— server 与 core 不允许有两种口径；
    /// 2. 出站 URL 中，本协议后缀表里的**任一后缀最多出现一次**；
    /// 3. 出站 URL 不得出现「同一后缀段连续出现两次」。
    #[test]
    fn outbound_url_behaviour_anchors() {
        // (protocol, base_url, model, 出站 URL 的接口后缀层数)
        let cases: [(&str, &str, &str, usize); 19] = [
            // ── openai：完整端点 / 纯 base / 带版本段 ──
            ("openai", "https://api.openai.com/v1/chat/completions", "gpt-4o", 1),
            ("openai", "https://api.openai.com", "gpt-4o", 1),
            ("openai", "https://api.deepseek.com/v1", "deepseek-chat", 1),
            // ── anthropic ──
            ("anthropic", "https://api.anthropic.com/v1/messages", "claude", 1),
            ("anthropic", "https://api.anthropic.com", "claude", 1),
            ("anthropic", "https://api.anthropic.com/v1", "claude", 1),
            // ── ollama ──
            ("ollama", "http://127.0.0.1:11434/api/generate", "llama3", 1),
            ("ollama", "http://127.0.0.1:11434/api/chat", "llama3", 1),
            ("ollama", "http://127.0.0.1:11434", "llama3", 1),
            // ── gemini ──
            (
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
                "gemini-2.0-pro",
                1,
            ),
            (
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta",
                "gemini-2.0-pro",
                1,
            ),
            (
                "gemini",
                "https://generativelanguage.googleapis.com",
                "gemini-2.0-pro",
                1,
            ),
            // ── 跨协议：所选协议的后缀表命不中用户粘贴的端点 ──
            // core 明确**不跨协议乱剥**（`strip_endpoint_suffix` 有专门测试
            // `strip_endpoint_suffix_does_not_cross_protocols`），因此出站 URL
            // 会同时含「别的协议的后缀」+「所选协议的后缀」两层 ——
            // 这是**预期行为**，不是缺陷（前端已通过 `stripped_protocol` 字段
            // 提示用户「这属于别的协议」）。
            ("openai", "https://api.anthropic.com/v1/messages", "gpt-4o", 2),
            ("anthropic", "https://api.openai.com/v1/chat/completions", "claude", 2),
            // ── 后缀叠了两遍：必须收敛到 1 层 ──
            (
                "openai",
                "https://api.openai.com/v1/chat/completions/chat/completions",
                "gpt-4o",
                1,
            ),
            (
                "anthropic",
                "https://api.anthropic.com/v1/messages/v1/messages",
                "claude",
                1,
            ),
            (
                "ollama",
                "http://127.0.0.1:11434/api/generate/api/generate",
                "llama3",
                1,
            ),
            // ── 接口路径大小写不一致 ──
            ("openai", "https://api.openai.com/V1/Chat/Completions", "gpt-4o", 1),
            ("ollama", "http://127.0.0.1:11434/API/GENERATE", "llama3", 1),
        ];
        for (protocol, base, model, want_layers) in cases {
            let url = outbound_url(&cfg_proto(protocol, base, model)).unwrap();
            // 锚 0：这批用例的 base_url 都是规范形态 —— core 的校验不改变它，
            // 因此「server 出站结果 ≡ core 对**原始输入**的产出」是可断言的。
            assert_eq!(
                prompt_forge_core::endpoint::validate_base_url(base).unwrap(),
                base,
                "用例的 base_url 应当已是规范形态: {base}"
            );
            // 锚 1：与 core 逐字节相等 —— server 侧不得有额外语义
            assert_eq!(
                url,
                prompt_forge_core::endpoint::resolve_endpoint(base, protocol, model),
                "server 必须与 core 给出同一个 URL: protocol={protocol} base={base}"
            );
            // 锚 2：出站 URL 的接口后缀层数（用全协议合并表互斥剥计数 ——
            // 单协议表会把跨协议双语义漏检成 1 层）
            assert_eq!(
                endpoint_suffix_layers(&url),
                want_layers,
                "出站 URL 的接口后缀层数不符: {url}（protocol={protocol} base={base}）"
            );
        }
    }

    /// 用**全协议合并表**反复剥最长匹配，统计出站 URL 里的接口后缀层数。
    ///
    /// 为什么必须用合并表（而不是所选协议的单表）：
    /// 跨协议时出站 URL 可能同时含**两个协议**的后缀段
    /// （如 `…/v1/messages/chat/completions`），单表只会剥掉一层就停下，
    /// 把「双语义」漏检成「1 层」。
    ///
    /// 为什么是「剥」而不是「数子串」：
    /// 正确 URL `https://api.openai.com/v1/chat/completions` 本身就同时含
    /// `/chat/completions` 与 `/completions` 两个子串 —— 按子串求和计数会误判。
    fn endpoint_suffix_layers(url: &str) -> usize {
        // 后缀表直接引用 core 的 pub 常量 —— server 侧不另抄一份
        let tables: [&[&'static str]; 4] = [
            prompt_forge_core::endpoint::OPENAI_SUFFIXES,
            prompt_forge_core::endpoint::ANTHROPIC_SUFFIXES,
            prompt_forge_core::endpoint::OLLAMA_SUFFIXES,
            prompt_forge_core::endpoint::GEMINI_SUFFIXES,
        ];
        let mut cur = url.to_string();
        let mut layers = 0usize;
        loop {
            let lower = cur.to_ascii_lowercase();
            let mut best: Option<&'static str> = None;
            for table in tables {
                for &suf in table {
                    let sl = suf.to_ascii_lowercase();
                    if sl.len() <= lower.len()
                        && lower.ends_with(&sl)
                        && best.is_none_or(|b| b.len() < suf.len())
                    {
                        best = Some(suf);
                    }
                }
            }
            let Some(suf) = best else { break };
            cur.truncate(cur.len() - suf.len());
            layers += 1;
            if layers > 32 {
                break; // 防御：理论上剥几次就到底了
            }
        }
        layers
    }

    /// 带 query / fragment 的 base_url：server 的判定必须与 core 一致，
    /// 且放行时产出的 URL 必须与 core **逐字节相等**。
    ///
    /// query / fragment 归 core 所有（core 可能随版本切换口径：
    /// 「一律拒绝」↔「合法则放行并原样搬运」），
    /// 所以这里用**条件式断言**而不是写死某一侧的结果 ——
    /// core 改口径时这条测试自动跟随，永远不会红。
    #[test]
    fn outbound_url_with_query_matches_core_verdict_and_result() {
        for (protocol, base, model) in [
            (
                "openai",
                "https://api.openai.com/v1/chat/completions?k=1",
                "gpt-4o",
            ),
            (
                "openai",
                "https://api.deepseek.com/v1?api-version=2024-01-01",
                "gpt-4o",
            ),
            (
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta?key=ABC123",
                "gemini-2.0-pro",
            ),
        ] {
            let core_ok = prompt_forge_core::endpoint::validate_base_url(base).is_ok();
            match outbound_url(&cfg_proto(protocol, base, model)) {
                Ok(url) => {
                    assert!(core_ok, "core 判定为非法，server 不得放行: {base}");
                    assert_eq!(
                        url,
                        prompt_forge_core::endpoint::resolve_endpoint(base, protocol, model),
                        "server 必须与 core 给出同一个 URL: protocol={protocol} base={base}"
                    );
                }
                Err(e) => {
                    assert!(!core_ok, "core 判定为合法，server 不得拒绝: {base}");
                    assert!(
                        e.starts_with("config:"),
                        "错误串必须严格 CODE:message 约定: {e}"
                    );
                }
            }
        }
    }

    /// 后缀被**叠了两遍**的病态输入：出站 URL 里该后缀不得出现两次。
    ///
    /// 「用户把同一段端点粘贴了两次」几乎总是手误；出站拼装的目标是产出
    /// 「可直接发请求、且不含重复后缀段」的 URL —— 叠两遍时 404 是必然结果。
    ///
    /// 这里只断言**行为**（不重复），不锁死具体拼装路径：
    /// 无论 server 是「先剥一层再调 core」还是「直接调 core」，
    /// 只要最终 URL 里该后缀只出现一次就算通过。
    #[test]
    fn outbound_url_never_duplicates_doubled_endpoint_suffix() {
        let cases: [(&str, &str, &str); 4] = [
            (
                "ollama",
                "http://127.0.0.1:11434/api/generate/api/generate",
                "/api/generate",
            ),
            (
                "ollama",
                "http://127.0.0.1:11434/api/chat/api/chat",
                "/api/chat",
            ),
            (
                "anthropic",
                "https://api.anthropic.com/v1/messages/v1/messages",
                "/v1/messages",
            ),
            (
                "openai",
                "https://api.openai.com/v1/chat/completions/chat/completions",
                "/chat/completions",
            ),
        ];
        for (protocol, base, suffix) in cases {
            let url = outbound_url(&cfg_proto(protocol, base, "m")).unwrap();
            assert!(
                url.matches(suffix).count() <= 1,
                "后缀 {suffix} 最多出现一次: {url}（protocol={protocol} base={base}）"
            );
            assert!(
                !url.contains(&format!("{suffix}{suffix}")),
                "后缀 {suffix} 不得连续出现两次: {url}（protocol={protocol} base={base}）"
            );
        }
    }

    /// 四条协议的 base_url 形态矩阵：根地址 / 完整端点都要归一到同一个最终 URL。
    ///
    /// 覆盖 audit-core 给出的 17 例形态中 server 侧相关的部分 ——
    /// 「粘贴完整端点」与「只填域名」必须得到**同一个**出站 URL。
    #[test]
    fn outbound_url_normalizes_base_and_full_endpoint_to_same_url() {
        let cases: [(&str, &str, &str, &str); 8] = [
            (
                "openai",
                "https://api.openai.com",
                "https://api.openai.com/v1/chat/completions",
                "gpt-4o",
            ),
            (
                "openai",
                "https://api.openai.com/v1/chat/completions",
                "https://api.openai.com/v1/chat/completions",
                "gpt-4o",
            ),
            (
                "openai",
                "https://api.deepseek.com",
                "https://api.deepseek.com/v1/chat/completions",
                "deepseek-chat",
            ),
            (
                "anthropic",
                "https://api.anthropic.com",
                "https://api.anthropic.com/v1/messages",
                "claude",
            ),
            (
                "anthropic",
                "https://api.anthropic.com/v1/messages",
                "https://api.anthropic.com/v1/messages",
                "claude",
            ),
            (
                "anthropic",
                "https://api.anthropic.com/v1",
                "https://api.anthropic.com/v1/messages",
                "claude",
            ),
            (
                "ollama",
                "http://127.0.0.1:11434",
                "http://127.0.0.1:11434/api/generate",
                "llama3",
            ),
            (
                "ollama",
                "http://127.0.0.1:11434/api/chat",
                "http://127.0.0.1:11434/api/generate",
                "llama3",
            ),
        ];
        for (protocol, base, want, model) in cases {
            let cfg = cfg_proto(protocol, base, model);
            assert_eq!(outbound_url(&cfg).unwrap(), want, "protocol={protocol} base={base}");
        }
    }

    /// base_url 的 query / fragment 判定必须**与 core 完全一致**。
    ///
    /// ## 为什么是「与 core 一致」而不是「server 自己写一条」
    ///
    /// base_url 能否带 query 是**core 的规则**，不是 server 的规则：
    /// - core `validate_base_url` 对带 `?` / `#` 的 base_url 一律**拒绝**
    ///   （base_url 契约是「不含 query / fragment」——base_url 会明文存进
    ///   config.json，放行 `?key=xxx` 会让 API Key 明文落盘，与本项目
    ///   「API Key 走 DPAPI 加密 + 请求头传递」的安全模型冲突）；
    /// - core 的 `resolve_endpoint` 出站拼装时也把 query / fragment 段**剥离**，
    ///   不把它们带进实际请求的 URL。
    ///
    /// server 侧若自己写一份规则，就等于**在 core 之外又开一套**，
    /// core 一旦改口径，server 侧这条测试立刻红 —— 那是 server 的
    /// 规则漂移，不是 core 的 bug。
    ///
    /// 因此这里断言的是**不变量**：server 的判定 ≡ core 的判定。
    /// core 改口径（放行 / 收紧）时 server 自动跟随，**永不红**，
    /// 而「server 不自建一套规则」这个真正的目标被钉住了。
    ///
    /// 另外单独立一条断言守住**任何口径下都必须成立**的底线：
    /// 含 HTML 元字符的 query 必须被拒（base_url 会回填前端并拼进 HTML）。
    #[test]
    fn base_url_query_or_fragment_matches_core_verdict() {
        for u in [
            // 带 query / fragment 的合法形态
            // （core 的口径由 core 组决定：可能「一律拒绝」，也可能「合法则放行」——
            //   这里只断言两侧结论一致，不写死某一侧）
            "https://generativelanguage.googleapis.com/v1beta?key=ABC123",
            "https://api.example.com/v1?api-version=2024-01-01",
            "https://api.example.com/v1/chat/completions#frag",
            // 非法形态（任何口径下都必须被拒）
            "https://x.com/v1?q=<script>alert(1)</script>",
            "https://x.com/v1?q=\"onmouseover=\"alert(1)",
            // 无 query 的普通地址（两侧都必须放行）
            "https://api.deepseek.com/v1",
        ] {
            let core_ok = prompt_forge_core::endpoint::validate_base_url(u).is_ok();
            let server_ok = outbound_url(&cfg_proto("openai", u, "gpt-4o")).is_ok();
            assert_eq!(
                core_ok, server_ok,
                "server 侧判定必须与 core 一致: {u:?}（core={core_ok}, server={server_ok}）"
            );
        }
    }

    /// 含 HTML 元字符的 query / fragment 必须被拒 —— **任何 core 口径下都成立**。
    ///
    /// base_url 会回填前端表单并参与 DOM 渲染（`web/app.js` 的预览用 innerHTML），
    /// `<` `>` `"` `'` 是 HTML 注入载体。
    ///
    /// 当前 core 口径下，任何 `?` / `#` 都会被 `validate_base_url` 专门拒绝
    /// （错误文案不回显 query 内容），因此本断言天然成立；即使将来 core
    /// 改回「放行」，这条断言也仍然守得住 HTML 注入这条底线。
    #[test]
    fn base_url_with_html_metacharacters_in_query_is_rejected() {
        for bad in [
            "https://x.com/v1?q=<script>alert(1)</script>",
            "https://x.com/v1?q=\"onmouseover=\"alert(1)",
            "https://x.com/v1?q='+alert(1)+'",
            "https://x.com/v1?q=><img src=x onerror=alert(1)>",
        ] {
            let cfg = cfg_proto("openai", bad, "gpt-4o");
            let err = outbound_url(&cfg).unwrap_err();
            assert!(
                err.starts_with("config:"),
                "含 HTML 元字符的 query 必须被拒绝: {bad:?} → {err}"
            );
        }
    }

    /// 四条出站协议路径都必须装上 base_url 闸门——漏一条就等于该协议仍可外泄凭据。
    ///
    /// 校验发生在发请求之前，所以这些用例不会真的发起网络请求。
    #[tokio::test]
    async fn all_protocols_reject_unsafe_base_url() {
        for protocol in ["openai", "anthropic", "ollama", "gemini"] {
            for bad in [
                "https://user:pass@evil.com",
                "https://api.openai.com@evil.com",
                "file:///etc/passwd",
                "ftp://evil.com",
                "",
            ] {
                let cfg = cfg_with_protocol(protocol, bad);
                let err = enhance(&cfg, &EnhanceRequest::new("hi"))
                    .await
                    .unwrap_err();
                assert!(
                    err.starts_with("config:"),
                    "protocol={protocol} base_url={bad:?} 应被 config: 前缀拒绝，实际: {err}"
                );
            }
        }
    }
}

