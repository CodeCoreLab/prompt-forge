//! 静态资源托管：从内嵌资源表（build.rs 生成）提供前端文件。
//!
//! 采用内嵌而非读磁盘，好处是发行产物为**单一 exe**，不依赖外部目录，复制即用。
//! 同时保留 `--web-dir` 参数用于开发期热更（改前端文件刷新浏览器即生效）。

use axum::body::Body;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use prompt_forge_core::mime::mime_for_path;
use serde_json::json;

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// 静态资源响应
pub fn serve(uri: &Uri, web_dir: Option<&std::path::Path>) -> Response {
    let path = uri.path();

    // 开发模式：优先读磁盘（支持热更）
    if let Some(dir) = web_dir {
        if let Some(resp) = serve_from_disk(dir, path) {
            return resp;
        }
    }

    // 生产模式：内嵌资源表
    let key = if path == "/" { "/index.html" } else { path };
    if let Some((_, bytes, mime)) = ASSETS.iter().find(|(url, _, _)| *url == key) {
        return build_response(StatusCode::OK, mime, bytes);
    }

    // SPA 兜底：未知路径回 index.html（非 /api 前缀）
    if !path.starts_with("/api/") {
        if let Some((_, bytes, mime)) = ASSETS.iter().find(|(url, _, _)| *url == "/index.html") {
            return build_response(StatusCode::OK, mime, bytes);
        }
    }

    // `/api/` 前缀未命中：返回 JSON 包络 404，与 API 层错误形态保持一致
    // （前端 `api()` 统一按 JSON 解析错误体，裸文本会抛 JSON 解析异常，
    //   用户看到的是「Unexpected token」而不是「接口不存在」）。
    if path.starts_with("/api/") {
        return build_json_not_found(path);
    }

    build_response(
        StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        b"404 Not Found",
    )
}

/// `/api/` 未命中路径的 JSON 包络 404
///
/// 形态与 `api.rs` / `security.rs` 的错误响应一致：
/// `{"ok":false,"error":"CODE:message","code":"...","message":"..."}`
/// —— 用 `serde_json::json!` 构造，路径中的引号/控制字符会被安全转义。
fn build_json_not_found(path: &str) -> Response {
    let message = format!("接口不存在: {path}");
    let body = json!({
        "ok": false,
        "error": format!("http:{message}"),
        "code": "http",
        "message": message,
    });
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| {
        b"{\"ok\":false,\"error\":\"http:interface not found\",\"code\":\"http\",\"message\":\"interface not found\"}".to_vec()
    });
    build_response(
        StatusCode::NOT_FOUND,
        "application/json; charset=utf-8",
        &bytes,
    )
}

/// 从磁盘目录读取（开发模式）
///
/// 防目录穿越：**不能只靠 `contains("..")`**——`..` 的判断必须是"路径段"级别，
/// 且最终必须校验规范化后的路径仍在 `dir` 之内（symlink / `..%2f` / 绝对路径都要挡住）。
fn serve_from_disk(dir: &std::path::Path, path: &str) -> Option<Response> {
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    // 路径段级别的穿越检查：`..` 作为独立路径段出现即拒绝
    if rel.split('/').any(|seg| seg == ".." || seg == ".") {
        return None;
    }
    // 绝对路径注入（`/etc/passwd` 已被上面的 trim 掉前导 /，但 Windows 盘符仍需挡）
    if rel.contains('\\') || rel.contains(':') {
        return None;
    }
    let file = dir.join(rel);
    // 最终防线：规范化后必须仍在 dir 之内
    let base = dir.canonicalize().ok()?;
    let resolved = file.canonicalize().ok()?;
    if !resolved.starts_with(&base) {
        return None;
    }
    let bytes = std::fs::read(&resolved).ok()?;
    let mime = mime_for_path(&resolved);
    Some(build_response(StatusCode::OK, mime, &bytes))
}

