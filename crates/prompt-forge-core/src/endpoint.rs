//! 端点解析：把用户随手粘贴的任意 URL 拆解为协议/主机/端口/路径。
//!
//! 需求背景：用户粘贴的往往是**完整接口 URL**（如
//! `https://note3-prev-api.askdiandian.com/v1/chat/completions`），
//! 而配置项要求的是**基础地址**（base_url）。手工删后缀易错。
//!
//! 本模块提供纯函数解析，不依赖任何 HTTP 客户端，可独立测试：
//! - `parse_endpoint` 拆解 URL → 结构化字段
//! - `normalize_base_url` 从完整接口 URL 反推基础地址
//! - `endpoint_for_protocol` 按协议给出建议的接口路径
//!
//! 设计原则：**解析失败不静默吞掉**，返回结构里的 `issues` 供 UI 提示。

/// 已知各协议在 OpenAI 兼容语系下的常见接口后缀（用于反推 base_url）
const OPENAI_SUFFIXES: &[&str] = &[
    "/chat/completions",
    "/completions",
    "/embeddings",
    "/responses",
    "/models",
];

/// Anthropic / Ollama / Gemini 的接口后缀
const ANTHROPIC_SUFFIXES: &[&str] = &["/v1/messages", "/messages"];
const OLLAMA_SUFFIXES: &[&str] = &["/api/generate", "/api/chat", "/api/show"];
const GEMINI_SUFFIXES: &[&str] = &[":generatecontent", ":streamgeneratecontent"];

/// 端点解析结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEndpoint {
    /// 协议：https / http（缺省补 https，本地地址补 http）
    pub scheme: String,
    /// 主机名（不含端口）
    pub host: String,
    /// 端口（URL 未显式给出则为 None）
    pub port: Option<u16>,
    /// 路径（不含 query），始终以 / 开头；根路径为 "/"
    pub path: String,
    /// 从完整接口 URL 反推出的基础地址（去掉接口后缀）
    pub base_url: String,
    /// 解析过程中的提示（补全了什么、剥离了什么）
    pub notes: Vec<String>,
}

/// 解析用户输入的端点字符串。
/// - 缺 scheme → 依据是否本地地址补 http/https
/// - 含完整接口路径 → 记录剥离了哪些后缀，并回填 base_url
pub fn parse_endpoint(raw: &str) -> Option<ParsedEndpoint> {
    let input = raw.trim();
    if input.is_empty() {
        return None;
    }

    let mut notes: Vec<String> = Vec::new();

    // 1) 拆 scheme
    let (scheme, rest) = match input.split_once("://") {
        Some((s, r)) => {
            let s = s.to_ascii_lowercase();
            if !matches!(s.as_str(), "http" | "https") {
                notes.push(format!("协议 {s} 已按 https 处理"));
                ("https".to_string(), r.to_string())
            } else {
                (s, r.to_string())
            }
        }
        None => {
            // 无 scheme：本地地址用 http，其余用 https
            let host_part = input.split('/').next().unwrap_or("");
            let is_local = host_part.starts_with("localhost")
                || host_part.starts_with("127.")
                || host_part.starts_with("192.168.")
                || host_part.starts_with("10.")
                || host_part.starts_with("172.16.");
            let s = if is_local { "http" } else { "https" };
            notes.push(format!("已自动补全协议 {s}://"));
            (s.to_string(), input.to_string())
        }
    };

    // 2) 拆 host[:port] 与 path
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (&rest[..], "/"),
    };
    // 去掉 query / fragment
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or("/")
        .to_string();

    if hostport.is_empty() {
        return None;
    }

    // 3) 拆 host 与 port（兼容 IPv6 字面量 [::1]:8080）
    let (host, port) = if let Some(stripped) = hostport.strip_prefix('[') {
        // IPv6: [::1]:8080
        match stripped.split_once(']') {
            Some((h, tail)) => {
                let p = tail
                    .strip_prefix(':')
                    .and_then(|s| s.parse::<u16>().ok());
                (format!("[{h}]"), p)
            }
            None => (format!("[{stripped}]"), None),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
                (h.to_string(), p.parse::<u16>().ok())
            }
            _ => (hostport.to_string(), None),
        }
    };

    if host.is_empty() {
        return None;
    }

    // 4) 反推 base_url：剥离已知接口后缀
    let lower = path.to_ascii_lowercase();
    let mut base_path = path.clone();
    let mut stripped: Option<&str> = None;
    for suf in OPENAI_SUFFIXES
        .iter()
        .chain(ANTHROPIC_SUFFIXES)
        .chain(OLLAMA_SUFFIXES)
    {
        if lower.ends_with(suf) {
            base_path = path[..path.len() - suf.len()].to_string();
            stripped = Some(suf);
            break;
        }
    }
    for suf in GEMINI_SUFFIXES {
        if lower.ends_with(suf) {
            // Gemini 形如 /v1beta/models/gemini-2.0-pro:generateContent
            base_path = path[..path.len() - suf.len()].to_string();
            stripped = Some(suf);
            break;
        }
    }
    if let Some(s) = stripped {
        notes.push(format!("已识别接口路径 {s}，基础地址回填为不含接口的根"));
    }

    let base_path = if base_path.is_empty() {
        String::new()
    } else {
        base_path
    };
    let base_url = format!("{scheme}://{host}{}", port.map(|p| format!(":{p}")).unwrap_or_default());
    let base_url = format!("{base_url}{base_path}");

    Some(ParsedEndpoint {
        scheme,
        host,
        port,
        path,
        base_url,
        notes,
    })
}

