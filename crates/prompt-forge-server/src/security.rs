//! 请求来源守卫：Origin / Host 校验。
//!
//! ## 为什么需要
//!
//! 服务只监听 `127.0.0.1`，但**"只监听回环"不等于"只有本机页面能访问"**：
//!
//! 1. **DNS rebinding**：攻击者控制 `evil.com`，把它的 A 记录指向 `127.0.0.1`。
//!    浏览器访问 `http://evil.com:8787/` 时，TCP 连到了本机服务，且请求头
//!    `Host: evil.com` —— 服务若不校验 Host 就会像正常页面一样响应，
//!    攻击者的 JS 就能读走配置（含 API Key）。
//!
//! 2. **CSRF**：任意第三方页面都能对 `http://127.0.0.1:8787/api/enhance` 发
//!    带 cookie / 简单请求头的 POST。服务侧若不校验 Origin，攻击者页面可
//!    借用户浏览器触发出站请求（把用户 Key 发到攻击者服务器）。
//!
//! ## 策略
//!
//! - **Host**：必须落在「本机 + 当前端口」白名单内（`127.0.0.1:port`、
//!   `localhost:port`、`[::1]:port`），否则拒绝。DNS rebinding 场景下
//!   Host 是 `evil.com`，直接被挡。
//! - **Origin**：仅对**有副作用**的请求（POST/PUT/PATCH/DELETE）校验。
//!   浏览器一定带 Origin；非浏览器客户端（curl / 脚本）不带 Origin，
//!   放行（本地工具链正常调用）。Origin 存在时必须指向本机同端口。
//!
//! ## 兼容性
//!
//! 本地 Web UI 通过 `fetch` 访问同源，`Origin` 会是 `http://127.0.0.1:port`
//! 或 `http://localhost:port` —— 均在白名单内，不受影响。

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// 允许的 Host 值（小写，不含端口）
#[derive(Debug, Clone)]
pub struct HostGuard {
    /// 允许的 host（不含端口）
    hosts: Vec<String>,
    /// 当前服务端口
    port: u16,
}

impl HostGuard {
    pub fn new(port: u16) -> Self {
        Self {
            hosts: vec!["127.0.0.1".to_string(), "localhost".to_string(), "[::1]".to_string()],
            port,
        }
    }

    /// 当前服务实际地址（用于构造本机 URL）
    ///
    /// 仅用于测试与诊断，不参与放行判定 —— 判定一律以 `host_allowed` 为准。
    #[cfg(test)]
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn host_allowed(&self, host: &str) -> bool {
        // Host 形如 "127.0.0.1:8787" / "localhost" / "[::1]:8787"
        let host = host.trim().to_ascii_lowercase();
        // 拆端口
        let (name, port_ok) = if let Some(stripped) = host.strip_prefix('[') {
            match stripped.split_once(']') {
                Some((h, tail)) => {
                    let port = tail.strip_prefix(':').and_then(|p| p.parse::<u16>().ok());
                    (format!("[{h}]"), port.map(|p| p == self.port).unwrap_or(true))
                }
                None => (host.clone(), false),
            }
        } else {
            match host.rsplit_once(':') {
                Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
                    (h.to_string(), p.parse::<u16>().map(|p| p == self.port).unwrap_or(false))
                }
                _ => (host.clone(), true),
            }
        };
        port_ok && self.hosts.contains(&name)
    }

    /// Origin 是否指向本机同端口
    fn origin_allowed(&self, origin: &str) -> bool {
        let o = origin.trim().to_ascii_lowercase();
        let rest = match o.split_once("://") {
            Some((s, r)) => {
                if !matches!(s, "http" | "https") {
                    return false;
                }
                r
            }
            None => return false,
        };
        // 去掉 path 部分
        let authority = rest.split('/').next().unwrap_or("");
        self.host_allowed(authority)
    }
}

