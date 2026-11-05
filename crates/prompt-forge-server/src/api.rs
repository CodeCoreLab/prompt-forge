//! JSON API 层：与历史版本 Tauri commands 一一对应的 HTTP 端点。
//!
//! 契约约定（对外接口稳定，语义与旧版完全一致）：
//! - 成功：HTTP 200 + `{"ok":true, ...}`
//! - 失败：HTTP 4xx/5xx + `{"ok":false, "error":"CODE:message"}`
//!   `CODE` 沿用旧版错误码（unauthorized / forbidden / rate_limited / timeout /
//!   server / network / http / config / no_profile / empty_result / cancelled），
//!   前端 `renderError` 逻辑可原样复用。
//!
//! 端点清单：
//! | 方法 | 路径 | 对应旧命令 |
//! |---|---|---|
//! | GET  | /api/modes     | get_modes |
//! | GET  | /api/config    | get_config |
//! | POST | /api/config    | save_config |
//! | POST | /api/test      | test_llm |
//! | POST | /api/enhance   | enhance_prompt |
//! | POST | /api/cancel    | cancel_enhance |
//! | GET  | /api/history   | get_history |
//! | POST | /api/clipboard | copy_to_clipboard |
//!
//! 2026-11-05 新增（向后兼容，不改动上述契约）：
//! | GET  | /api/methods   | 增强方式列表 + 思考强度分级 |
//! | GET  | /api/roles     | 角色列表（内置 + 自定义）+ 当前生效角色 |
//! | POST | /api/parse-url | 端点解析（host/port/path/接口路径回填） |

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use prompt_forge_core::config::{AppConfig, LlmConfig};
use prompt_forge_core::endpoint;
use prompt_forge_core::methods::{self, ThinkingLevel};
use prompt_forge_core::modes;
use prompt_forge_core::roles;

use crate::llm;
use crate::state::AppState;
use crate::store;

/// 统一错误类型：`CODE:message` 形态，与旧版一致
pub struct ApiError {
    status: StatusCode,
    code: String,
    message: String,
}

impl ApiError {
    /// 业务错误 → HTTP 400
    pub fn bad(code: &str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: code.into(),
            message: message.into(),
        }
    }

    /// 内部错误 → HTTP 500
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal".into(),
            message: message.into(),
        }
    }

    /// 从旧版错误串 `CODE:message` 解析（保持前端行为等价）
    pub fn from_legacy(err: String) -> Self {
        let (code, message) = match err.split_once(':') {
            Some((c, m)) => (c.to_string(), m.to_string()),
            None => ("error".to_string(), err),
        };
        // 已知业务码 → 400；未知 → 500
        let status = match code.as_str() {
            "unauthorized" | "forbidden" | "rate_limited" | "config" | "no_profile"
            | "empty_result" | "cancelled" | "http" | "timeout" | "network" | "server" => {
                StatusCode::BAD_REQUEST
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            code,
            message,
        }
    }

    /// 适配 `map_err`：把纯消息包成 `config` 类业务错误
    pub fn from_msg(message: String) -> Self {
        Self::bad("config", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({
            "ok": false,
            "error": format!("{}:{}", self.code, self.message),
            "code": self.code,
            "message": self.message,
        });
        (self.status, Json(body)).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// 模式列表 = 内置 + 用户自定义
pub async fn get_modes(State(_st): State<AppState>) -> ApiResult<Json<Value>> {
    let cfg = store::load_config().unwrap_or_default();
    let mut list: Vec<Value> = modes::MODES
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "name": m.name,
                "description": m.description,
            })
        })
        .collect();
    for m in &cfg.custom_modes {
        list.push(json!({
            "id": m.full_id(),
            "name": m.name,
            "description": format!("[自定义·{}]", if m.kind == "extend" { "继承" } else { "替换" }),
        }));
    }
    Ok(Json(json!({ "ok": true, "modes": list })))
}

/// 读取配置（API Key 解密为明文供前端编辑）
pub async fn get_config(State(_st): State<AppState>) -> ApiResult<Json<Value>> {
    let mut cfg = store::load_config_for_client().map_err(ApiError::internal)?;
    // 回填系统实际自启状态（注册表真值），避免配置与系统状态不一致时开关显示错误
    cfg.autostart = crate::autostart::is_enabled();
    let value = serde_json::to_value(cfg).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(json!({ "ok": true, "config": value })))
}

