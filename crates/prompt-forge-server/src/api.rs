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
    /// 请求标识（服务端分配的权威序号）——前端据此判定「我的请求是否已过期」。
    ///
    /// 放在错误体里是关键：错误也可能因「被新请求取代」而产生，
    /// 前端拿到旧请求的错误时需要能识别出「这已经是上一轮了」。
    request_id: Option<u64>,
}

impl ApiError {
    /// 业务错误 → HTTP 400
    pub fn bad(code: &str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: code.into(),
            message: message.into(),
            request_id: None,
        }
    }

    /// 内部错误 → HTTP 500
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal".into(),
            message: message.into(),
            request_id: None,
        }
    }

    /// 附加请求标识（服务端分配的轮次序号）
    pub fn with_request_id(mut self, id: u64) -> Self {
        self.request_id = Some(id);
        self
    }

    /// 从旧版错误串 `CODE:message` 解析（保持前端行为等价）
    ///
    /// 错误码 → HTTP 状态码的映射**语义化**：
    /// - 凭据类（401/403）、限流（429）、超时（504）、上游故障（502）各有对应状态码；
    /// - 业务错误（config / no_profile / empty_result / cancelled / superseded）才是 400。
    ///
    /// 注意：前端 `api()` 只读响应体的 `code` 字段，**不依赖 HTTP 状态码**，
    /// 因此这里的状态码调整不会改变前端行为（`resp.ok` 判定仍成立）。
    pub fn from_legacy(err: String) -> Self {
        let (code, message) = match err.split_once(':') {
            Some((c, m)) => (c.to_string(), m.to_string()),
            None => ("error".to_string(), err),
        };
        let status = match code.as_str() {
            "unauthorized" => StatusCode::UNAUTHORIZED,
            "forbidden" => StatusCode::FORBIDDEN,
            "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
            "timeout" => StatusCode::GATEWAY_TIMEOUT,
            // 上游服务故障 / 网络不可达 / 上游返回非 2xx：都是网关侧问题
            "server" | "network" | "http" => StatusCode::BAD_GATEWAY,
            // 业务错误：请求本身不合法
            "config" | "no_profile" | "empty_result" | "cancelled" | "superseded" => {
                StatusCode::BAD_REQUEST
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            code,
            message,
            request_id: None,
        }
    }

    /// 适配 `map_err`：把纯消息包成 `config` 类业务错误
    pub fn from_msg(message: String) -> Self {
        Self::bad("config", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "ok": false,
            "error": format!("{}:{}", self.code, self.message),
            "code": self.code,
            "message": self.message,
        });
        if let Some(id) = self.request_id {
            body["request_id"] = json!(id);
        }
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

/// 写入侧的接口地址安全闸门。
///
/// ## 为什么写入侧也要拦
///
/// 出站前还有一道（server/llm.rs 的 `checked_base_url`），但**写入侧必须也拦**：
/// 否则恶意页面可 POST /api/config 把攻击者的 base_url 写进配置，
/// 用户下次点「测试连接」就会把自己的 API Key 发出去 —— 延迟触发更难察觉。
///
/// ## 为什么空地址放行
///
/// 与 core 的 `AppConfig::validate`（core config.rs）保持**同一套规则**：
/// 空地址是合法的「还没填完」，不该在「保存整份配置」时被误判非法。
/// 若这里比 core 严一档，一个空档位就会把**整份配置**（含用户改的
/// 自定义角色、模式、autostart）一起锁死 —— 一个空档位把无关设置全废。
///
/// 真正拦截空地址的位置是**出站侧** `llm::checked_base_url`
/// （空地址直接拒绝），写入侧不必重复收紧。
///
/// 非空地址里仍必须过白名单：`https://user:pass@evil.com` 这类伪装地址
/// 写进配置就等于把凭据送到错误的服务方。
fn check_write_side_base_urls(profiles: &[LlmConfig]) -> Result<(), ApiError> {
    for p in profiles {
        if p.base_url.trim().is_empty() {
            continue;
        }
        prompt_forge_core::endpoint::validate_base_url(&p.base_url).map_err(|e| {
            // 点明「实际会连接的主机」：`https://user:pass@evil.com` 这类伪装地址，
            // core 的文案只说「不得包含用户名/密码（user:pass@host）」——
            // 用户看到的是**占位说明**，完全看不出真正会连的是 `evil.com`。
            // 写入侧是用户填错地址的第一道反馈，恰恰最需要点明。
            let mut msg = format!("配置「{}」的接口地址非法: {e}", p.name);
            if let Some(host) = crate::llm::actual_host_hint(&p.base_url) {
                msg.push_str(&format!("，实际会连接的主机是 {host}"));
            }
            ApiError::bad("config", msg)
        })?;
    }
    Ok(())
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

    // 接口地址安全闸门（写入侧）
    check_write_side_base_urls(&cfg.profiles)?;

    // 配置互斥锁：保证读改写原子性
    let _guard = st.config_lock.lock().await;

    // 先落盘配置，再改注册表。
    //
    // 顺序很重要：反过来（先写注册表）时，若落盘失败，注册表已经被改了
    // （自启状态与磁盘配置不一致），且配置里的 autostart 字段根本没生效 ——
    // 用户看到「保存失败」，开机自启却已经开了。
    // 先落盘后改注册表：落盘失败则注册表未动，状态一致。
    store::save_config(&cfg).map_err(|e| ApiError::bad("config", e))?;

    // 自启动偏好即时生效（写入注册表）
    //
    // 这一步失败不回滚已落盘的配置：注册表与磁盘配置短暂不一致，
    // 下次 get_config 会用注册表真值回填（见 get_config），状态可自愈。
    crate::autostart::apply(cfg.autostart).map_err(ApiError::from_msg)?;

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

/// 按下标从**磁盘当前配置**里取出生效的配置档。
///
/// ## 为什么必须以「磁盘当前配置」为准
///
/// 请求体里的 `profile_index` 是前端选中的下标。前端删掉一档后必须先落盘，
/// 否则这个下标会指向**已被删除的那一档** —— 于是请求带着旧档的 API Key
/// 发往**旧档的主机**，而用户以为自己在用新档（Key 发往错误服务方）。
///
/// 因此这里的索引只在**当次读到的磁盘配置**里解析：
/// - 索引越界 → 明确报错 `no_profile`，绝不回退到别的档（回退 = 把 Key 发给
///   另一个主机，与「用户以为在用 A 实际用了 B」同一类事故）；
/// - `profiles` 为空 → 报「尚未配置大模型」。
fn select_profile(cfg: &AppConfig, profile_index: usize) -> Result<LlmConfig, ApiError> {
    cfg.profiles.get(profile_index).cloned().ok_or_else(|| {
        ApiError::bad(
            "no_profile",
            if cfg.profiles.is_empty() {
                "尚未配置大模型，请先在设置中添加配置".to_string()
            } else {
                format!("配置档不存在（索引 {profile_index}）")
            },
        )
    })
}

/// 增强提示词（可被 /api/cancel 中断）
///
/// 生效参数优先级：**请求体覆盖 > 保存的配置**（前端切换增强方式/思考强度/角色后
/// 立即生效，即便用户还没点保存）。
///
/// 配置档选择：请求体可带 `profile_index`（前端当前选中的档位下标）覆盖磁盘上的
/// `active_index` —— 否则用户在前端切换了档位但未保存时，请求会打到旧档位。
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

    // ── 选择生效的配置档 ──────────────────────────────────────
    // 优先用请求体里的 profile_index（前端当前选中档位），否则用磁盘的 active_index。
    let profile_index = payload
        .get("profile_index")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(cfg.active_index);
    let mut profile = select_profile(&cfg, profile_index)?;
    // DPAPI 密文 → 明文，供出站 HTTP 使用（解密失败显式报错，不静默空串）
    profile.api_key = prompt_forge_core::crypto::load_key(&profile.api_key)
        .map_err(|e| ApiError::bad("config", format!("API Key 解密失败: {e}")))?;

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

    // 角色：解析为 system prompt 前置
    // 严格解析：用户显式指定了角色但该角色不存在 → 显式报错，
    // 不静默回落到默认角色（否则用户以为用的是所选角色，实际注入的是另一个）。
    let role = roles::resolve_strict(&role_id, &cfg.custom_roles)
        .map_err(|e| ApiError::bad("config", e))?;
    let thinking_level = ThinkingLevel::parse(&thinking_raw);

    // 新一轮：废弃旧轮次，登记新令牌
    // generation 由 begin_enhance 权威分配（不得自行推算，否则并发下会错位）
    let (token, generation) = st.begin_enhance();

    // 可中断执行 + 整体墙钟上限
    //
    // 墙钟上限必须覆盖**最坏情况下的全部出站请求**：
    // 一次 attempt 最多发 2 个请求（首发 + 降级重试），attempt 最多 3 次
    // → 最坏 6 次出站，单靠出站 timeout 约束不住整体。
    //
    // 预算口径**下沉到 llm 层**（`llm::total_outbound_budget_secs`）——
    // llm 层内部用的就是同一个 deadline，两处各写一个魔数必然漂移。
    let wall_clock = std::time::Duration::from_secs(llm::total_outbound_budget_secs(&profile));
    let request = llm::EnhanceRequest::new(&prompt)
        .with_system(&role.system_prompt)
        .with_thinking(thinking_level);
    let result = tokio::select! {
        r = tokio::time::timeout(wall_clock, llm::enhance(&profile, &request)) => match r {
            Ok(inner) => inner,
            Err(_) => Err("timeout:增强请求超出总时长上限，已终止".to_string()),
        },
        _ = token.cancelled() => {
            if st.is_user_cancelled(generation) {
                Err("cancelled:已取消".to_string())
            } else {
                // 被新轮次取代：静默丢弃，不提示"已取消"
                Err("superseded:已被新的请求取代".to_string())
            }
        }
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
                // 服务端分配的权威轮次序号：前端据此判定响应是否属于当前轮
                "request_id": generation,
                "applied": {
                    "method_id": method_id,
                    "thinking": thinking_level.id(),
                    "role_id": role.id,
                    "role_name": role.name,
                }
            })))
        }
        // 错误也要带 request_id：前端必须能识别「这已经是上一轮的错误」
        Err(e) => Err(ApiError::from_legacy(e).with_request_id(generation)),
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
    Ok(Json(build_roles_payload(&cfg)))
}