/// 拒绝响应（统一 JSON 包络，与 API 层错误格式一致）
fn reject(code: &str, message: &str) -> Response {
    let body = serde_json::json!({
        "ok": false,
        "error": format!("{code}:{message}"),
        "code": code,
        "message": message,
    });
    (StatusCode::FORBIDDEN, axum::Json(body)).into_response()
}

/// 中间件本体
pub async fn guard(
    State(guard): State<HostGuard>,
    req: Request,
    next: Next,
) -> Response {
    // ── Host 校验（所有请求）──
    let host_ok = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| guard.host_allowed(h))
        .unwrap_or(false);
    if !host_ok {
        return reject("forbidden", "Host 不被允许（拒绝 DNS rebinding）");
    }

    // ── Origin 校验（所有带 Origin 头的请求）──
    //
    // 为什么 GET 也要校验：`GET /api/config` 会回传**解密后的 API Key 明文**
    // （store.rs 的 load_config_for_client）。若只在有副作用的方法上校验 Origin，
    // 跨源页面就能对读接口发起带 Origin 的 GET —— 服务照常返回明文 Key。
    //
    // 为什么不能只靠「服务端不返 CORS 头」防住：浏览器同源策略确实会拦掉
    // 跨源读取响应体，但那是**客户端**的约束；服务端不校验 = 一旦将来误加
    // CORS 层（或有人用非浏览器客户端补 Origin 头），明文 Key 立刻外泄。
    // 校验放在服务端才是真防线。
    //
    // 无 Origin 头 = 非浏览器客户端（curl / 本地脚本）→ 放行（本地工具链正常调用）。
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let allowed = origin
            .to_str()
            .map(|o| guard.origin_allowed(o))
            .unwrap_or(false);
        if !allowed {
            return reject("forbidden", "跨源请求被拒绝（Origin 不匹配）");
        }
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard_for_test(port: u16) -> HostGuard {
        HostGuard::new(port)
    }

    #[test]
    fn host_allowlist_accepts_loopback() {
        let g = guard_for_test(8787);
        assert!(g.host_allowed("127.0.0.1:8787"));
        assert!(g.host_allowed("localhost:8787"));
        assert!(g.host_allowed("LOCALHOST:8787"));
        assert!(g.host_allowed("[::1]:8787"));
        // 无端口（HTTP/1.0 形态）
        assert!(g.host_allowed("127.0.0.1"));
    }

    #[test]
    fn host_allowlist_rejects_rebinding_and_wrong_port() {
        let g = guard_for_test(8787);
        // DNS rebinding 场景
        assert!(!g.host_allowed("evil.com"));
        assert!(!g.host_allowed("evil.com:8787"));
        assert!(!g.host_allowed("localhost.evil.com:8787"));
        // 端口不匹配
        assert!(!g.host_allowed("127.0.0.1:9999"));
        assert!(!g.host_allowed("127.0.0.1:8788"));
    }

    #[test]
    fn origin_allowlist() {
        let g = guard_for_test(8787);
        // 当前服务地址用于构造本机 URL（仅测试/诊断用）
        assert_eq!(g.origin(), "http://127.0.0.1:8787");
        assert!(g.origin_allowed("http://127.0.0.1:8787"));
        assert!(g.origin_allowed("http://localhost:8787"));
        assert!(g.origin_allowed("http://127.0.0.1:8787/"));
        // 协议不参与判定（host:port 一致即放行）——本机不会有 https 服务，
        // 但放行无害，且能兼容用户用 https 访问的反代场景
        assert!(g.origin_allowed("https://127.0.0.1:8787"));
        // 跨源必须拒绝
        assert!(!g.origin_allowed("http://evil.com"));
        assert!(!g.origin_allowed("http://evil.com:8787"));
        assert!(!g.origin_allowed("http://localhost.evil.com:8787"));
        // 端口不匹配
        assert!(!g.origin_allowed("http://127.0.0.1:9999"));
        // 非 http(s) 协议
        assert!(!g.origin_allowed("file://127.0.0.1:8787"));
    }
}