/// 保存配置
pub async fn save_config(
    State(st): State<AppState>,
    Json(payload): Json<Value>,
) -> ApiResult<Json<Value>> {
    // 兼容两种载荷形态：{config:{...}} 或直接 {...}
    let raw = payload
        .get("config")
        .cloned()
        .unwrap_or(payload);
    let cfg: AppConfig = serde_json::from_value(raw).map_err(|e| {
        ApiError::bad("config", format!("配置格式错误: {e}"))
    })?;

    // 配置互斥锁：保证读改写原子性
    let _guard = st.config_lock.lock().await;

    // 自启动偏好即时生效（写入注册表）
    crate::autostart::apply(cfg.autostart).map_err(ApiError::from_msg)?;

    store::save_config(&cfg).map_err(|e| ApiError::bad("config", e))?;
    Ok(Json(json!({ "ok": true })))
}

/// 测试连接
pub async fn test_llm(
    State(_st): State<AppState>,
    Json(payload): Json<Value>,
) -> ApiResult<Json<Value>> {
    let raw = payload
        .get("profile")
        .cloned()
        .unwrap_or(payload);
    let profile: LlmConfig =
        serde_json::from_value(raw).map_err(|e| ApiError::bad("config", format!("配置格式错误: {e}")))?;
    // 连通性测试走最小思考强度：目的是验证连通性，不是消耗推理预算
    let request = llm::EnhanceRequest::new("回复：连接成功").with_thinking(ThinkingLevel::Off);
    match llm::enhance(&profile, &request).await {
        Ok(text) => Ok(Json(json!({ "ok": true, "output": text }))),
        Err(e) => Err(ApiError::from_legacy(e)),
    }
}

/// 增强提示词（可被 /api/cancel 中断）
///
/// 生效参数优先级：**请求体覆盖 > 保存的配置**（前端切换增强方式/思考强度/角色后
/// 立即生效，即便用户还没点保存）。
pub async fn enhance(
    State(st): State<AppState>,
    Json(payload): Json<Value>,
) -> ApiResult<Json<Value>> {
    let input = payload
        .get("input")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mode_id = payload
        .get("mode_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // 取配置快照（加锁，避免与保存并发导致半更新状态）
    let cfg = {
        let _guard = st.config_lock.lock().await;
        store::load_config().map_err(ApiError::internal)?
    };

    // ── 解析生效的增强选项（请求体可覆盖配置）──────────────────
    let opt = cfg.enhance.clone();
    let method_id = payload
        .get("method_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&opt.method_id)
        .to_string();
    let thinking_raw = payload
        .get("thinking")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&opt.thinking)
        .to_string();
    let role_id = payload
        .get("role_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&opt.role_id)
        .to_string();

    // 组装提示词：custom-* 前缀走自定义模式，其余走内置模板
    let prompt = if let Some(custom_id) = mode_id.strip_prefix("custom-") {
        let mode = cfg
            .custom_modes
            .iter()
            .find(|m| m.id == custom_id)
            .ok_or_else(|| ApiError::bad("config", format!("自定义模式不存在: {mode_id}")))?;
        modes::build_custom_prompt_with(mode, Some(&method_id), &input).map_err(ApiError::from_msg)?
    } else {
        modes::build_prompt_with(&mode_id, Some(&method_id), &input).map_err(ApiError::from_msg)?
    };

    let mut profile = cfg
        .active()
        .ok_or_else(|| {
            ApiError::bad(
                "no_profile",
                "尚未配置大模型，请先在设置中添加配置",
            )
        })?
        .clone();
    // DPAPI 密文 → 明文，供出站 HTTP 使用
    profile.api_key = prompt_forge_core::crypto::load_key(&profile.api_key);

    // 角色：解析为 system prompt 前置
    let role = roles::resolve(&role_id, &cfg.custom_roles);
    let thinking_level = ThinkingLevel::parse(&thinking_raw);

    // 新一轮：废弃旧轮次，登记新令牌
    let token = st.begin_enhance();

    // 可中断执行
    let request = llm::EnhanceRequest::new(&prompt)
        .with_system(&role.system_prompt)
        .with_thinking(thinking_level);
    let result = tokio::select! {
        r = llm::enhance(&profile, &request) => r,
        _ = token.cancelled() => Err("cancelled:已取消".to_string()),
    };

    match result {
        Ok(output) => {
            store::push_history(store::HistoryEntry {
                input,
                mode_id,
                output: output.clone(),
                ts: store::now_secs(),
            });
            Ok(Json(json!({
                "ok": true,
                "output": output,
                "applied": {
                    "method_id": method_id,
                    "thinking": thinking_level.id(),
                    "role_id": role.id,
                    "role_name": role.name,
                }
            })))
        }
        Err(e) => Err(ApiError::from_legacy(e)),
    }
}

/// 增强方式列表 + 深度思考强度分级
pub async fn get_methods(State(_st): State<AppState>) -> ApiResult<Json<Value>> {
    let method_list: Vec<Value> = methods::METHODS
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "name": m.name,
                "description": m.description,
            })
        })
        .collect();
    let thinking_list: Vec<Value> = ThinkingLevel::ALL
        .iter()
        .map(|l| {
            json!({
                "id": l.id(),
                "name": l.name(),
                "description": l.description(),
                "budget_tokens": l.budget_tokens(),
            })
        })
        .collect();
    Ok(Json(json!({
        "ok": true,
        "methods": method_list,
        "thinking_levels": thinking_list,
        "default_method_id": methods::DEFAULT_METHOD_ID,
        "default_thinking": methods::DEFAULT_THINKING_ID,
    })))
}