/// 构造 `/api/roles` 的响应体（纯函数，便于单测）。
fn build_roles_payload(cfg: &AppConfig) -> Value {
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
    // 当前生效角色：配置里的 role_id 可能因自定义角色被删而失效，
    // 此时回落默认角色（这是"展示当前状态"，不是"执行用户选择"，回落是合理的）。
    // 但**必须把「发生了回落」显式标记出去** —— 否则前端无法区分
    // 「用户本来就想要默认角色」和「用户选的那个角色已经没了」。
    let (current, role_missing) = resolve_display_role(&cfg.enhance.role_id, &cfg.custom_roles);
    json!({
        "ok": true,
        "roles": list,
        "current": current,
        // 配置里用户原本选的 id（可能已失效）——前端据此提示「已回退到 X」
        "current_role_id": &cfg.enhance.role_id,
        // true = 原选角色已失效、已回落到默认角色
        "role_missing": role_missing,
    })
}

/// 展示态角色解析：允许回落，但把「发生了回落」显式标记出来。
///
/// 返回 `(生效角色, 是否发生了回落)`：
/// - 角色存在 → `(该角色, false)`
/// - 角色不存在（自定义角色被删 / id 写错）→ `(默认角色, true)`
///
/// **不把解析错误回传前端**：`resolve_strict` 的错误信息里含用户填的 role_id，
/// 可能是自定义内容，没必要也不安全原样回显 —— 前端只需要一个布尔标记。
fn resolve_display_role(
    role_id: &str,
    customs: &[roles::CustomRole],
) -> (roles::ResolvedRole, bool) {
    match roles::resolve_strict(role_id, customs) {
        Ok(r) => (r, false),
        Err(_) => (roles::default_role(), true),
    }
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
        Some(p) => {
            // 命中的接口后缀**属于哪个协议**？
            //
            // 前端 `parse_endpoint` 判定「是否命中接口路径」用的是**全协议合并表**
            // （core endpoint.rs 里 OPENAI/ANTHROPIC/OLLAMA 三表串联），
            // 所以**任何协议**的后缀都会被剥掉并回填 base_url。
            // 但 server 出站时 `resolve_endpoint` 只剥**所选协议**的后缀表
            // （core `suffixes_for_protocol`）—— 跨协议时前端会承诺
            // 「出站会自动去重」，实际却不去重，出站 URL 是畸形路径（404）。
            //
            // 这里用 core 的 `has_endpoint_suffix`（与 `strip_endpoint_suffix`
            // 严格同源：同一份后缀表、同样的大小写策略、同样只看 path 部分）
            // 逐协议判定，把「命中的是哪个协议」告诉前端 ——
            // 前端据此**只在协议匹配时**才承诺自动去重，
            // 不匹配时明确提示「识别到的接口路径属于 X 协议，与当前所选的 Y 不匹配」。
            //
            // server 侧不复制后缀表：判定全部委托 core。
            let matched_protocols: Vec<&str> = ["openai", "anthropic", "ollama", "gemini"]
                .into_iter()
                .filter(|proto| endpoint::has_endpoint_suffix(&raw, proto))
                .collect();

            Ok(Json(json!({
                "ok": true,
                "scheme": p.scheme,
                "host": p.host,
                "port": p.port,
                "path": p.path,
                "base_url": p.base_url,
                "notes": p.notes,
                "suggested_endpoint": endpoint::endpoint_for_protocol(&protocol),
                "normalized": endpoint::normalize_base_url(&p.base_url, &protocol),
                // 命中的接口路径所属协议（第一个命中）—— 单值便于前端直接比较
                "stripped_protocol": matched_protocols.first().copied(),
                // 全部命中的协议 —— 前端可据此判断是否与当前所选协议一致
                "stripped_protocols": matched_protocols,
            })))
        }
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
    // spawn_blocking：clip.exe 的 wait() 是阻塞调用，
    // 放在 tokio worker 线程上会占住整个 worker（本机只有几个 worker），
    // 导致其他请求（包括增强请求）被拖慢。
    tokio::task::spawn_blocking(move || copy_text(&text))
        .await
        .map_err(|e| ApiError::internal(format!("剪贴板任务异常: {e}")))?
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })))
}

