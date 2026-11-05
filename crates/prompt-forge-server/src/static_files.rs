//! 静态资源托管：从内嵌资源表（build.rs 生成）提供前端文件。
//!
//! 采用内嵌而非读磁盘，好处是发行产物为**单一 exe**，不依赖外部目录，复制即用。
//! 同时保留 `--web-dir` 参数用于开发期热更（改前端文件刷新浏览器即生效）。

use axum::body::Body;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

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

    build_response(
        StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        b"404 Not Found",
    )
}

/// 从磁盘目录读取（开发模式）
fn serve_from_disk(dir: &std::path::Path, path: &str) -> Option<Response> {
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    // 防目录穿越
    if rel.contains("..") {
        return None;
    }
    let file = dir.join(rel);
    let bytes = std::fs::read(&file).ok()?;
    let mime = mime_for(&file);
    Some(build_response(StatusCode::OK, mime, &bytes))
}

/// 构造响应（带内容类型与长度头）
fn build_response(status: StatusCode, mime: &str, bytes: &[u8]) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, bytes.len())
        // 本地开发禁用缓存，避免改前端不生效
        .header(header::CACHE_CONTROL, "no-store")
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

/// Content-Type 映射（与 build.rs 保持一致）
fn mime_for(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
