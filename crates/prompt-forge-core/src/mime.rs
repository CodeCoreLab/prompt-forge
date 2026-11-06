//! Content-Type 映射（单一真源）
//!
//! 背景：原先 `prompt-forge-server/build.rs`（编译期生成内嵌资源表）与
//! `prompt-forge-server/src/static_files.rs`（运行期读磁盘）各写了一份
//! **完全相同**的 `mime_for`，改一处忘另一处就会出现「某个文件类型突然返回
//! `application/octet-stream`」的诡异故障。
//!
//! 本模块是唯一实现：`build.rs` 通过 `[build-dependencies]` 引用 core crate，
//! 运行期 `static_files.rs` 通过普通依赖引用，两边调用同一份代码。
//!
//! 纯函数、无 I/O、无平台依赖，可在任意平台独立测试。

use std::path::Path;

/// 按文件路径推断 Content-Type
///
/// 取路径扩展名（转小写）后交给 [`mime_for_extension`]。
/// 无扩展名或扩展名非 UTF-8 时返回 `application/octet-stream`。
pub fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    mime_for_extension(&ext)
}

/// 按扩展名（不含点）推断 Content-Type
///
/// **调用方无需先转小写**：本函数内部自己做 ASCII 小写化，
/// 因此 `"HTML"` 与 `"html"` 结果一致。
pub fn mime_for_extension(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_html_extension() {
        assert_eq!(
            mime_for_path(Path::new("a/b/c.HTML")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mime_for_path(Path::new("a/b/c.html")),
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn path_without_extension_is_octet_stream() {
        assert_eq!(
            mime_for_path(Path::new("a/b/c")),
            "application/octet-stream"
        );
    }

    #[test]
    fn extension_is_case_insensitive() {
        assert_eq!(mime_for_extension("HTML"), mime_for_extension("html"));
        assert_eq!(mime_for_extension("HTML"), "text/html; charset=utf-8");
        assert_eq!(mime_for_extension("JS"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for_extension("PNG"), "image/png");
    }

    #[test]
    fn empty_extension_is_octet_stream() {
        assert_eq!(mime_for_extension(""), "application/octet-stream");
    }

    #[test]
    fn unknown_extension_is_octet_stream() {
        assert_eq!(
            mime_for_extension("unknownext"),
            "application/octet-stream"
        );
        assert_eq!(
            mime_for_extension("unknownext"),
            mime_for_extension("UNKNOWNEXT")
        );
    }

    #[test]
    fn key_extension_mappings() {
        assert_eq!(
            mime_for_extension("js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_for_extension("mjs"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for_extension("css"), "text/css; charset=utf-8");
        assert_eq!(mime_for_extension("json"), "application/json; charset=utf-8");
        assert_eq!(mime_for_extension("svg"), "image/svg+xml");
        assert_eq!(mime_for_extension("png"), "image/png");
        assert_eq!(mime_for_extension("jpg"), "image/jpeg");
        assert_eq!(mime_for_extension("jpeg"), "image/jpeg");
        assert_eq!(mime_for_extension("gif"), "image/gif");
        assert_eq!(mime_for_extension("ico"), "image/x-icon");
        assert_eq!(mime_for_extension("woff2"), "font/woff2");
        assert_eq!(mime_for_extension("woff"), "font/woff");
        assert_eq!(mime_for_extension("ttf"), "font/ttf");
        assert_eq!(mime_for_extension("txt"), "text/plain; charset=utf-8");
        assert_eq!(mime_for_extension("md"), "text/plain; charset=utf-8");
    }
}