/// 写入系统剪贴板（Windows 用 clip.exe，UTF-16LE 输入避免乱码）
///
/// 用 `SystemRoot` 拼出 clip.exe 的**绝对路径**，而不是依赖 PATH 查找：
/// 相对 PATH 查找会命中当前工作目录下的同名文件（CWD 劫持），
/// 且服务的工作目录随启动方式变化，行为不可预测。
fn copy_text(text: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let clip = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("clip.exe");

        let mut child = Command::new(&clip)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 clip.exe 失败 ({}): {e}", clip.display()))?;
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


#[cfg(test)]
mod tests {
    use super::*;
    use prompt_forge_core::roles::CustomRole;

    fn cfg_with_role(role_id: &str, customs: Vec<CustomRole>) -> AppConfig {
        AppConfig {
            enhance: prompt_forge_core::config::EnhanceOptions {
                role_id: role_id.to_string(),
                ..Default::default()
            },
            custom_roles: customs,
            ..Default::default()
        }
    }

    // ── 展示态角色解析（role_missing 标记）────────────────────

    /// 回归：配置里的角色已被删除时，必须显式标记「原角色失效、已回落」——
    /// 否则用户以为配好的角色还在用，实际注入的是另一个角色的 system prompt。
    #[test]
    fn roles_payload_flags_missing_role() {
        let payload = build_roles_payload(&cfg_with_role("custom-not-exist", Vec::new()));
        assert_eq!(payload["role_missing"], json!(true));
        assert_eq!(payload["current"]["id"], json!("builtin-general"));
        assert_eq!(payload["current_role_id"], json!("custom-not-exist"));
    }

    /// 正常态：角色存在 → role_missing 必须为 false。
    #[test]
    fn roles_payload_does_not_flag_existing_role() {
        let payload = build_roles_payload(&cfg_with_role("builtin-general", Vec::new()));
        assert_eq!(payload["role_missing"], json!(false));
        assert_eq!(payload["current"]["id"], json!("builtin-general"));
        assert_eq!(payload["current_role_id"], json!("builtin-general"));
    }

    /// 自定义角色仍在时不得误报 role_missing。
    #[test]
    fn roles_payload_accepts_existing_custom_role() {
        let custom = CustomRole {
            id: "mine".into(),
            name: "我的角色".into(),
            system_prompt: "你是我的助手".into(),
        };
        let payload = build_roles_payload(&cfg_with_role("custom-mine", vec![custom]));
        assert_eq!(payload["role_missing"], json!(false));
        assert_eq!(payload["current"]["id"], json!("custom-mine"));
    }

    /// 自定义角色被删除后（列表里没有了）必须标记失效。
    #[test]
    fn roles_payload_flags_deleted_custom_role() {
        let payload = build_roles_payload(&cfg_with_role("custom-deleted", Vec::new()));
        assert_eq!(payload["role_missing"], json!(true));
        assert_eq!(payload["current"]["id"], json!("builtin-general"));
    }

    /// 展示态解析：角色存在 → 不标记回落。
    #[test]
    fn display_role_resolves_without_fallback_flag() {
        let (r, missing) = resolve_display_role("builtin-general", &[]);
        assert!(!missing, "角色存在时不得标记回落");
        assert_eq!(r.id, "builtin-general");
    }

    /// 展示态解析：角色不存在 → 回落默认角色 **且必须标记 role_missing**。
    #[test]
    fn display_role_flags_fallback_when_role_missing() {
        let (r, missing) = resolve_display_role("custom-不存在", &[]);
        assert!(missing, "角色失效必须被标记");
        assert_eq!(r.id, roles::default_role().id);
    }

    // ── 配置档选择（N-1：Key 发往错误服务方）─────────────────

    fn llm_profile(name: &str, base_url: &str) -> LlmConfig {
        LlmConfig {
            name: name.into(),
            protocol: "openai".into(),
            base_url: base_url.into(),
            model: "m".into(),
            api_key: "sk-x".into(),
            temperature: 0.7,
            timeout_secs: 0,
            max_tokens: 0,
        }
    }

    /// 回归（N-1）：前端删掉一档并落盘后，同一个 `profile_index` 必须解析到
    /// **新的那一档**，绝不是已被删除的那一档。
    ///
    /// 事故形态：磁盘是 `[A(a.example), B(b.example)]`，前端删掉 A 并落盘后磁盘
    /// 变成 `[B]`；若请求仍按下标 0 解析到「内存里的 A」，就会把 **A 的 Key
    /// 发往 a.example** —— 用户以为在用 B，实际用的是已删除的 A。
    #[test]
    fn profile_index_resolves_against_current_disk_profiles() {
        // 磁盘当前只剩 B（前端已删掉 A 并落盘）
        let cfg = AppConfig {
            active_index: 0,
            profiles: vec![llm_profile("B", "https://b.example")],
            ..Default::default()
        };
        let p = match select_profile(&cfg, 0) {
            Ok(p) => p,
            Err(e) => panic!("下标 0 必须命中当前磁盘上的那一档: {}", e.message),
        };
        assert_eq!(
            p.base_url, "https://b.example",
            "下标 0 必须解析到当前磁盘上的那一档"
        );
        assert_eq!(p.name, "B");
    }

    /// 回归（N-1）：下标越界必须明确报错，**绝不允许回退到别的档**。
    ///
    /// 回退 = 把 Key 发往另一个主机 —— 与「用户以为在用 A、实际用了 B」同一类事故。
    #[test]
    fn profile_index_out_of_range_is_rejected_not_fallback() {
        let cfg = AppConfig {
            active_index: 0,
            profiles: vec![llm_profile("B", "https://b.example")],
            ..Default::default()
        };
        let err = match select_profile(&cfg, 1) {
            Ok(_) => panic!("越界下标必须报错，不得回退到别的档"),
            Err(e) => e,
        };
        assert_eq!(err.code, "no_profile");
        assert!(err.message.contains("索引 1"), "{}", err.message);
    }

    /// 一个配置档都没有 → 报「尚未配置」，不得当作越界索引。
    #[test]
    fn select_profile_on_empty_list_reports_not_configured() {
        let cfg = AppConfig::default();
        let err = match select_profile(&cfg, 0) {
            Ok(_) => panic!("空列表必须报错"),
            Err(e) => e,
        };
        assert_eq!(err.code, "no_profile");
        assert!(err.message.contains("尚未配置"), "{}", err.message);
    }

    // ── 写入侧接口地址闸门（#29：与 core 规则对齐）───────────

    /// 回归（#29）：含**空 base_url** 的配置档必须能被保存。
    ///
    /// core 的 `AppConfig::validate` 明确放行空地址（注释：未填写的空档位
    /// 不该在「保存整份配置」时被误判非法）。若写入侧比 core 严一档，
    /// 一个空档位就会把整份配置（自定义角色 / 模式 / autostart）一起锁死。
    #[test]
    fn write_side_gate_accepts_empty_base_url_profile() {
        let profiles = vec![
            llm_profile("配置 1", ""),
            llm_profile("配置 2", "https://b.example"),
        ];
        assert!(
            check_write_side_base_urls(&profiles).is_ok(),
            "空地址档位必须放行，不得把整份配置锁死"
        );
        // 纯空白地址与空地址同义
        assert!(check_write_side_base_urls(&[llm_profile("配置 1", "   ")]).is_ok());
    }

    /// 回归（#29）：放行空地址**不得**削弱对伪装地址的防护。
    ///
    /// `https://user:pass@evil.com` 写进配置 = 用户下次点「测试连接」
    /// 就把 API Key 发到 evil.com —— 这正是写入侧闸门存在的理由。
    #[test]
    fn write_side_gate_still_rejects_credential_bearing_url() {
        for bad in [
            "https://user:pass@evil.com",
            "https://api.openai.com@evil.com",
            "file:///etc/passwd",
        ] {
            let err = match check_write_side_base_urls(&[llm_profile("配置 1", bad)]) {
                Ok(()) => panic!("伪装地址必须被写入侧闸门拒绝: {bad}"),
                Err(e) => e,
            };
            assert_eq!(err.code, "config");
            assert!(
                err.message.contains("接口地址非法"),
                "错误文案必须点明是接口地址非法: {}",
                err.message
            );
        }
    }

    /// 写入侧的错误串必须点明「实际会连接的主机」。
    ///
    /// core 的文案只说「不得包含用户名/密码（user:pass@host）」——
    /// 那是**占位说明**，用户完全看不出真正会连的是 `evil.com`。
    /// 写入侧是用户填错地址的第一道反馈，恰恰最需要点明。
    #[test]
    fn write_side_gate_error_names_actual_host() {
        // 1) 纯 userinfo：点明 evil.com
        let err = match check_write_side_base_urls(&[llm_profile("配置 1", "https://user:pass@evil.com")]) {
            Ok(()) => panic!("userinfo 地址必须被写入侧闸门拒绝"),
            Err(e) => e,
        };
        assert_eq!(err.code, "config");
        assert!(
            err.message.contains("evil.com"),
            "错误串必须点明实际会连接的主机: {}",
            err.message
        );

        // 2) 伪装地址 `https://api.openai.com@evil.com/v1`：
        //    点明 evil.com，且**不得**出现被冒充的 api.openai.com
        let err = match check_write_side_base_urls(&[llm_profile(
            "配置 1",
            "https://api.openai.com@evil.com/v1",
        )]) {
            Ok(()) => panic!("伪装地址必须被写入侧闸门拒绝"),
            Err(e) => e,
        };
        assert!(
            err.message.contains("evil.com"),
            "错误串必须点明实际会连接的主机: {}",
            err.message
        );
        assert!(
            !err.message.contains("api.openai.com"),
            "错误串不得出现被冒充的主机名（会误导用户以为连的是 openai）: {}",
            err.message
        );
    }

    // ── 错误码 → HTTP 状态码映射 ──────────────────────────────

    /// 凭据类错误必须有专属状态码，不能全塞进 400。
    ///
    /// 回归：旧实现把所有已知码都映射成 400，
    /// 「上游 401」「被限流 429」「网关超时 504」与「配置写错 400」无法区分。
    #[test]
    fn from_legacy_maps_error_codes_to_meaningful_status() {
        let cases: &[(&str, StatusCode)] = &[
            ("unauthorized", StatusCode::UNAUTHORIZED),
            ("forbidden", StatusCode::FORBIDDEN),
            ("rate_limited", StatusCode::TOO_MANY_REQUESTS),
            ("timeout", StatusCode::GATEWAY_TIMEOUT),
            ("server", StatusCode::BAD_GATEWAY),
            ("network", StatusCode::BAD_GATEWAY),
            ("http", StatusCode::BAD_GATEWAY),
            ("config", StatusCode::BAD_REQUEST),
            ("no_profile", StatusCode::BAD_REQUEST),
            ("empty_result", StatusCode::BAD_REQUEST),
            ("cancelled", StatusCode::BAD_REQUEST),
            ("superseded", StatusCode::BAD_REQUEST),
        ];
        for (code, want) in cases {
            let e = ApiError::from_legacy(format!("{code}:详情"));
            assert_eq!(e.status, *want, "错误码 {code} 的状态码映射不对");
            assert_eq!(e.code, *code, "错误码本身必须原样保留");
            assert_eq!(e.message, "详情", "message 应为冒号后的部分");
        }
    }

    /// 未知错误码 → 500（内部错误），不得当成业务错误。
    #[test]
    fn from_legacy_unknown_code_is_internal_error() {
        let e = ApiError::from_legacy("weird:某错误".into());
        assert_eq!(e.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(e.code, "weird");
    }

    /// 无冒号的错误串 → code 退化为 `error`，message 为整串。
    #[test]
    fn from_legacy_without_colon_falls_back_to_error_code() {
        let e = ApiError::from_legacy("纯文本错误".into());
        assert_eq!(e.code, "error");
        assert_eq!(e.message, "纯文本错误");
        assert_eq!(e.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // ── request_id 回传 ───────────────────────────────────────

    /// 错误体必须带 request_id —— 前端据此判定「这已经是上一轮的错误」。
    #[tokio::test]
    async fn error_response_body_carries_request_id() {
        let e = ApiError::bad("config", "配置有问题").with_request_id(42);
        let resp = e.into_response();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("应能读取响应体");
        let v: Value = serde_json::from_slice(&body).expect("响应体应为合法 JSON");
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["code"], json!("config"));
        assert_eq!(v["message"], json!("配置有问题"));
        assert_eq!(v["error"], json!("config:配置有问题"));
        assert_eq!(v["request_id"], json!(42));
    }

    /// 未设置 request_id 时不应出现该字段（而不是 null）。
    #[tokio::test]
    async fn error_response_omits_request_id_when_absent() {
        let resp = ApiError::bad("config", "无轮次").into_response();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("应能读取响应体");
        let v: Value = serde_json::from_slice(&body).expect("响应体应为合法 JSON");
        assert!(
            v.get("request_id").is_none(),
            "未设置 request_id 时不应出现该字段: {v}"
        );
    }
}