/// 角色列表（内置 + 用户自定义）+ 当前生效角色
pub async fn get_roles(State(_st): State<AppState>) -> ApiResult<Json<Value>> {
    let cfg = store::load_config().unwrap_or_default();
    let mut list: Vec<Value> = roles::ROLES
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "name": r.name,
                "description": r.description,
                "builtin": true,
            })
        })
        .collect();
    for r in &cfg.custom_roles {
        list.push(json!({
            "id": r.full_id(),
            "name": r.name,
            "description": "[自定义角色]",
            "builtin": false,
        }));
    }
    let current = roles::resolve(&cfg.enhance.role_id, &cfg.custom_roles);
    Ok(Json(json!({
        "ok": true,
        "roles": list,
        "current": current,
    })))
}

/// 端点解析：粘贴完整 URL 后自动回填基础地址
pub async fn parse_url(Json(payload): Json<Value>) -> ApiResult<Json<Value>> {
    let raw = payload
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let protocol = payload
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("openai")
        .to_string();

    match endpoint::parse_endpoint(&raw) {
        Some(p) => Ok(Json(json!({
            "ok": true,
            "scheme": p.scheme,
            "host": p.host,
            "port": p.port,
            "path": p.path,
            "base_url": p.base_url,
            "notes": p.notes,
            "suggested_endpoint": endpoint::endpoint_for_protocol(&protocol),
            "normalized": endpoint::normalize_base_url(&p.base_url, &protocol),
        }))),
        None => Err(ApiError::bad("config", "无法解析该地址，请检查是否包含主机名")),
    }
}

/// 取消当前增强
pub async fn cancel(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    st.trigger_cancel();
    Ok(Json(json!({ "ok": true })))
}

/// 读取历史（最新在前）
pub async fn get_history(State(_st): State<AppState>) -> ApiResult<Json<Value>> {
    let hist = store::load_history();
    Ok(Json(json!({ "ok": true, "history": hist })))
}

/// 复制文本到系统剪贴板
pub async fn copy_to_clipboard(Json(payload): Json<Value>) -> ApiResult<Json<Value>> {
    let text = payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    copy_text(&text).map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })))
}

/// 写入系统剪贴板（Windows 用 clip.exe，UTF-16LE 输入避免乱码）
fn copy_text(text: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new("clip.exe")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 clip.exe 失败: {e}"))?;
        if let Some(stdin) = child.stdin.as_mut() {
            let wide: Vec<u8> = text
                .encode_utf16()
                .flat_map(|u| u.to_le_bytes())
                .collect();
            stdin.write_all(&wide).map_err(|e| format!("写入剪贴板失败: {e}"))?;
        }
        let _ = child.wait();
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = text;
        Err("当前平台未实现剪贴板写入".into())
    }
}