/// 归一化基础地址：去尾斜杠、补 /v1（OpenAI 兼容系默认需要）。
/// 仅在 base_url 未显式带版本段时补 /v1。
pub fn normalize_base_url(raw: &str, protocol: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    if s.is_empty() {
        return String::new();
    }
    let needs_v1 = matches!(protocol, "openai" | "ollama" | "gemini");
    if needs_v1 && !s.contains("/v1") && !s.contains("/v1beta") && !s.contains("/compatible-mode") {
        format!("{s}/v1")
    } else {
        s.to_string()
    }
}

/// 按协议给出建议的接口路径（UI 提示用）
pub fn endpoint_for_protocol(protocol: &str) -> &'static str {
    match protocol {
        "openai" => "/v1/chat/completions",
        "anthropic" => "/v1/messages",
        "ollama" => "/api/chat",
        "gemini" => "/v1beta/models/{model}:generateContent",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_openai_url() {
        let p = parse_endpoint("https://note3-prev-api.askdiandian.com/v1/chat/completions").unwrap();
        assert_eq!(p.scheme, "https");
        assert_eq!(p.host, "note3-prev-api.askdiandian.com");
        assert_eq!(p.port, None);
        assert_eq!(p.path, "/v1/chat/completions");
        assert_eq!(p.base_url, "https://note3-prev-api.askdiandian.com/v1");
        assert!(p.notes.iter().any(|n| n.contains("chat/completions")));
    }

    #[test]
    fn parse_with_port() {
        let p = parse_endpoint("http://127.0.0.1:11434/api/chat").unwrap();
        assert_eq!(p.scheme, "http");
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.port, Some(11434));
        assert_eq!(p.base_url, "http://127.0.0.1:11434");
    }

    #[test]
    fn parse_local_gets_http_scheme() {
        let p = parse_endpoint("localhost:11434").unwrap();
        assert_eq!(p.scheme, "http");
        assert_eq!(p.host, "localhost");
        assert_eq!(p.port, Some(11434));
        assert!(p.notes.iter().any(|n| n.contains("http://")));
    }

    #[test]
    fn parse_remote_gets_https_scheme() {
        let p = parse_endpoint("api.deepseek.com").unwrap();
        assert_eq!(p.scheme, "https");
        assert_eq!(p.host, "api.deepseek.com");
    }

    #[test]
    fn parse_strips_query() {
        let p = parse_endpoint("https://x.com/v1/chat/completions?a=1#f").unwrap();
        assert_eq!(p.path, "/v1/chat/completions");
        assert_eq!(p.base_url, "https://x.com/v1");
    }

    #[test]
    fn parse_gemini_path() {
        let p = parse_endpoint("https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent")
            .unwrap();
        assert_eq!(p.base_url, "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro");
    }

    #[test]
    fn parse_empty_returns_none() {
        assert!(parse_endpoint("").is_none());
        assert!(parse_endpoint("   ").is_none());
        assert!(parse_endpoint("https://").is_none());
    }

    #[test]
    fn parse_ipv6() {
        let p = parse_endpoint("http://[::1]:8080/v1/chat/completions").unwrap();
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.port, Some(8080));
    }

    #[test]
    fn normalize_adds_v1_for_openai() {
        assert_eq!(
            normalize_base_url("https://api.deepseek.com", "openai"),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1", "openai"),
            "https://api.deepseek.com/v1"
        );
        // Anthropic 不补 /v1（其接口自带 /v1/messages）
        assert_eq!(
            normalize_base_url("https://api.anthropic.com", "anthropic"),
            "https://api.anthropic.com"
        );
    }
}