/// 构造响应（带内容类型与长度头 + 防御性响应头）
///
/// 防御性响应头不是「有更好」的可选项，而是**修掉一类真实攻击面**：
/// - `Content-Security-Policy`：默认-src 'self' + script-src 'self' →
///   即使有 XSS 注入点，也**无法执行内联脚本或加载外部脚本**
///   （内联 handler 的 onclick 会被这条策略直接挡掉）。
/// - `X-Content-Type-Options: nosniff`：阻止浏览器「猜测」响应类型，
///   避免用户上传/反射的内容被当成脚本执行。
/// - `Referrer-Policy: no-referrer`：本机页面若跳转外链，不泄露
///   `http://127.0.0.1:8787` 这类地址与可能的路径参数。
/// - `X-Frame-Options: DENY`：禁止被 iframe 嵌套（点击劫持）。
///
/// CSP 里**不加** `unsafe-inline`：前端是纯静态文件，无内联脚本与内联样式需求，
/// 加了等于把刚补上的防线拆掉。
fn build_response(status: StatusCode, mime: &str, bytes: &[u8]) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, bytes.len())
        // 缓存策略：
        // - 开发模式（磁盘）：no-store，改前端刷新即生效
        // - 生产模式（内嵌）：内嵌资源在编译期固定，版本随 exe 走 →
        //   `no-cache`（每次校验 ETag）即可，不引入版本号 query 串的复杂度。
        .header(header::CACHE_CONTROL, "no-cache")
        .header(
            "Content-Security-Policy",
            // 'unsafe-inline' 缺席：内联脚本/事件处理器一律不执行
            // script-src-attr 'none'：HTML 属性上的事件处理器（onclick=...）一并禁止
            "default-src 'self'; script-src 'self'; script-src-attr 'none'; style-src 'self'; \
             style-src-attr 'none'; img-src 'self' data:; connect-src 'self'; font-src 'self'; \
             frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'",
        )
        .header("X-Content-Type-Options", "nosniff")
        .header("Referrer-Policy", "no-referrer")
        .header("X-Frame-Options", "DENY")
        .body(Body::from(bytes.to_vec()))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain")],
                "响应构造失败",
            )
                .into_response()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(p: &str) -> Uri {
        p.parse().unwrap()
    }

    /// `/api/` 前缀未命中 → JSON 包络 404
    #[tokio::test]
    async fn api_miss_returns_json_envelope() {
        let resp = serve(&uri("/api/does-not-exist"), None);
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json; charset=utf-8")
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["code"], serde_json::json!("http"));
        assert_eq!(
            v["message"],
            serde_json::json!("接口不存在: /api/does-not-exist")
        );
        assert_eq!(
            v["error"],
            serde_json::json!("http:接口不存在: /api/does-not-exist")
        );
    }

    /// 路径含引号/控制字符时，JSON 仍可解析（安全转义）
    #[tokio::test]
    async fn api_miss_path_with_quotes_is_escaped() {
        let resp = build_json_not_found("/api/a\"b\nc");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["code"], serde_json::json!("http"));
        assert_eq!(
            v["message"],
            serde_json::json!("接口不存在: /api/a\"b\nc")
        );
    }

    /// 非 `/api/` 前缀未命中 → SPA 兜底回 index.html（200）
    #[test]
    fn non_api_miss_falls_back_to_index() {
        let resp = serve(&uri("/some/spa/route"), None);
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ── 防御性响应头 ──────────────────────────────────────────

    /// CSP 必须存在且**不含 unsafe-inline**。
    ///
    /// 含 unsafe-inline 等于把「禁止内联脚本执行」这条防线整个拆掉 ——
    /// XSS 注入点仍可直接执行任意脚本，CSP 形同虚设。
    #[test]
    fn response_carries_strict_csp_without_unsafe_inline() {
        let resp = serve(&uri("/"), None);
        let csp = resp
            .headers()
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok())
            .expect("必须有 Content-Security-Policy 头");
        assert!(
            csp.contains("default-src 'self'"),
            "CSP 应限制 default-src 为 self: {csp}"
        );
        assert!(
            csp.contains("script-src 'self'"),
            "CSP 应限制 script-src 为 self: {csp}"
        );
        assert!(
            !csp.contains("unsafe-inline"),
            "CSP 绝不能含 unsafe-inline（否则防线失效）: {csp}"
        );
        assert!(
            !csp.contains("unsafe-eval"),
            "CSP 绝不能含 unsafe-eval: {csp}"
        );
        assert!(
            csp.contains("frame-ancestors 'none'"),
            "CSP 应禁止被嵌套: {csp}"
        );
        assert!(
            csp.contains("script-src-attr 'none'"),
            "CSP 应禁止 HTML 属性上的事件处理器: {csp}"
        );
        assert!(
            csp.contains("base-uri 'none'") && csp.contains("form-action 'self'"),
            "CSP 应限制 base-uri 与 form-action: {csp}"
        );
        assert!(
            csp.contains("object-src 'none'"),
            "CSP 应禁用 object 元素: {csp}"
        );
    }

    /// nosniff / no-referrer / DENY 三条头必须齐全。
    #[test]
    fn response_carries_defensive_headers() {
        let resp = serve(&uri("/"), None);
        let h = resp.headers();
        assert_eq!(
            h.get("x-content-type-options")
                .and_then(|v| v.to_str().ok()),
            Some("nosniff"),
            "必须有 X-Content-Type-Options: nosniff"
        );
        assert_eq!(
            h.get("referrer-policy").and_then(|v| v.to_str().ok()),
            Some("no-referrer"),
            "必须有 Referrer-Policy: no-referrer"
        );
        assert_eq!(
            h.get("x-frame-options").and_then(|v| v.to_str().ok()),
            Some("DENY"),
            "必须有 X-Frame-Options: DENY"
        );
    }

    /// JSON 包络 404 同样要带防御性头（错误响应也不该缺）。
    #[test]
    fn json_not_found_carries_defensive_headers() {
        let resp = build_json_not_found("/api/x");
        assert!(resp.headers().contains_key("content-security-policy"));
        assert!(resp.headers().contains_key("x-content-type-options"));
    }
}
