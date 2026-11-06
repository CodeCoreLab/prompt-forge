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
//! - `strip_endpoint_suffix` 剥掉 base_url 尾部**已存在的**接口后缀
//!   （出站拼接的逆运算，防止 `…/api/generate/api/generate` → 404）
//! - `has_endpoint_suffix` 判断 base_url 是否已是完整接口路径
//! - `resolve_endpoint` 把 base_url（可能是完整端点 URL）解析成**最终请求 URL**
//!
//! 设计原则：**解析失败不静默吞掉**，返回结构里的 `issues` 供 UI 提示。

/// 已知各协议在 OpenAI 兼容语系下的常见接口后缀（用于反推 base_url）
///
/// 这些常量同时是**出站拼接的逆运算依据** —— 见 [`strip_endpoint_suffix`]：
/// 用户手填的 base_url 可能已经带上接口后缀，出站时若再无条件拼一次
/// 就会变成 `…/api/generate/api/generate` → 404。
pub const OPENAI_SUFFIXES: &[&str] = &[
    "/chat/completions",
    "/completions",
    "/embeddings",
    "/responses",
    "/models",
];

/// Anthropic / Ollama / Gemini 的接口后缀
pub const ANTHROPIC_SUFFIXES: &[&str] = &["/v1/messages", "/messages"];
pub const OLLAMA_SUFFIXES: &[&str] = &["/api/generate", "/api/chat", "/api/show"];

/// Gemini 的接口后缀（含冒号的**方法名**，不是路径段）
///
/// Gemini 实际调用形态是 `/v1beta/models/{model}:generateContent`，
/// 方法名是**驼峰** `:generateContent`；早期版本误写成全小写
/// `:generatecontent` —— 两种形态都列进来，且匹配时**大小写不敏感**
/// （见私有辅助函数 `suffix_matches`），避免因大小写差异剥不掉后缀 → 出站 404。
pub const GEMINI_SUFFIXES: &[&str] = &[
    ":generatecontent",
    ":streamgeneratecontent",
    ":generateContent",
    ":streamGenerateContent",
];

/// `parse_endpoint` 反推 base_url 时用的**合并后缀表**（任何协议的后缀都剥）
///
/// # 为什么与单协议表分开
///
/// `strip_endpoint_suffix` / `has_endpoint_suffix` 用的是**单协议表**
/// （按协议分叉）—— server 侧 `api.rs` 据此逐协议判定并下发
/// `stripped_protocol` / `stripped_protocols`，前端按协议分叉承诺
/// 「出站会不会自动去重」。这个**分叉是有意保留的**，不要动。
///
/// `parse_endpoint` 的职责是「从用户粘贴的完整端点 URL 反推基础地址」，
/// 粘贴的可能是任意协议的端点，因此用**合并表**。
///
/// # 与单协议表的一致性要求
///
/// 合并表里的**每个字面量**都必须等于某个单协议表里的字面量，
/// 且**大小写策略**必须与 [`suffix_matches`] 一致（路径后缀精确匹配、
/// Gemini 方法名大小写不敏感）—— 否则会出现「parse 说剥了、`has` 说没剥」
/// 的内部矛盾（前端据此判定协议 owner 的承诺链就断了）。
///
/// 一致性由测试 `parse_suffix_table_matches_single_protocol_tables` 钉住。
pub const PARSE_SUFFIXES: &[&str] = &[
    // openai
    "/chat/completions",
    "/completions",
    "/embeddings",
    "/responses",
    "/models",
    // anthropic
    "/v1/messages",
    "/messages",
    // ollama
    "/api/generate",
    "/api/chat",
    "/api/show",
    // gemini（方法名；大小写不敏感由 suffix_matches 保证）
    ":generatecontent",
    ":streamgeneratecontent",
    ":generateContent",
    ":streamGenerateContent",
];

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

/// 主机名允许的字符集（DNS 名字 / IPv4 / IPv6 字面量 / 单标签）
///
/// 用于**白名单校验**——主机名会回填进前端表单并参与 DOM 渲染，
/// 必须排除 `<` `>` `"` `'` `&` 等会破坏 HTML/属性上下文的字符，
/// 也不能含空白与控制字符。
///
/// **IPv6 字面量必须写成 `[...]` 形式**（`[::1]`、`[::1]:8080`）：
/// 括号内只允许十六进制数字、`:`、`.`、`%`（zone id）与字母数字。
/// 非括号形态里出现 `:` 一律拒绝 —— 否则 `x.com:80x`、`x.com:`、`a.b:c`
/// 这类畸形 host 会因为「含冒号」被误送进 IPv6 宽松分支而全部通过校验。
/// 域名 + 端口的形态由调用方先按 `rsplit_once(':')` 拆出端口再交给本函数，
/// 因此这里看到裸 `:` 一律判非法。
fn is_valid_hostname(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    // IPv6 字面量：[...] 形式
    //  - 地址本体（`%` 之前）只允许 hexdigit / ':' / '.' —— IPv6 地址只可能是
    //    十六进制数字，放行任意字母会让 `[gg::1]` 这类非法字面量蒙混过关。
    //  - zone id（`%` 之后）允许字母数字与 `-`/`_`/`.`/`%`（如 `%eth0`）。
    if host.starts_with('[') {
        let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) else {
            return false;
        };
        if inner.is_empty() {
            return false;
        }
        let (addr, zone) = match inner.split_once('%') {
            Some((a, z)) => (a, Some(z)),
            None => (inner, None),
        };
        if addr.is_empty() {
            return false;
        }
        if !addr
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        {
            return false;
        }
        if let Some(z) = zone {
            if z.is_empty()
                || !z
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '%'))
            {
                return false;
            }
        }
        return true;
    }
    // 非括号形态：出现冒号 / 方括号一律非法（端口必须先被拆掉）
    if host.contains(':') || host.contains('[') || host.contains(']') {
        return false;
    }
    host.chars().all(|c| {
        c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_'
    })
}

/// 路径允许的字符集：RFC 3986 的 `pchar` + `/` + `%`
///
/// **必须排除** `<` `>` `"` `'` —— 路径会原样回填到前端并被拼进 HTML
/// （`web/app.js` 的 URL 解析预览用 innerHTML），这四个字符是 HTML 注入的载体。
/// 注意：只排除控制字符与空格是不够的 —— `<script>alert(1)</script>` 与
/// `/<img/src=x/onerror=alert(1)>`（斜杠代替空格）都能在无空白条件下注入。
///
/// `pchar` = unreserved(`A-Za-z0-9-._~`) + sub-delims(`!$&()*+,;=`) + `:@`
/// （sub-delims 中的 `'` 除外：它在 HTML 属性上下文中危险）
/// + `/` + `%`（百分号编码）
fn is_valid_path(path: &str) -> bool {
    path.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '-' | '.'
                    | '_'
                    | '~'
                    | '!'
                    | '$'
                    | '&'
                    | '('
                    | ')'
                    | '*'
                    | '+'
                    | ','
                    | ';'
                    | '='
                    | ':'
                    | '@'
                    | '/'
                    | '%'
            )
    })
}

/// 把 URL 切成「地址部分」与「query / fragment 整段」
///
/// 切分点取**第一个** `?` 或 `#`（谁先出现算谁），其后整段（含分隔符）原样返回。
///
/// # 为什么 query 与 fragment 合成一整段
///
/// RFC 3986 的结构是 `…[?query][#fragment]`，但**切分点无法只落在 `?` 上**——
/// 那样 `#` 之后的内容会被当成 query 的一部分，两者的字符集规则会互相打架。
/// 合成一整段后用一个切分点统一处理，最简单也最安全。
///
/// # 本模块的统一口径：**剥离**
///
/// 本模块的其余几处（[`parse_endpoint`] / [`validate_base_url`] /
/// [`strip_endpoint_suffix`] / [`has_endpoint_suffix`] / [`resolve_endpoint`] /
/// [`normalize_base_url`] / [`has_version_segment`]）都用**同一个切分点**，
/// 语义保持一致：query / fragment **一律剥离**（base_url 契约不含它们）。
/// 理由见 [`validate_base_url`] 的说明（凭据明文落盘 + 随 URL 外发）。
///
/// 剥离时**必须给出可见提示**（[`parse_endpoint`] 的 `notes`）——
/// 静默丢弃会让用户以为鉴权参数已带上，实际发出的是无凭据请求。
fn split_query_fragment(s: &str) -> (&str, &str) {
    match s.find(['?', '#']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// 解析用户输入的端点字符串。
/// - 缺 scheme → 依据是否本地地址补 http/https
/// - 含完整接口路径 → 记录剥离了哪些后缀，并回填 base_url
/// - 主机名/路径含非法字符 → 返回 None（不回显不可信内容到前端）
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
            let is_local = is_local_host(host_part);
            let s = if is_local { "http" } else { "https" };
            notes.push(format!("已自动补全协议 {s}://"));
            (s.to_string(), input.to_string())
        }
    };

    // 2) 先把 query / fragment 整段切出来
    //
    // 切分必须在「拆 host[:port] 与 path」**之前** —— 否则
    // `https://host?key=X`（无路径带 query）这种形态会把 `?key=X` 留在 hostport 里，
    // 主机名白名单必然判非法 → 整个地址解析失败。
    //
    // query / fragment **整段剥离**（不参与 host/path 校验，也不进 base_url）。
    let (head, tail) = split_query_fragment(&rest);

    // 3) 拆 host[:port] 与 path
    let (hostport, path) = match head.find('/') {
        Some(i) => (&head[..i], &head[i..]),
        None => (head, "/"),
    };

    if hostport.is_empty() {
        return None;
    }
    // 拒绝 URL 中携带的 userinfo（`https://user:pass@host`）——
    // 这会把凭据写进 base_url，再回填到前端输入框 / 拼进出站 URL。
    if hostport.contains('@') {
        return None;
    }

    // 3) 拆 host 与 port（兼容 IPv6 字面量 [::1]:8080）
    let (host, port) = if let Some(stripped) = hostport.strip_prefix('[') {
        // IPv6: [::1]:8080
        // `]` 之后的尾部必须为空或 `:端口` —— 否则 `[::1].evil.com` 里的
        // `.evil.com` 会被静默丢弃，导致「校验的 host」与「出站的 host」不一致。
        let Some((h, tail)) = stripped.split_once(']') else {
            return None; // 缺少右括号 → 非法 IPv6 字面量
        };
        if h.is_empty() {
            return None;
        }
        let port = match tail {
            "" => None,
            t => {
                let Some(p) = t.strip_prefix(':') else {
                    return None; // `]` 之后必须是空或 `:端口`
                };
                if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                match p.parse::<u16>() {
                    Ok(pn) => Some(pn),
                    Err(_) => return None, // 端口越界
                }
            }
        };
        (format!("[{h}]"), port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
                // 端口非法（>65535）→ 显式丢弃端口而非静默接受
                match p.parse::<u16>() {
                    Ok(pn) => (h.to_string(), Some(pn)),
                    Err(_) => (h.to_string(), None),
                }
            }
            _ => (hostport.to_string(), None),
        }
    };

    if host.is_empty() {
        return None;
    }
    // 白名单校验：主机名与路径必须只含安全字符
    if !is_valid_hostname(&host) || !is_valid_path(path) {
        return None;
    }

    // 非本地主机使用明文 http → 给出醒目提示（凭据与内容将裸奔于网络）
    if scheme == "http" && !is_local_host(&host) {
        notes.push("警告: 非本机地址使用 http 明文传输，凭据与提示词内容可被中间人截获".into());
    }

    // 4) 反推 base_url：剥离已知接口后缀
    //
    // 用 [`match_suffix`]（与 `strip_endpoint_suffix` / `has_endpoint_suffix`
    // **同源**）而不是自己写 `lower.ends_with(suf)`：
    // - **大小写策略必须一致**。路径后缀按 RFC 3986 是**大小写敏感**的
    //   （`/Chat/Completions` 与 `/chat/completions` 是不同路径），
    //   `strip` / `has` 都是严格匹配。若 parse 宽松而 strip 严格，
    //   就会出现「预览说已剥离、实际剥不掉」——前端拿不到权威 owner，
    //   只能走兜底判据，跨层承诺链就断了。
    // - **最长优先**必须一致。`/v1/chat/completions` 同时以 `/chat/completions`
    //   与 `/completions` 结尾，短的那个先命中会剥成 `…/v1/chat`（错的）。
    //
    // 注意：parse 用的是**合并表**（[`PARSE_SUFFIXES`]，任何协议的后缀都剥），
    // 而 `strip` / `has` 用的是**单协议表** ——
    // 这个差异是**有意保留**的（server 侧 `api.rs` 按协议分叉下发
    // `stripped_protocol` / `stripped_protocols`，前端据此决定
    // 「出站会不会自动去重」）。
    let mut base_path = path.to_string();
    if let Some(suf) = match_suffix(path, PARSE_SUFFIXES) {
        base_path = path[..path.len() - suf.len()].to_string();
        notes.push(format!("已识别接口路径 {suf}，基础地址回填为不含接口的根"));
    }

    // 去掉尾斜杠，且根路径（`/`）按空串处理 ——
    // 否则 `https://x.com` 会被回填成 `https://x.com/`，`https://x.com/v1/`
    // 会被回填成 `https://x.com/v1/`，而 [`validate_base_url`] 给出的是
    // `https://x.com` / `https://x.com/v1`（它会去掉尾斜杠）。
    // 同一地址两个函数两个形态 → 前端回填值与实际出站值长得不一样。
    let base_path = base_path.trim_end_matches('/');
    let base_path = if base_path.is_empty() {
        String::new()
    } else {
        base_path.to_string()
    };
    // base_url = origin + 剥掉接口后缀后的路径（**不含** query / fragment）
    //
    // query / fragment **整段剥离** —— base_url 契约是「不含 query / fragment」，
    // 理由见 [`validate_base_url`]：base_url 会**明文存进 config.json**，
    // 放行 `?key=xxx` 等于提供一条「API Key 绕过 DPAPI 明文落盘」的路径。
    //
    // 剥离时**必须给出可见提示**，不能静默丢弃 ——
    // 用户贴 `?key=xxx` 进来，若静默丢掉，他会以为鉴权参数已带上
    // （实际发出的是无凭据请求）。有了 note，前端能明确告诉他
    // 「key 要填在另一个框」。
    if !tail.is_empty() {
        notes.push(
            "已忽略地址中的查询参数或片段（?… / #…）；API Key 请填写在「API Key」输入框，勿写入地址"
                .into(),
        );
    }
    let origin = format!(
        "{scheme}://{host}{}",
        port.map(|p| format!(":{p}")).unwrap_or_default()
    );
    let base_url = format!("{origin}{base_path}");

    Some(ParsedEndpoint {
        scheme,
        host,
        port,
        path: path.to_string(),
        base_url,
        notes,
    })
}

/// 校验并归一一个「基础地址」是否可安全用于出站请求。
///
/// 这是**出站前的最后一道闸门**：`base_url` 会带着用户的 API Key（Bearer 头）
/// 一起发出去，所以必须在这里挡住：
/// - 非 http/https 协议（如 `file:`、`ftp:`）
/// - 携带 userinfo 的 URL（`https://user:pass@host`）——凭据会写进 base_url 并回显
/// - 非法主机名（`<` `>` `"` 等）
/// - 非法路径字符
/// - **任何 query / fragment**（`?…` / `#…`）——见下方说明
/// - 空地址
///
/// # query / fragment 的口径：**一律拒绝**
///
/// base_url 契约是「**不含 query / fragment**」。本函数是这条契约的**执行点**：
/// 只要地址里出现 `?` 或 `#`，就明确拒绝。
///
/// ## 为什么不允许 base_url 带 query
///
/// - `base_url` 会**明文存进 config.json**。放行 `?key=xxx` 这类 query，
///   等于让 API Key **明文落盘**，与本项目既有的安全模型（API Key 走 DPAPI
///   加密 + `Authorization` / `x-goog-api-key` 请求头传递）**直接冲突**。
/// - 用户真正需要填密钥的地方是 **API Key 输入框**，不是地址栏。
/// - 因此 core 内部四条规则统一为「query / fragment 一律**剥离或拒绝**」：
///   [`parse_endpoint`] 剥离（解析预览）、本函数拒绝（出站闸门）、
///   [`strip_endpoint_suffix`] 与 [`resolve_endpoint`] 剥离。
///
/// ## 错误文案
///
/// 命中 `?` / `#` 时给**专门**的可读错误（提示密钥该填哪里），而不是笼统的
/// 「路径包含非法字符」；其它非法字符仍走原有的「路径包含非法字符」。
///
/// 返回**重建后的规范地址**（协议小写、主机/端口/路径按解析结果拼回、去尾斜杠）。
/// 返回重建串而非原样返回，是为了让「校验时看到的主机」与「出站时使用的主机」
/// 严格一致 —— 否则 IPv6 字面量后缀（`[::1].evil.com`）之类的内容会被静默截断，
/// 前端预览显示的主机与实际出站主机就不是同一个了。
pub fn validate_base_url(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("接口地址不能为空".into());
    }
    let (scheme, rest) = s
        .split_once("://")
        .ok_or_else(|| "接口地址缺少协议（应为 http:// 或 https://）".to_string())?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return Err(format!("接口地址协议必须是 http 或 https，当前: {scheme}"));
    }
    // base_url 契约：**不含 query / fragment** —— 出现 `?` 或 `#` 一律拒绝。
    //
    // 为什么单独给一条可读错误，而不是笼统的「路径包含非法字符」：
    // - `?` / `#` 会被下面的 RFC 3986 `pchar` 白名单挡掉，但那条文案对用户
    //   没有指导性 —— 用户真正需要知道的是「密钥不该写在 URL 里」；
    // - base_url 会**明文存进 config.json**。放行 `?key=xxx` 这类 query，
    //   等于让 API Key 明文落盘，与本项目既有的安全模型（API Key 走 DPAPI
    //   加密 + `Authorization` / `x-goog-api-key` 请求头传递）直接冲突；
    // - 因此 core 内部四条规则统一为「query / fragment 一律**剥离或拒绝**」：
    //   [`parse_endpoint`] 剥离（解析预览）、本函数拒绝（出站闸门）、
    //   [`strip_endpoint_suffix`] 与 [`resolve_endpoint`] 剥离。
    //
    // 错误文案**不回显 query 内容** —— `?key=xxx` 里可能带着真实凭据，
    // 回显到日志或前端就等于二次泄露。
    if rest.contains('?') || rest.contains('#') {
        return Err(
            "接口地址不得包含查询参数或片段（?… / #…）；密钥请填写在 API Key 输入框".into(),
        );
    }
    // userinfo 判定只针对 **host[:port] 段**（authority 段）。
    //
    // `https://user:pass@host` 的风险来自 **authority 段**里的 `@` —— 它会让
    // 「用户以为连的是 host，实际连的是 user:pass@host 的 host」。
    // path 段里的 `@` 是 RFC 3986 `pchar` 的**合法字符**
    //（`pchar` = unreserved + sub-delims + `:` `@`），现实中确实存在
    // `https://api.example.com/v1/users/@me` 这类路径。
    //
    // 因此这里**只**检查 hostport 段；path 段交给 `is_valid_path` 按 `pchar`
    // 白名单校验（它同样排除 `<` `>` `"` `'` 与空白/控制字符）。
    //
    // 若这里额外拒绝 path 里的 `@`，就会与 `parse_endpoint` 打架：
    // parse 按 `pchar` 放行 → base_url 回填进表单 → 用户保存 → 写入侧闸门
    // 调 `validate_base_url` → 被拒 → **用户流程断裂**。
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if hostport.contains('@') {
        return Err("接口地址不得包含用户名/密码（user:pass@host）".into());
    }
    if hostport.is_empty() {
        return Err("接口地址缺少主机名".into());
    }
    // 拆端口
    let (host, port) = if let Some(stripped) = hostport.strip_prefix('[') {
        // IPv6 字面量：[::1] 或 [::1]:8080
        // `]` 之后的尾部必须为空或 `:端口` —— 否则 `[::1].evil.com` 的 `.evil.com`
        // 会被静默丢弃，host 被判成 `[::1]` 放行（白名单形同虚设）。
        let Some((h, tail)) = stripped.split_once(']') else {
            return Err(format!("接口地址 IPv6 字面量缺少右括号: {hostport}"));
        };
        if h.is_empty() {
            return Err("接口地址 IPv6 字面量为空".into());
        }
        let port = match tail {
            "" => None,
            t => {
                let Some(p) = t.strip_prefix(':') else {
                    return Err(format!("接口地址 IPv6 字面量后缀非法: {t}"));
                };
                if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
                    return Err(format!("接口地址 IPv6 端口非法: {p}"));
                }
                match p.parse::<u16>() {
                    Ok(pn) => Some(pn),
                    Err(_) => return Err(format!("接口地址端口非法: {p}")),
                }
            }
        };
        (format!("[{h}]"), port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                match p.parse::<u16>() {
                    Ok(pn) => (h.to_string(), Some(pn)),
                    Err(_) => return Err(format!("接口地址端口非法: {p}")),
                }
            }
            // 出现冒号但尾部不是纯数字（`x.com:80x`、`x.com:`、`a.b:c`）
            // → 畸形端口，直接报错。绝不能把它当主机名整体放行 ——
            // 域名形态的冒号只能来自「:端口」，其余一律非法。
            Some((_, "")) => {
                return Err("接口地址端口非法: 端口为空".to_string());
            }
            Some((_, p)) => {
                return Err(format!("接口地址端口非法: {p}"));
            }
            None => (hostport.to_string(), None),
        }
    };
    if !is_valid_hostname(&host) {
        return Err(format!("接口地址主机名非法: {host}"));
    }
    if !is_valid_path(path) {
        return Err("接口地址路径包含非法字符".into());
    }
    // 重建规范串：保证「校验的主机」与「出站的主机」严格一致
    //
    // query / fragment 已在上面被**拒绝**，这里不存在尾段。
    let port_str = port.map(|p| format!(":{p}")).unwrap_or_default();
    let path_str = path.trim_end_matches('/');
    Ok(format!("{scheme}://{host}{port_str}{path_str}"))
}

/// 归一化基础地址：去尾斜杠、按协议补版本段。
///
/// 协议语义（与服务端出站拼接规则严格对应）：
/// - `openai`：OpenAI 兼容系，默认需要 `/v1`；用户已显式给出版本段则不重复补。
/// - `anthropic`：官方接口自带 `/v1/messages`，**不补** `/v1`（否则变成 `/v1/v1/messages`）。
/// - `ollama`：原生接口是 `/api/chat`，**不补** `/v1`（否则 404）。
/// - `gemini`：原生接口是 `/v1beta/models/...`，**不补** `/v1`（否则 404）。
///
/// 「已带版本段」的判定按**路径段精确匹配**（`/v1`、`/v1beta`、`/v1beta1`…），
/// 不用字符串 `contains` —— 否则 `https://x.com/v1proxy` 会被误判为已带版本段。
///
/// # query / fragment
///
/// **一律丢弃**（切分点取第一个 `?` 或 `#`，其后整段含分隔符直接丢弃）。
/// base_url 契约是「不含 query / fragment」——理由见 [`validate_base_url`]：
/// base_url 会明文存进 config.json，放行 `?key=xxx` 会让 API Key 明文落盘。
///
/// 丢弃同时也解决了「版本段只能补在 path 上」的老问题：
/// `https://x.com/v1?key=ABC` 归一成 `https://x.com/v1`，
/// 而不会补成 `https://x.com/v1?key=ABC/v1` 那种坏 URL。
pub fn normalize_base_url(raw: &str, protocol: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    // query / fragment 一律丢弃（base_url 契约：不含 query / fragment）
    let (head, _tail) = split_query_fragment(s);
    let head = head.trim_end_matches('/');
    if !matches!(protocol, "openai") {
        return head.to_string();
    }
    if !has_version_segment(head) {
        format!("{head}/v1")
    } else {
        head.to_string()
    }
}

/// 协议 → 已知接口后缀表（`strip_endpoint_suffix` 的逆运算依据）
///
/// 未知协议返回 `None` —— 调用方据此**原样返回**，绝不猜测、也不跨协议乱剥
/// （例如用 ollama 协议去剥 anthropic 的 `/v1/messages`）。
fn suffixes_for_protocol(protocol: &str) -> Option<&'static [&'static str]> {
    match protocol {
        "openai" => Some(OPENAI_SUFFIXES),
        "anthropic" => Some(ANTHROPIC_SUFFIXES),
        "ollama" => Some(OLLAMA_SUFFIXES),
        "gemini" => Some(GEMINI_SUFFIXES),
        _ => None,
    }
}

/// 判断 `path` 是否以 `suffix` 结尾
///
/// **大小写策略分两类**（这是本模块唯一一处不区分大小写的地方）：
/// - 以 `/` 开头的**路径后缀**（`/v1/messages`、`/api/generate` …）→ **精确匹配**。
///   路径段大小写敏感，宽松匹配会把用户的真实路径误判成接口后缀并吃掉。
/// - 以 `:` 开头的 **Gemini 方法名后缀**（`:generateContent` …）→ **大小写不敏感**。
///   Gemini 真实接口用的是驼峰 `:generateContent`，而历史常量表写成了全小写
///   `:generatecontent`；方法名本身不是路径段，做宽松匹配更稳（两种形态都已在
///   [`GEMINI_SUFFIXES`] 中显式列出）。
///
/// `suffix` 必须非空；按字节切分前先校验字符边界，避免多字节字符被截断 panic。
fn suffix_matches(path: &str, suffix: &str) -> bool {
    if suffix.is_empty() || path.len() < suffix.len() {
        return false;
    }
    let start = path.len() - suffix.len();
    if !path.is_char_boundary(start) {
        return false;
    }
    let tail = &path[start..];
    if suffix.starts_with(':') {
        tail.eq_ignore_ascii_case(suffix)
    } else {
        tail == suffix
    }
}

/// 在 `path` 尾部查找协议已知的后缀，命中则返回**最长**的那个
///
/// 最长优先是必需的：`/v1/chat/completions` 同时以 `/chat/completions` 与
/// `/completions` 结尾，短的那个先命中会把 base 剥成 `…/v1/chat`（错的）。
fn match_suffix(path: &str, suffixes: &[&'static str]) -> Option<&'static str> {
    let mut best: Option<&'static str> = None;
    for &suf in suffixes {
        if !suffix_matches(path, suf) {
            continue;
        }
        match best {
            // 已有的更长 → 不覆盖（平长时保留先出现的）
            Some(b) if b.len() >= suf.len() => {}
            _ => best = Some(suf),
        }
    }
    best
}

/// 去掉 URL 尾部的斜杠，但**不得破坏 `scheme://` 形式**
///
/// 直接 `trim_end_matches('/')` 会把 `http://` 削成 `http:` —— 出站拼接会得到
/// `http:/api/generate` → 解析成非法 URL。这里在削到「尾部只剩冒号」时停下。
fn trim_trailing_slashes(url: &str) -> &str {
    let mut cur = url;
    loop {
        let next = cur.trim_end_matches('/');
        if next.len() == cur.len() {
            return cur;
        }
        if next.ends_with(':') {
            return cur;
        }
        cur = next;
    }
}

/// 剥掉 `base_url` 尾部**已存在的**该协议接口后缀，返回可直接用于出站拼接的基础地址。
///
/// # 为什么需要
///
/// 用户手填的 base_url 有两种来源：前端从完整端点 URL 反推（前端侧已剥离），
/// 或**用户直接手填成带接口后缀的形式**（`http://127.0.0.1:11434/api/generate`）。
/// 出站若无条件再拼一次后缀，就会得到
/// `http://127.0.0.1:11434/api/generate/api/generate` → **404**。
/// 本函数是「出站拼接」的逆运算，把已存在的后缀摘掉，让拼接结果唯一。
///
/// # 语义约定
///
/// 1. **按协议匹配**：anthropic 只认 [`ANTHROPIC_SUFFIXES`]、ollama 只认
///    [`OLLAMA_SUFFIXES`]、gemini 只认 [`GEMINI_SUFFIXES`]、openai 只认
///    [`OPENAI_SUFFIXES`]。**未知协议原样返回**（不猜）。
///    协议不匹配时同样原样返回（不跨协议乱剥）。
///    注：无论协议是否已知，query / fragment 都**一律丢弃**（见约定 4）。
/// 2. **大小写**：路径后缀（`/v1/messages` 等）**精确匹配**；仅 Gemini 的
///    方法名后缀（`:generateContent`）大小写不敏感 —— 见私有辅助函数
///    `suffix_matches`。
/// 3. **只剥一层**：尾部连续出现两次相同后缀时只去掉最外层
///    （`…/api/generate/api/generate` → `…/api/generate`）。
///    **理由**：保守做法 —— 用户自建的双层路径（如网关把 `/v1/messages` 挂在
///    自己的前缀下再叠一层）会被误剥成不存在的地址；反过来少剥一层最坏结果是
///    仍为 404（可观测、可修），误剥则可能打到**错误的接口**（不可观测）。
/// 4. **query / fragment**：**一律丢弃**。base_url 契约是「不含 query / fragment」
///    —— 详见 [`validate_base_url`] 的说明：base_url 会明文存进 config.json，
///    放行 `?key=xxx` 会让 API Key 明文落盘，与本项目「API Key 走 DPAPI 加密 +
///    `Authorization` / `x-goog-api-key` 请求头传递」的安全模型直接冲突。
///    切分点取第一个 `?` 或 `#`，其后整段（含分隔符）**直接丢弃**，
///    不再搬运到结果末尾。
/// 5. **空白与斜杠**：先 `trim()`；结果**不得以 `/` 结尾**
///    （`http://x:11434/api/generate/` → `http://x:11434`），
///    但 `scheme://` 的双斜杠不会被削掉。
/// 6. **openai 保留 `/v1`**：`https://api.openai.com/v1/chat/completions` →
///    `https://api.openai.com/v1`（**保留 `/v1`**）。这与
///    [`normalize_base_url`] 对 openai 补 `/v1` 的行为一致 ——
///    出站的 [`resolve_endpoint`] 会自行判断是否已有版本段，两种 base 都能拼对。
///
/// # 不做的事
///
/// - 不改 scheme 大小写、不改主机名大小写、不校验 host 合法性
///   （出站前的合法性校验由 [`validate_base_url`] 负责）。
/// - 不为 openai 补 `/v1`（那是 [`normalize_base_url`] 的职责）。
pub fn strip_endpoint_suffix(base_url: &str, protocol: &str) -> String {
    let s = base_url.trim();
    if s.is_empty() {
        return String::new();
    }
    // query / fragment 一律**丢弃**（base_url 契约：不含 query / fragment ——
    // 详见 [`validate_base_url`] 的说明）。切分点取第一个 `?` 或 `#`。
    let (head, _tail) = split_query_fragment(s);
    // 未知协议 → 原样返回（不猜）
    let Some(suffixes) = suffixes_for_protocol(protocol) else {
        return head.to_string();
    };

    // 只在 path 部分匹配后缀
    // 先去掉尾斜杠再判断后缀（`/api/generate/` 与 `/api/generate` 等价）
    let head = trim_trailing_slashes(head);

    match match_suffix(head, suffixes) {
        Some(suf) => {
            let stripped = &head[..head.len() - suf.len()];
            trim_trailing_slashes(stripped).to_string()
        }
        // 未命中任何已知后缀 → 原样（仅去掉尾斜杠）
        None => head.to_string(),
    }
}

/// 判断 `base_url` 尾部**是否已经是**该协议的完整接口路径
///
/// 与 [`strip_endpoint_suffix`] 共用同一套匹配逻辑（同一份后缀表、同样的大小写
/// 策略、同样只认 path 部分），因此在「是否命中接口后缀」这一点上二者结论必然一致：
/// `has_endpoint_suffix(u, p) == true` ⇔ `strip_endpoint_suffix(u, p)` 确实**剥掉了后缀**。
/// （唯一无关差异：`strip_endpoint_suffix` 还会顺带去掉尾斜杠，
/// 那是归一化而非剥后缀。）
///
/// - 未知协议 → `false`。
/// - 空串 → `false`。
/// - 尾部带斜杠不影响判定（`…/api/generate/` 仍算命中）。
/// - query / fragment 不参与判定（只看 path 部分）。
pub fn has_endpoint_suffix(base_url: &str, protocol: &str) -> bool {
    let s = base_url.trim();
    if s.is_empty() {
        return false;
    }
    let Some(suffixes) = suffixes_for_protocol(protocol) else {
        return false;
    };
    let (head, _) = split_query_fragment(s);
    let head = trim_trailing_slashes(head);
    match_suffix(head, suffixes).is_some()
}

/// 路径中是否已存在版本段（`/v1`、`/v1beta`、`/v2` …）
///
/// 只按完整路径段匹配，且必须是 **`v` + 数字** 的形态（可带 beta/alpha 后缀）：
/// `v1` / `v1beta` / `v2` / `v1beta1`。
/// 排除 `video`、`vue` 这类以 v 开头但不是版本号的首词，
/// 也排除 `api-v1`（含前缀的段不算独立版本段）。
///
/// 只看 path 部分（去掉 `scheme://host[:port]`），query / fragment 不参与判定。
///
/// # 为什么公开
///
/// 「这个 base_url 是否已带版本段」是出站拼 URL 的**跨 crate 共用判定**：
/// core 的 [`normalize_base_url`] 用它决定要不要补 `/v1`，
/// core 的 [`resolve_endpoint`] 用它决定要不要补 `/v1beta`，
/// server 侧拼 gemini 出站地址时也需要同一判定。
/// 语义只存在于 core 一处，server 直接调用即可 ——
/// 否则同一规则在两个 crate 各写一份，日后必然漂移
/// （这正是 gemini 出站 URL 拼出 `/v1beta/v1beta` 双版本段的结构性成因）。
///
/// 注意与 [`is_version_segment`] 的区别：本函数看的是**路径中任意一段**，
/// 而版本段在接口路径里应当是**末段**（`/v1beta` 是版本段，`/v1/foo` 的末段 `foo` 不是）。
/// 判断「末段是不是版本段」时，请取末段后调用 [`is_version_segment`]。
pub fn has_version_segment(url: &str) -> bool {
    // 只看 path 部分（去掉 scheme://host[:port] 与 query / fragment）
    let (head, _) = split_query_fragment(url);
    let path = match head.find("://") {
        Some(i) => {
            let rest = &head[i + 3..];
            match rest.find('/') {
                Some(j) => &rest[j..],
                None => return false,
            }
        }
        None => head,
    };
    path.split('/').any(is_version_segment)
}

/// 判断单个路径段是否为版本段（严格语法）
///
/// `v` + 数字，可选后缀仅限 `beta` / `alpha` / `rc`（可带数字后缀）：
/// `v1`、`v1beta`、`v1beta1`、`v2`、`v3rc1` 均为版本段；
/// `v1proxy`、`video`、`vue`、`api-v1` **均不是**——误判会导致漏补 `/v1` → 出站 404。
///
/// 大小写不敏感（`V1` 也算版本段）。
///
/// # 为什么公开
///
/// 见 [`has_version_segment`] 的说明：版本段判定规则只应在 core 一处。
/// server 需要「路径末段是否为版本段」时，取末段后调用本函数即可。
pub fn is_version_segment(seg: &str) -> bool {
    let s = seg.to_ascii_lowercase();
    let bytes = s.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'v' || !bytes[1].is_ascii_digit() {
        return false;
    }
    let mut i = 1;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == bytes.len() {
        return true; // 纯数字版本，如 v1 / v2
    }
    let suffix = &s[i..];
    for base in ["beta", "alpha", "rc"] {
        if let Some(rest) = suffix.strip_prefix(base) {
            return rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

/// 判断主机是否为**本地/内网**地址（用于决定默认协议与是否允许 http 明文）。
///
/// 判定必须精确：
/// - `localhost` 精确匹配（`localhost.evil.com` **不是**本地）
/// - IPv4 回环 `127.0.0.0/8`、私有段 `10/8`、`172.16/12`、`192.168/16`（按数值区间判定，
///   而非字符串前缀，避免 `172.16.evil.com` 命中）
/// - IPv6 环回 `::1`
///
/// **必须容忍带端口的输入**（`localhost:11434`、`127.0.0.1:11434`、`[::1]:11434`）——
/// 用户粘贴本地地址时最常见的就是带端口形态，若不剥离端口，本地地址会被误判为
/// 远端 → 协议被升级成 https → 本地服务（无 TLS）直接连不上。
pub fn is_local_host(host: &str) -> bool {
    let raw = host.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return false;
    }
    // 1) IPv6 字面量：[::1] 或 [::1]:8080 → 取括号内（不再剥端口）
    // 2) 裸 IPv6（含多个 ':'，如 ::1）→ 整体就是地址，不能按最后一个冒号切
    // 3) IPv4 / 域名 → 剥离可选端口
    let h = if raw.starts_with('[') {
        match raw.split_once(']') {
            Some((inside, _)) => inside.to_string(),
            None => return false,
        }
    } else if raw.matches(':').count() > 1 {
        raw
    } else {
        match raw.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
                head.to_string()
            }
            _ => raw,
        }
    };
    let h = h.trim_matches(|c| c == '[' || c == ']');
    if h.is_empty() {
        return false;
    }
    if h == "localhost" {
        return true;
    }
    // IPv6 环回
    if h == "::1" || h == "0:0:0:0:0:0:0:1" {
        return true;
    }
    // IPv4：按数值区间判定（字符串前缀会把 `10.evil.com` 误判为本地）
    let octets: Vec<&str> = h.split('.').collect();
    if octets.len() == 4 {
        let nums: Option<Vec<u32>> = octets.iter().map(|o| o.parse::<u32>().ok()).collect();
        if let Some(n) = nums {
            if n.iter().any(|x| *x > 255) {
                return false;
            }
            let [a, b, _c, _d] = [n[0], n[1], n[2], n[3]];
            return a == 127
                || a == 10
                || (a == 172 && (16..32).contains(&b))
                || (a == 192 && b == 168);
        }
    }
    false
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

/// 把 `base_url`（可能是**完整端点 URL**）解析成该协议的**最终请求 URL**
///
/// # 为什么需要
///
/// 用户填的 `base_url` 有两种来源：只填到域名（`https://api.deepseek.com`），
/// 或直接从文档/控制台**粘贴完整端点**（`https://api.openai.com/v1/chat/completions`、
/// `https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent`）。
/// 出站时若无条件再拼一次协议后缀，就会变成
/// `…/v1/chat/completions/v1/chat/completions` → **404**。
/// 本函数把两种输入**归一**到同一个最终 URL。
///
/// # 各协议的归一规则
///
/// | 协议 | 规则 |
/// |---|---|
/// | `openai` | 剥掉全部已知的 openai 后缀（`/chat/completions`、`/completions`、`/embeddings`、`/responses`、`/models`，**大小写不敏感**且**剥到不命中为止**），再按需补 `/v1`，最后拼**小写**的 `/chat/completions` |
/// | `anthropic` | 剥掉 `/v1/messages`、`/messages`；若剥完仍以 `/v1` 结尾则去掉该 `/v1`（避免 `/v1/v1/messages`），再拼 `/v1/messages` |
/// | `ollama` | 剥掉 `/api/generate`、`/api/chat`、`/api/show`，再拼 `/api/generate` |
/// | `gemini` | 见下方说明 |
/// | 其它 | **原样返回**（不猜、不跨协议乱剥） |
///
/// # gemini 为什么特殊
///
/// Gemini 的调用形态是 `/v1beta/models/{model}:generateContent` —— 后缀是
/// **方法名**（`:generateContent`）而不是路径段，且 `{model}` 段也在路径里。
/// 因此不能只剥 `:generateContent`（那样会残留 `/v1beta/models/{model}`）。
/// 规则是：
///
/// - 若路径中含 `/models/` 段 → **保留 `/models/` 之前的全部前缀**
///   （含 `/v1beta` 版本段，也含网关子路径如 `/gemini`），
///   再拼 `/models/{model}:generateContent`；
/// - 否则若路径**以版本段结尾**（`/v1`、`/v1beta` …）→ 拼 `/models/{model}:generateContent`；
/// - 否则（无版本段）→ 补 `/v1beta/models/{model}:generateContent`。
///
/// # 幂等性
///
/// 对已解析出的最终 URL 再调一次，结果不变 —— 出站路径可以安全地重复调用。
///
/// # query / fragment
///
/// **一律丢弃**（切分点取第一个 `?` 或 `#`，其后整段含分隔符直接丢掉）。
/// 这与 [`strip_endpoint_suffix`] / [`parse_endpoint`] / [`validate_base_url`]
/// 同一口径 —— base_url 契约不含 query / fragment，理由见 [`validate_base_url`]。
///
/// # 参数
///
/// - `model`：仅 `gemini` 用到，且应是**已 percent-encode** 的值
///   （含 `/` `?` `#` 空格等字符的模型名必须先编码，否则会破坏 URL 结构）。
///   传空串时回退使用 URL 里自带的 model 段。
/// - `base_url` 应带协议（`https://` / `http://`）；缺失时按下述规则拼接但不补协议。
///
/// # 与既有函数的关系
///
/// - [`strip_endpoint_suffix`] 是「剥一层后缀、保留其余部分」的逆运算，用于配置回填；
/// - [`has_endpoint_suffix`] 只判断「尾部是否已是该协议的后缀」。
/// - 本函数是**出站前最后一步**：产出可直接发请求的 URL。
pub fn resolve_endpoint(base_url: &str, protocol: &str, model: &str) -> String {
    let raw = base_url.trim();
    if raw.is_empty() {
        return String::new();
    }
    // query / fragment 整段**丢弃**（只对 path 部分做后缀运算）
    let (head, _tail) = split_query_fragment(raw);
    let head = trim_trailing_slashes(head);

    match protocol {
        "openai" => resolve_openai_endpoint(head),
        "anthropic" => resolve_anthropic_endpoint(head),
        "ollama" => resolve_ollama_endpoint(head),
        "gemini" => resolve_gemini_endpoint(head, model),
        // 未知协议 → 原样返回（不猜）
        _ => head.to_string(),
    }
}

/// 反复剥掉**已知的**接口后缀，直到不再命中
///
/// # 为什么是「剥到不命中」而不是「只剥一层」
///
/// 输入可能是用户把同一段端点粘贴了两次（`…/api/generate/api/generate`）。
/// 只剥一层会让出站 URL 仍是两层 → 404。
/// 这里循环剥到**不再命中已知后缀**，使出站 URL 收敛到单层。
///
/// # 为什么这不算「误剥到不存在的接口」
///
/// [`strip_endpoint_suffix`] 的约定 3（只剥一层）约束的是**配置回填**场景 ——
/// 不能静默改写用户填的值。本函数是**出站拼装的最后一步**，职责就是产出
/// 「可直接发请求的 URL」，它本来就在大量改写（补 `/v1`、`/api/chat`→`/api/generate`、
/// 换 gemini model 段），多剥一层不违背职责。
///
/// 剥的**只有已知后缀表里的字面量**，主机与其它路径段永不动 ——
/// 不存在「打到错误主机」的风险（那由 `validate_base_url` 挡）。
///
/// # 大小写
///
/// 已知后缀匹配**大小写不敏感**（`suffix_matches` 对 `:` 开头的方法名不敏感，
/// 这里对路径后缀同样按不敏感处理）—— 用户粘贴的 `/API/GENERATE` 也应收敛。
fn strip_all_suffixes(base: &str, suffixes: &[&'static str]) -> String {
    let mut cur = base.to_string();
    // 先算成 owned 再赋值 —— `strip_suffix_ci` 的返回值借用了 `cur`
    while let Some(next) = strip_suffix_ci(&cur, suffixes) {
        let next = next.to_string();
        if next.len() >= cur.len() {
            break;
        }
        cur = next;
    }
    trim_trailing_slashes(&cur).to_string()
}

/// openai：归一到 `{base}/chat/completions`（缺 `/v1` 时补上）
///
/// # 大小写归一
///
/// 用户粘贴的路径可能是 `/V1/Chat/Completions`。真实 OpenAI 兼容服务的
/// 接口路径是**小写**的 `/chat/completions` → 原样带出去会 404。
/// 因此这里剥掉已知后缀后**按规范重拼**小写的 `/chat/completions`。
///
/// 但**版本段保持用户原样**（`/V1` 不改成 `/v1`）—— 版本段可能是某些服务
/// 真实路径的一部分（如网关挂在 `/V1` 下），改小写会打到不存在的接口。
fn resolve_openai_endpoint(base: &str) -> String {
    // 剥掉所有已知的 openai 后缀（`/chat/completions`、`/embeddings`、
    // `/completions`、`/responses`、`/models`），并收敛叠放的重复段
    let stripped = strip_all_suffixes(base, OPENAI_SUFFIXES);
    if has_exact_v1_segment(&stripped) {
        format!("{stripped}/chat/completions")
    } else {
        format!("{stripped}/v1/chat/completions")
    }
}

/// anthropic：归一到 `{base}/v1/messages`（避免 `/v1/v1/messages`）
fn resolve_anthropic_endpoint(base: &str) -> String {
    // 最长优先：`/v1/messages` 先于 `/messages` 命中；叠放的重复段收敛到单层
    let stripped = strip_all_suffixes(base, ANTHROPIC_SUFFIXES);
    // 剥完仍以 `/v1` 结尾 → 去掉，避免拼出 `/v1/v1/messages`
    let stripped = stripped.strip_suffix("/v1").unwrap_or(&stripped);
    format!("{stripped}/v1/messages")
}

/// ollama：归一到 `{base}/api/generate`
fn resolve_ollama_endpoint(base: &str) -> String {
    let stripped = strip_all_suffixes(base, OLLAMA_SUFFIXES);
    format!("{stripped}/api/generate")
}

/// gemini：归一到 `{base}/models/{model}:generateContent`
fn resolve_gemini_endpoint(base: &str, model: &str) -> String {
    // 先剥掉尾部的方法名（`:generateContent` / `:streamGenerateContent`）
    let head = match_suffix(base, GEMINI_SUFFIXES)
        .map(|suf| trim_trailing_slashes(&base[..base.len() - suf.len()]))
        .unwrap_or(base);

    // 路径里带 `/models/` 段（`…/v1beta/models/gemini-2.0-pro`）：
    // 保留 `/models/` 之前的**全部**前缀 —— 版本段（`/v1beta`）与
    // 网关子路径（如 `/gemini`）都在前缀里，不能被吃掉。
    if let Some(idx) = head.rfind("/models/") {
        let prefix = &head[..idx];
        let url_model = &head[idx + "/models/".len()..];
        let m = pick_model(model, url_model);
        return format!("{prefix}/models/{m}:generateContent");
    }
    // 路径以 `/models` 结尾（模型列表端点，如 `…/v1beta/models`）：
    // 去掉尾部 `/models` 后按「无 `/models/` 段」处理。
    let head = head.strip_suffix("/models").unwrap_or(head);

    // 无 `/models/` 段：路径以版本段结尾则不补版本，否则补 `/v1beta`
    let last_seg = head.rsplit('/').next().unwrap_or("");
    let base = if is_version_segment(last_seg) {
        head.to_string()
    } else {
        format!("{head}/v1beta")
    };
    let m = pick_model(model, "");
    format!("{base}/models/{m}:generateContent")
}

/// 选 model：优先用调用方传入的（已 percent-encode），为空时回退用 URL 自带的
fn pick_model(model: &str, url_model: &str) -> String {
    let m = model.trim();
    if m.is_empty() {
        url_model.to_string()
    } else {
        m.to_string()
    }
}

/// 剥掉 `url` 尾部**最长的**、大小写不敏感匹配的已知后缀，返回剥掉后的部分
///
/// 最长优先是必需的：`/v1/chat/completions` 同时以 `/chat/completions` 与
/// `/completions` 结尾，短的那个先命中会剥错。
fn strip_suffix_ci<'a>(url: &'a str, suffixes: &[&'static str]) -> Option<&'a str> {
    let lower = url.to_ascii_lowercase();
    let mut best: Option<&'static str> = None;
    for &suf in suffixes {
        let sl = suf.to_ascii_lowercase();
        if sl.len() > lower.len() || !lower.ends_with(&sl) {
            continue;
        }
        // 字节边界检查：URL 可能含多字节字符，按字节切会 panic
        let start = url.len() - suf.len();
        if !url.is_char_boundary(start) {
            continue;
        }
        match best {
            Some(b) if b.len() >= suf.len() => {}
            _ => best = Some(suf),
        }
    }
    best.map(|suf| &url[..url.len() - suf.len()])
}

/// 路径中是否存在**恰好为 `v1`** 的路径段
///
/// 判定规则：路径段值等于 `v1`（**忽略大小写**）。
/// 这是 core 内 openai 出站拼 URL 的**唯一**版本段判定 ——
/// 出站拼接的权威实现在 [`resolve_endpoint`]，本函数只服务它。
///
/// 与 [`has_version_segment`] 的区别：后者接受任意 `v<数字>` 版本段
/// （`v1beta` / `v2` …），而 openai 兼容系只认 `v1`。
fn has_exact_v1_segment(url: &str) -> bool {
    // query / fragment 不参与判定（`/v1?key=ABC` 里的版本段是 `v1`，
    // 不是 `v1?key=ABC`）
    let (head, _) = split_query_fragment(url);
    let path = match head.find("://") {
        Some(i) => {
            let rest = &head[i + 3..];
            match rest.find('/') {
                Some(j) => &rest[j..],
                None => return false,
            }
        }
        None => head,
    };
    path.split('/').any(|seg| seg.eq_ignore_ascii_case("v1"))
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

    /// query / fragment **一律剥离**，不进 base_url
    ///
    /// 改写原因：此测试原先断言「query / fragment 原样保留在 base_url 里」。
    /// 项目负责人已裁定 base_url **不含** query / fragment ——
    /// base_url 会明文存进 config.json，保留 `?key=xxx` 会让 API Key 明文落盘，
    /// 与本项目「API Key 走 DPAPI 加密 + `Authorization` / `x-goog-api-key`
    /// 请求头传递」的安全模型冲突。密钥请填在 API Key 输入框。
    #[test]
    fn parse_drops_query_and_fragment_from_base_url() {
        let p = parse_endpoint("https://x.com/v1/chat/completions?a=1#f").unwrap();
        assert_eq!(p.path, "/v1/chat/completions");
        // query / fragment **剥离**（base_url 契约：不含 query / fragment）
        assert_eq!(p.base_url, "https://x.com/v1");
        // base_url 里不得残留 `?` / `#`
        assert!(!p.base_url.contains('?') && !p.base_url.contains('#'));
        // 剥离必须有**可见提示** —— 静默丢弃会让用户以为鉴权参数已带上
        assert!(
            p.notes
                .iter()
                .any(|n| n.contains("已忽略地址中的查询参数或片段")),
            "剥离 query / fragment 必须给出提示: {:?}",
            p.notes
        );
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

    // ── 白名单 / 注入防护 ──────────────────────────────────────

    /// 回归：主机名含 HTML 元字符时必须解析失败（前端会把 host 回填进 DOM）
    #[test]
    fn parse_rejects_html_in_hostname() {
        assert!(parse_endpoint("https://x.com\"><img src=x onerror=alert(1)>").is_none());
        assert!(parse_endpoint("https://evil<script>").is_none());
        assert!(parse_endpoint("https://a b.com").is_none());
    }

    /// 回归：URL 中携带 userinfo（凭据）必须解析失败，不得回填到前端
    #[test]
    fn parse_rejects_userinfo() {
        assert!(parse_endpoint("https://user:secret@api.deepseek.com/v1").is_none());
        assert!(parse_endpoint("https://token@internal.corp/v1").is_none());
    }

    /// 非本地主机用 http 时必须给出明文传输告警
    #[test]
    fn parse_warns_plaintext_http_for_remote() {
        let p = parse_endpoint("http://api.deepseek.com/v1").unwrap();
        assert!(p.notes.iter().any(|n| n.contains("http 明文")));
        // 本地 http 不告警
        let local = parse_endpoint("http://127.0.0.1:11434").unwrap();
        assert!(!local.notes.iter().any(|n| n.contains("http 明文")));
    }

    /// 回归：`localhost.evil.com` 不是本地地址，不得降级为 http
    #[test]
    fn parse_localhost_lookalike_is_remote() {
        assert!(!is_local_host("localhost.evil.com"));
        assert!(!is_local_host("127.evil.com"));
        assert!(!is_local_host("10.evil.com"));
        assert!(!is_local_host("172.16.evil.com"));
        assert!(!is_local_host("192.168.evil.com"));
        // 真正的本地地址
        assert!(is_local_host("localhost"));
        assert!(is_local_host("127.0.0.1"));
        assert!(is_local_host("127.1.2.3"));
        assert!(is_local_host("10.1.2.3"));
        assert!(is_local_host("172.16.0.1"));
        assert!(is_local_host("172.31.255.255"));
        assert!(is_local_host("192.168.1.1"));
        assert!(is_local_host("::1"));
        assert!(is_local_host("[::1]"));
        // 172.15/172.32 不在私有段内
        assert!(!is_local_host("172.15.0.1"));
        assert!(!is_local_host("172.32.0.1"));
        assert!(!is_local_host("8.8.8.8"));
    }

    /// 非法端口（>65535）不得被静默接受
    #[test]
    fn parse_rejects_invalid_port() {
        let p = parse_endpoint("https://x.com:99999/v1").unwrap();
        assert_eq!(p.port, None);
    }

    // ── 协议语义 ────────────────────────────────────────────────

    #[test]
    fn normalize_does_not_add_v1_for_non_openai_protocols() {
        // Anthropic：服务端拼 {base}/v1/messages，补 /v1 会变成 /v1/v1/messages
        assert_eq!(
            normalize_base_url("https://api.anthropic.com", "anthropic"),
            "https://api.anthropic.com"
        );
        // Ollama：服务端拼 {base}/api/generate，补 /v1 会 404
        assert_eq!(
            normalize_base_url("http://localhost:11434", "ollama"),
            "http://localhost:11434"
        );
        // Gemini：服务端拼 {base}/v1beta/models/...，补 /v1 会 404
        assert_eq!(
            normalize_base_url("https://generativelanguage.googleapis.com", "gemini"),
            "https://generativelanguage.googleapis.com"
        );
    }

    /// 回归：`/v1proxy` 之类不能被当成已带版本段
    #[test]
    fn normalize_version_segment_is_exact() {
        assert_eq!(
            normalize_base_url("https://x.com/v1proxy", "openai"),
            "https://x.com/v1proxy/v1"
        );
        assert_eq!(
            normalize_base_url("https://x.com/v1beta", "openai"),
            "https://x.com/v1beta"
        );
        // 以 v 开头但不是版本号的首词
        assert_eq!(
            normalize_base_url("https://x.com/video", "openai"),
            "https://x.com/video/v1"
        );
        assert_eq!(
            normalize_base_url("https://x.com/vue", "openai"),
            "https://x.com/vue/v1"
        );
        // 含前缀的段不算独立版本段
        assert_eq!(
            normalize_base_url("https://x.com/api-v1", "openai"),
            "https://x.com/api-v1/v1"
        );
        // 合法版本段（含 beta/rc 后缀）
        assert_eq!(
            normalize_base_url("https://x.com/v1beta1", "openai"),
            "https://x.com/v1beta1"
        );
        assert_eq!(
            normalize_base_url("https://x.com/v3rc1", "openai"),
            "https://x.com/v3rc1"
        );
    }

    /// 回归：IPv6 环回 `::1` 必须识别为本地（不能被端口剥离逻辑破坏）
    #[test]
    fn ipv6_loopback_is_local() {
        assert!(is_local_host("::1"));
        assert!(is_local_host("[::1]"));
        assert!(is_local_host("[::1]:8080"));
        assert!(is_local_host("0:0:0:0:0:0:0:1"));
    }

    /// 回归：带端口的本机地址必须识别为本地（否则协议被升级成 https → 连不上本地服务）
    #[test]
    fn local_host_with_port_is_local() {
        assert!(is_local_host("localhost:11434"));
        assert!(is_local_host("127.0.0.1:11434"));
        assert!(is_local_host("127.0.0.1"));
        assert!(is_local_host("10.0.0.5:8080"));
        assert!(is_local_host("192.168.1.10:3000"));
    }

    /// 回归：无 scheme 的本机地址必须补 http（不能升级成 https）
    #[test]
    fn parse_localhost_with_port_keeps_http() {
        let p = parse_endpoint("localhost:11434").unwrap();
        assert_eq!(p.scheme, "http", "本机地址必须补 http，不能升级成 https");
        let p2 = parse_endpoint("127.0.0.1:8080/v1").unwrap();
        assert_eq!(p2.scheme, "http");
    }

    /// 回归：`validate_base_url` 必须挡住凭据外泄与非 http 协议
    #[test]
    fn validate_base_url_blocks_dangerous_inputs() {
        // userinfo 凭据
        assert!(validate_base_url("https://user:pass@evil.com").is_err());
        assert!(validate_base_url("https://token@evil.com").is_err());
        // 非 http(s) 协议
        assert!(validate_base_url("file:///C:/Windows/win.ini").is_err());
        assert!(validate_base_url("ftp://evil.com").is_err());
        // 空 / 缺协议 / 缺主机
        assert!(validate_base_url("").is_err());
        assert!(validate_base_url("evil.com").is_err());
        assert!(validate_base_url("https://").is_err());
        // 路径含 HTML 元字符
        assert!(validate_base_url("https://x.com/<script>").is_err());
        // 合法值
        assert_eq!(
            validate_base_url("https://api.deepseek.com/").unwrap(),
            "https://api.deepseek.com"
        );
        assert_eq!(
            validate_base_url("http://localhost:11434").unwrap(),
            "http://localhost:11434"
        );
        assert_eq!(
            validate_base_url("https://x.com/v1").unwrap(),
            "https://x.com/v1"
        );
    }

    /// 回归：路径白名单必须挡住免空格的 HTML 注入载荷
    #[test]
    fn path_whitelist_blocks_html_payloads() {
        // 免空格载荷（斜杠代替空格）
        assert!(parse_endpoint("https://x.com/<img/src=x/onerror=alert(1)>").is_none());
        assert!(parse_endpoint("https://x.com/<script>alert(1)</script>").is_none());
        // 属性注入
        assert!(parse_endpoint("https://x.com/x\"onmouseover=\"alert(1)").is_none());
        assert!(parse_endpoint("https://x.com/x'onmouseover='alert(1)").is_none());
        // 正常路径放行
        assert!(parse_endpoint("https://x.com/v1/chat/completions").is_some());
        assert!(parse_endpoint("https://x.com/a-b_c.d/e~f").is_some());
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

    // ── IPv6 / 畸形 host 白名单（回归）──────────────────────────

    /// 回归：畸形 host 不得因为「含冒号」被误送进 IPv6 宽松分支而通过校验
    #[test]
    fn malformed_hosts_with_colon_are_rejected() {
        // 端口不是纯数字
        assert!(validate_base_url("https://x.com:80x/v1").is_err());
        // 端口为空
        assert!(validate_base_url("https://x.com:/v1").is_err());
        // 域名 + 非数字后缀
        assert!(validate_base_url("https://a.b:c/v1").is_err());
        // 端口越界
        assert!(validate_base_url("https://x.com:99999/v1").is_err());
    }

    /// 回归：`[::1].evil.com` 的 `.evil.com` 尾部必须参与校验，不得被静默丢弃
    #[test]
    fn ipv6_literal_tail_is_validated() {
        assert!(validate_base_url("https://[::1].evil.com/v1").is_err());
        assert!(validate_base_url("https://[::1]x/v1").is_err());
        // 缺右括号
        assert!(validate_base_url("https://[::1/v1").is_err());
        // 合法形态
        assert!(validate_base_url("https://[::1]:8080/v1").is_ok());
        assert!(validate_base_url("https://[::1]/v1").is_ok());
    }

    /// 回归：解析与校验必须给出同一个 host（预览 = 出站）
    #[test]
    fn parse_and_validate_agree_on_ipv6_host() {
        let p = parse_endpoint("https://[::1]:8080/v1").expect("应解析成功");
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.port, Some(8080));
        assert_eq!(p.base_url, "https://[::1]:8080/v1");
        assert!(validate_base_url("https://[::1].evil.com/v1").is_err());
        assert!(parse_endpoint("https://[::1].evil.com/v1").is_none());
    }

    /// 回归：IPv6 字面量后缀带非法端口必须报错
    #[test]
    fn ipv6_literal_with_bad_port_is_rejected() {
        assert!(validate_base_url("https://[::1]:80x/v1").is_err());
        assert!(validate_base_url("https://[::1]:/v1").is_err());
        assert!(validate_base_url("https://[::1]:99999/v1").is_err());
        assert!(parse_endpoint("https://[::1]:99999/v1").is_none());
        // 端口 > 65535 的具体边界值
        assert!(validate_base_url("https://[::1]:70000/v1").is_err());
    }

    /// 回归：IPv6 括号内出现非十六进制字符必须拒绝
    /// （地址本体只可能是 hexdigit，放行任意字母会让 `[gg::1]` 蒙混过关）
    #[test]
    fn ipv6_literal_with_bad_chars_is_rejected() {
        assert!(validate_base_url("https://[gg::1]/v1").is_err());
        assert!(parse_endpoint("https://[gg::1]/v1").is_none());
        // 合法形态不受影响（含 zone id）
        assert!(validate_base_url("https://[::1]/v1").is_ok());
        assert!(validate_base_url("https://[fe80::1%eth0]:8080/v1").is_ok());
    }

    /// 回归：`validate_base_url` 返回的必须是**重建后的规范串**，
    /// 使「校验的主机」与「出站的主机」严格一致
    #[test]
    fn validate_base_url_returns_rebuilt_canonical_string() {
        // 尾部斜杠被去掉
        assert_eq!(
            validate_base_url("https://api.deepseek.com/").unwrap(),
            "https://api.deepseek.com"
        );
        // 路径保留
        assert_eq!(
            validate_base_url("https://x.com/v1").unwrap(),
            "https://x.com/v1"
        );
        // 端口保留
        assert_eq!(
            validate_base_url("http://localhost:11434").unwrap(),
            "http://localhost:11434"
        );
        // 协议大小写归一
        assert_eq!(
            validate_base_url("HTTPS://x.com/v1").unwrap(),
            "https://x.com/v1"
        );
        // IPv6 保留方括号
        assert_eq!(
            validate_base_url("https://[::1]:8080/v1").unwrap(),
            "https://[::1]:8080/v1"
        );
    }

    /// 回归：常见合法地址不得被本轮收紧误伤
    #[test]
    fn validate_base_url_accepts_common_addresses() {
        assert_eq!(
            validate_base_url("https://api.openai.com").unwrap(),
            "https://api.openai.com"
        );
        assert_eq!(
            validate_base_url("http://localhost:11434").unwrap(),
            "http://localhost:11434"
        );
        assert_eq!(
            validate_base_url("https://api.deepseek.com/v1").unwrap(),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            validate_base_url("https://[::1]/v1").unwrap(),
            "https://[::1]/v1"
        );
    }

    /// base_url 契约：**不含 query / fragment** —— 任何 `?…` / `#…` 都必须被拒。
    ///
    /// 断言的是**错误文案**（不是笼统的 `is_err()`）：
    /// - 带 `?` / `#` → 命中专门分支，提示「密钥请填写在 API Key 输入框」；
    /// - 错误文案**不得回显 query 内容**（里面可能有真实凭据）；
    /// - 其它非法字符 → 仍走原有的「路径包含非法字符」。
    ///
    /// 改写原因：此前 core 曾把「合法 query 一律放行并原样保留」写成不变量
    /// （Gemini `?key=…` / Azure `?api-version=…` 是真实调用形态）。
    /// 项目负责人已裁定改为「base_url 不含 query / fragment」——
    /// base_url 会明文存进 config.json，放行 `?key=xxx` 会让 API Key 明文落盘。
    #[test]
    fn validate_base_url_rejects_query_and_fragment() {
        for bad in [
            "https://x.com/v1?key=ABC123",
            "https://x.com/v1?api-version=2024-01-01",
            "https://x.com/v1#frag",
            "https://x.com?key=ABC123",
            "https://x.com/v1?a=1#frag",
            "https://x.com/v1?mail=a@b.com",
            "https://x.com:8080?key=ABC123",
            // 含 HTML 元字符 / 空白 / 控制字符 / 畸形 fragment
            "https://x.com/v1?q=<script>alert(1)</script>",
            "https://x.com/v1?q=\"onmouseover=\"alert(1)",
            "https://x.com/v1?q='+alert(1)+'",
            "https://x.com/v1?q=><img src=x onerror=alert(1)>",
            "https://x.com/v1?q=a b",
            "https://x.com/v1?q=a\tb",
            "https://x.com/v1#a#b",
        ] {
            let err = validate_base_url(bad).unwrap_err();
            assert!(
                err.contains("不得包含查询参数或片段"),
                "带 query / fragment 的 base_url 必须被专门错误拒绝: {bad:?} → {err}"
            );
            // 错误文案不得回显 query 内容（可能含真实凭据）
            assert!(
                !err.contains("script") && !err.contains("onmouseover") && !err.contains("alert"),
                "错误文案不得回显 query 内容: {bad:?} → {err}"
            );
        }

        // 其它非法字符仍走原有文案
        let err = validate_base_url("https://x.com/<script>").unwrap_err();
        assert!(
            err.contains("路径包含非法字符"),
            "其它非法字符必须仍走「路径包含非法字符」: {err}"
        );
    }

    // ── strip_endpoint_suffix / has_endpoint_suffix ─────────────
    //
    // 回归：出站请求的接口后缀被重复拼接 → 404
    // （base_url 已含 /api/generate，出站再拼一次 → /api/generate/api/generate）

    /// 主路径：四条协议各协议的后缀都必须能被正确剥离
    #[test]
    fn strip_endpoint_suffix_table() {
        // ollama
        assert_eq!(
            strip_endpoint_suffix("http://127.0.0.1:11434/api/generate", "ollama"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            strip_endpoint_suffix("http://127.0.0.1:11434/api/chat", "ollama"),
            "http://127.0.0.1:11434"
        );
        // 已是无后缀的基础地址 → 不变
        assert_eq!(
            strip_endpoint_suffix("http://127.0.0.1:11434", "ollama"),
            "http://127.0.0.1:11434"
        );
        // anthropic
        assert_eq!(
            strip_endpoint_suffix("https://api.anthropic.com/v1/messages", "anthropic"),
            "https://api.anthropic.com"
        );
        assert_eq!(
            strip_endpoint_suffix("https://api.anthropic.com/messages", "anthropic"),
            "https://api.anthropic.com"
        );
        // openai —— **保留 /v1**（与 normalize_base_url 补 /v1 的行为一致）
        assert_eq!(
            strip_endpoint_suffix("https://api.openai.com/v1/chat/completions", "openai"),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            strip_endpoint_suffix("https://api.openai.com/v1/completions", "openai"),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            strip_endpoint_suffix("https://api.openai.com/v1/responses", "openai"),
            "https://api.openai.com/v1"
        );
        // gemini —— 只去掉 `:generateContent`，**保留 model 段**
        assert_eq!(
            strip_endpoint_suffix(
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
                "gemini"
            ),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro"
        );
    }

    /// 回归：Gemini 真实接口用的是驼峰 `:generateContent`，
    /// 常量表历史上写成全小写 → 大写/驼峰形态剥不掉 → 出站 404
    #[test]
    fn strip_endpoint_suffix_gemini_is_case_insensitive_on_method_name() {
        // 驼峰（真实接口形态）
        assert_eq!(
            strip_endpoint_suffix(
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
                "gemini"
            ),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro"
        );
        // 全小写（历史常量形态）
        assert_eq!(
            strip_endpoint_suffix(
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generatecontent",
                "gemini"
            ),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro"
        );
        // 流式接口
        assert_eq!(
            strip_endpoint_suffix(
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:streamGenerateContent",
                "gemini"
            ),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro"
        );
        assert_eq!(
            strip_endpoint_suffix(
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:streamgeneratecontent",
                "gemini"
            ),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro"
        );
        // 常量表两种形态都在
        assert!(GEMINI_SUFFIXES.contains(&":generatecontent"));
        assert!(GEMINI_SUFFIXES.contains(&":generateContent"));
        assert!(GEMINI_SUFFIXES.contains(&":streamgeneratecontent"));
        assert!(GEMINI_SUFFIXES.contains(&":streamGenerateContent"));
    }

    /// 未知协议 → 原样返回（不猜）
    #[test]
    fn strip_endpoint_suffix_unknown_protocol_returns_as_is() {
        assert_eq!(
            strip_endpoint_suffix("http://127.0.0.1:11434/api/generate", "unknown"),
            "http://127.0.0.1:11434/api/generate"
        );
        assert_eq!(
            strip_endpoint_suffix("https://api.anthropic.com/v1/messages", ""),
            "https://api.anthropic.com/v1/messages"
        );
    }

    /// 协议不匹配 → 原样返回（不得跨协议乱剥）
    #[test]
    fn strip_endpoint_suffix_does_not_cross_protocols() {
        // ollama 协议 + anthropic 后缀
        assert_eq!(
            strip_endpoint_suffix("https://api.anthropic.com/v1/messages", "ollama"),
            "https://api.anthropic.com/v1/messages"
        );
        // anthropic 协议 + ollama 后缀
        assert_eq!(
            strip_endpoint_suffix("http://127.0.0.1:11434/api/generate", "anthropic"),
            "http://127.0.0.1:11434/api/generate"
        );
        // openai 协议 + ollama 后缀
        assert_eq!(
            strip_endpoint_suffix("http://127.0.0.1:11434/api/chat", "openai"),
            "http://127.0.0.1:11434/api/chat"
        );
        // gemini 协议 + openai 后缀
        assert_eq!(
            strip_endpoint_suffix("https://api.openai.com/v1/chat/completions", "gemini"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    /// 重复后缀**只剥一层**（保守：避免吃掉用户自建的双层路径）
    #[test]
    fn strip_endpoint_suffix_strips_only_one_layer() {
        assert_eq!(
            strip_endpoint_suffix("http://x:11434/api/generate/api/generate", "ollama"),
            "http://x:11434/api/generate"
        );
        assert_eq!(
            strip_endpoint_suffix(
                "https://api.anthropic.com/v1/messages/v1/messages",
                "anthropic"
            ),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            strip_endpoint_suffix(
                "https://api.openai.com/v1/chat/completions/chat/completions",
                "openai"
            ),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    /// 带 query / fragment：**一律丢弃**（base_url 契约：不含 query / fragment）
    ///
    /// 改写原因：此测试原先断言「query / fragment 原样搬运到结果末尾」。
    /// 项目负责人已裁定 base_url **不含** query / fragment ——
    /// base_url 会明文存进 config.json，保留 `?key=xxx` 会让 API Key 明文落盘，
    /// 与本项目「API Key 走 DPAPI 加密 + 请求头传递」的安全模型冲突。
    #[test]
    fn strip_endpoint_suffix_drops_query_and_fragment() {
        // 带 query：path 部分的剥离照旧，query **丢弃**
        assert_eq!(
            strip_endpoint_suffix("http://x:11434/api/generate?a=1", "ollama"),
            "http://x:11434"
        );
        assert_eq!(
            strip_endpoint_suffix("http://x:11434/api/generate?key=abc&x=2", "ollama"),
            "http://x:11434"
        );
        // 带 fragment：同样丢弃
        assert_eq!(
            strip_endpoint_suffix("http://x:11434/api/generate#frag", "ollama"),
            "http://x:11434"
        );
        // 无后缀时 query 同样丢弃
        assert_eq!(
            strip_endpoint_suffix("http://x:11434?a=1", "ollama"),
            "http://x:11434"
        );
        // 未知协议：不剥后缀，但 query 同样丢弃
        assert_eq!(
            strip_endpoint_suffix("http://x:11434/api/generate?a=1", "unknown"),
            "http://x:11434/api/generate"
        );
        // query 内容（无论何种编码）都不得残留
        assert_eq!(
            strip_endpoint_suffix("http://x:11434/api/generate?q=a+b&r=%20", "ollama"),
            "http://x:11434"
        );
    }

    /// 空串 / 纯空白 → 原样返回空串（不得 panic）
    #[test]
    fn strip_endpoint_suffix_handles_empty_input() {
        assert_eq!(strip_endpoint_suffix("", "ollama"), "");
        assert_eq!(strip_endpoint_suffix("   ", "ollama"), "");
        assert_eq!(strip_endpoint_suffix("", "openai"), "");
        assert_eq!(strip_endpoint_suffix("", "unknown"), "");
        assert!(!has_endpoint_suffix("", "ollama"));
        assert!(!has_endpoint_suffix("   ", "ollama"));
    }

    /// 末尾斜杠：先去掉尾斜杠再判断后缀
    #[test]
    fn strip_endpoint_suffix_handles_trailing_slash() {
        assert_eq!(
            strip_endpoint_suffix("http://x:11434/api/generate/", "ollama"),
            "http://x:11434"
        );
        assert_eq!(
            strip_endpoint_suffix("http://x:11434/api/chat/", "ollama"),
            "http://x:11434"
        );
        assert_eq!(
            strip_endpoint_suffix("https://api.anthropic.com/v1/messages/", "anthropic"),
            "https://api.anthropic.com"
        );
        assert_eq!(
            strip_endpoint_suffix("https://api.openai.com/v1/chat/completions/", "openai"),
            "https://api.openai.com/v1"
        );
        // 基础地址自带尾斜杠 → 也不得以 / 结尾
        assert_eq!(
            strip_endpoint_suffix("http://127.0.0.1:11434/", "ollama"),
            "http://127.0.0.1:11434"
        );
    }

    /// 前后空白必须被 trim
    #[test]
    fn strip_endpoint_suffix_trims_whitespace() {
        assert_eq!(
            strip_endpoint_suffix("  http://127.0.0.1:11434/api/generate  ", "ollama"),
            "http://127.0.0.1:11434"
        );
    }

    /// 路径后缀大小写敏感：不得把用户真实路径误判成接口后缀
    #[test]
    fn strip_endpoint_suffix_is_case_sensitive_for_path_suffixes() {
        // /API/GENERATE 不是已知的 ollama 接口后缀 → 不剥
        assert_eq!(
            strip_endpoint_suffix("http://x:11434/API/GENERATE", "ollama"),
            "http://x:11434/API/GENERATE"
        );
        assert_eq!(
            strip_endpoint_suffix("https://api.anthropic.com/V1/Messages", "anthropic"),
            "https://api.anthropic.com/V1/Messages"
        );
    }

    /// has_endpoint_suffix 与 strip_endpoint_suffix 的结论必须一致
    #[test]
    fn has_endpoint_suffix_matches_strip_behavior() {
        // true：确实带该协议的后缀
        assert!(has_endpoint_suffix("http://127.0.0.1:11434/api/generate", "ollama"));
        assert!(has_endpoint_suffix("http://127.0.0.1:11434/api/chat", "ollama"));
        assert!(has_endpoint_suffix("https://api.anthropic.com/v1/messages", "anthropic"));
        assert!(has_endpoint_suffix("https://api.openai.com/v1/chat/completions", "openai"));
        assert!(has_endpoint_suffix(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
            "gemini"
        ));
        // 尾斜杠 / 带 query 仍然算命中
        assert!(has_endpoint_suffix("http://127.0.0.1:11434/api/generate/", "ollama"));
        assert!(has_endpoint_suffix("http://127.0.0.1:11434/api/generate?a=1", "ollama"));

        // false：纯基础地址
        assert!(!has_endpoint_suffix("http://127.0.0.1:11434", "ollama"));
        assert!(!has_endpoint_suffix("https://api.anthropic.com", "anthropic"));
        assert!(!has_endpoint_suffix("https://api.openai.com/v1", "openai"));
        assert!(!has_endpoint_suffix(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro",
            "gemini"
        ));

        // false：协议不匹配 / 未知协议
        assert!(!has_endpoint_suffix("https://api.anthropic.com/v1/messages", "ollama"));
        assert!(!has_endpoint_suffix("http://127.0.0.1:11434/api/chat", "openai"));
        assert!(!has_endpoint_suffix("http://127.0.0.1:11434/api/generate", "unknown"));
        assert!(!has_endpoint_suffix("http://127.0.0.1:11434/api/generate", ""));
    }

    /// 一致性：has_endpoint_suffix == true ⇔ strip 确实改变了字符串
    #[test]
    fn has_endpoint_suffix_agrees_with_strip_endpoint_suffix() {
        let cases = [
            ("http://127.0.0.1:11434/api/generate", "ollama"),
            ("http://127.0.0.1:11434", "ollama"),
            ("https://api.anthropic.com/v1/messages", "anthropic"),
            ("https://api.anthropic.com/messages", "anthropic"),
            ("https://api.openai.com/v1/chat/completions", "openai"),
            ("https://api.openai.com/v1", "openai"),
            (
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
                "gemini",
            ),
            (
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro",
                "gemini",
            ),
            ("http://127.0.0.1:11434/api/generate?a=1", "ollama"),
            ("http://127.0.0.1:11434/api/generate/", "ollama"),
        ];
        for (url, proto) in cases {
            let stripped = strip_endpoint_suffix(url, proto);
            let changed = stripped != url.trim();
            assert_eq!(
                has_endpoint_suffix(url, proto),
                changed,
                "判定与剥离结论应一致: url={url} proto={proto} stripped={stripped}"
            );
        }
    }

    /// 端到端：把「已含后缀的 base_url」交给 strip → 出站拼接结果唯一且正确
    #[test]
    fn stripped_base_url_produces_correct_outbound_url() {
        // ollama 出站拼接规则：{base}/api/generate
        let base = strip_endpoint_suffix("http://127.0.0.1:11434/api/generate", "ollama");
        assert_eq!(
            format!("{base}/api/generate"),
            "http://127.0.0.1:11434/api/generate"
        );
        // anthropic 出站拼接规则：{base}/v1/messages
        let base = strip_endpoint_suffix("https://api.anthropic.com/v1/messages", "anthropic");
        assert_eq!(
            format!("{base}/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        // openai 出站拼接规则：{base}/chat/completions（/v1 已由 base 保留）
        let base = strip_endpoint_suffix("https://api.openai.com/v1/chat/completions", "openai");
        assert_eq!(
            format!("{base}/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        // gemini：后缀是**方法名** `:generateContent`（不是路径段），剥离后
        // **保留 model 段** —— 服务端只需在其后补方法名，model 段不得重复拼接
        let base = strip_endpoint_suffix(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
            "gemini",
        );
        assert_eq!(
            base,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro"
        );
        assert_eq!(
            format!("{base}:generateContent"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent"
        );
    }

    // ── resolve_endpoint：出站最终 URL ──────────────────────────

    /// openai 协议：4 类输入形态都归一到正确的 chat/completions 端点
    #[test]
    fn resolve_openai_variants() {
        let m = "gpt-4o";
        let cases = [
            // 已是完整端点 → 原样返回
            (
                "https://api.openai.com/v1/chat/completions",
                "https://api.openai.com/v1/chat/completions",
            ),
            // 只有域名 → 补 /v1
            (
                "https://api.deepseek.com",
                "https://api.deepseek.com/v1/chat/completions",
            ),
            // 另一个 openai 后缀（embeddings）→ 换成 chat/completions
            (
                "https://api.openai.com/v1/embeddings",
                "https://api.openai.com/v1/chat/completions",
            ),
            // 已有 /v1 但无后缀 → 只补 chat/completions
            (
                "https://api.openai.com/v1",
                "https://api.openai.com/v1/chat/completions",
            ),
            // 网关子路径 + /v1 + 完整端点 → 原样
            (
                "https://proxy.example.com/openai/v1/chat/completions",
                "https://proxy.example.com/openai/v1/chat/completions",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(
                resolve_endpoint(input, "openai", m),
                want,
                "input={input}"
            );
        }
    }

    /// anthropic 协议：4 类输入形态都归一到 /v1/messages（不得出现 /v1/v1/messages）
    #[test]
    fn resolve_anthropic_variants() {
        let m = "claude-sonnet-4";
        let cases = [
            (
                "https://api.anthropic.com/v1/messages",
                "https://api.anthropic.com/v1/messages",
            ),
            (
                "https://api.anthropic.com",
                "https://api.anthropic.com/v1/messages",
            ),
            // 已有 /v1 → 不得拼成 /v1/v1/messages
            (
                "https://api.anthropic.com/v1",
                "https://api.anthropic.com/v1/messages",
            ),
            // 网关子路径 + 完整端点 → 原样（网关前缀不得被吃掉）
            (
                "https://gw.example.com/ai/v1/messages",
                "https://gw.example.com/ai/v1/messages",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(
                resolve_endpoint(input, "anthropic", m),
                want,
                "input={input}"
            );
        }
    }

    /// ollama 协议：3 类输入形态都归一到 /api/generate
    #[test]
    fn resolve_ollama_variants() {
        let m = "llama3";
        let cases = [
            (
                "http://127.0.0.1:11434/api/chat",
                "http://127.0.0.1:11434/api/generate",
            ),
            (
                "http://127.0.0.1:11434",
                "http://127.0.0.1:11434/api/generate",
            ),
            (
                "http://127.0.0.1:11434/api/generate",
                "http://127.0.0.1:11434/api/generate",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(resolve_endpoint(input, "ollama", m), want, "input={input}");
        }
    }

    /// gemini 协议：5 类输入形态
    ///
    /// gemini 最易错：`/models/{model}` 段与 `:generateContent` 方法名都要处理，
    /// 且**网关子路径**（`/gemini`）不得被吃掉。
    #[test]
    fn resolve_gemini_variants() {
        let m = "gemini-2.0-pro";
        let cases = [
            // 完整端点 → 原样
            (
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
            ),
            // 网关子路径 + 完整端点 → 原样（/gemini 前缀必须保留）
            (
                "https://gw.example.com/gemini/v1beta/models/gemini-2.0-pro:generateContent",
                "https://gw.example.com/gemini/v1beta/models/gemini-2.0-pro:generateContent",
            ),
            // 有 /models/ 段但无方法名 → 补方法名
            (
                "https://x.com/v1beta/models/gemini-2.0-pro",
                "https://x.com/v1beta/models/gemini-2.0-pro:generateContent",
            ),
            // 有版本段但无 /models/ → 补 models 段
            (
                "https://x.com/v1beta",
                "https://x.com/v1beta/models/gemini-2.0-pro:generateContent",
            ),
            // 无版本段 → 补 /v1beta
            (
                "https://x.com",
                "https://x.com/v1beta/models/gemini-2.0-pro:generateContent",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(resolve_endpoint(input, "gemini", m), want, "input={input}");
        }
    }

    /// 幂等性：对已解析出的最终 URL 再调一次，结果不变
    ///
    /// 出站路径会重复经过本函数（预览 → 保存 → 实际请求），
    /// 非幂等会导致 URL 被一层层追加后缀 → 404。
    #[test]
    fn resolve_endpoint_is_idempotent() {
        let cases = [
            ("https://api.openai.com/v1/chat/completions", "openai", "gpt-4o"),
            ("https://api.anthropic.com/v1/messages", "anthropic", "claude-sonnet-4"),
            ("http://127.0.0.1:11434/api/generate", "ollama", "llama3"),
            (
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
                "gemini",
                "gemini-2.0-pro",
            ),
        ];
        for (url, proto, model) in cases {
            let once = resolve_endpoint(url, proto, model);
            let twice = resolve_endpoint(&once, proto, model);
            assert_eq!(once, twice, "非幂等: proto={proto} url={url}");
        }
    }

    /// 幂等性：从「裸 base_url」出发，连调两次结果也必须一致
    #[test]
    fn resolve_endpoint_from_bare_base_is_stable() {
        let cases = [
            ("https://api.deepseek.com", "openai", "gpt-4o"),
            ("https://api.anthropic.com", "anthropic", "claude-sonnet-4"),
            ("http://127.0.0.1:11434", "ollama", "llama3"),
            ("https://x.com", "gemini", "gemini-2.0-pro"),
        ];
        for (url, proto, model) in cases {
            let once = resolve_endpoint(url, proto, model);
            let twice = resolve_endpoint(&once, proto, model);
            assert_eq!(once, twice, "非幂等: proto={proto} url={url}");
        }
    }

    /// 未知协议 → 原样返回（绝不猜测、绝不跨协议乱剥）
    #[test]
    fn resolve_endpoint_unknown_protocol_returns_as_is() {
        assert_eq!(
            resolve_endpoint("https://api.example.com/v1/chat/completions", "unknown", "m"),
            "https://api.example.com/v1/chat/completions"
        );
    }

    /// 空输入 → 空串（不产生 "null" 之类的垃圾 URL）
    #[test]
    fn resolve_endpoint_empty_input() {
        assert_eq!(resolve_endpoint("", "openai", "m"), "");
        assert_eq!(resolve_endpoint("   ", "openai", "m"), "");
    }

    /// query / fragment **一律丢弃**（base_url 契约：不含 query / fragment）
    ///
    /// 改写原因：此测试原先断言「query / fragment 原样搬运到结果末尾」。
    /// 项目负责人已裁定 base_url **不含** query / fragment ——
    /// base_url 会明文存进 config.json，保留 `?key=xxx` 会让 API Key 明文落盘，
    /// 与本项目「API Key 走 DPAPI 加密 + 请求头传递」的安全模型冲突。
    #[test]
    fn resolve_endpoint_drops_query_and_fragment() {
        // query 丢弃：结果与「同地址无 query」完全一致
        assert_eq!(
            resolve_endpoint("https://api.openai.com/v1/chat/completions?a=1", "openai", "m"),
            resolve_endpoint("https://api.openai.com/v1/chat/completions", "openai", "m")
        );
        assert_eq!(
            resolve_endpoint("https://api.openai.com/v1/chat/completions?a=1", "openai", "m"),
            "https://api.openai.com/v1/chat/completions"
        );
        // 无路径 + query：query 同样丢弃
        assert_eq!(
            resolve_endpoint("https://api.openai.com?a=1", "openai", "m"),
            "https://api.openai.com/v1/chat/completions"
        );
        // fragment 同样丢弃
        assert_eq!(
            resolve_endpoint("https://api.openai.com/v1/chat/completions#f", "openai", "m"),
            "https://api.openai.com/v1/chat/completions"
        );
        // query + fragment 一并丢弃
        let out = resolve_endpoint("https://api.openai.com/v1/chat/completions?a=1#f", "openai", "m");
        assert_eq!(out, "https://api.openai.com/v1/chat/completions");
        // 丢弃后不含任何 `?` / `#` 残留
        assert!(!out.contains('?') && !out.contains('#'));
    }

    /// 尾部斜杠与空白必须被规范化，不得产生 `//` 或 `//chat/completions`
    #[test]
    fn resolve_endpoint_normalizes_trailing_slash_and_whitespace() {
        assert_eq!(
            resolve_endpoint("  https://api.openai.com/v1/  ", "openai", "m"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            resolve_endpoint("https://api.openai.com/v1/chat/completions/", "openai", "m"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            resolve_endpoint("https://api.anthropic.com/", "anthropic", "m"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    /// gemini 的 model 段来自调用方参数（已 percent-encode），
    /// URL 自带的 model 段仅在参数为空时兜底
    #[test]
    fn resolve_gemini_uses_caller_model_and_falls_back_to_url() {
        // 调用方给了 model → 用调用方的
        assert_eq!(
            resolve_endpoint(
                "https://x.com/v1beta/models/old-name:generateContent",
                "gemini",
                "new-model"
            ),
            "https://x.com/v1beta/models/new-model:generateContent"
        );
        // 调用方 model 为空 → 用 URL 自带的
        assert_eq!(
            resolve_endpoint(
                "https://x.com/v1beta/models/url-model:generateContent",
                "gemini",
                ""
            ),
            "https://x.com/v1beta/models/url-model:generateContent"
        );
    }

    /// 回归：gemini 不能只剥 `:generateContent` 冒号段
    ///
    /// 只剥冒号段会残留 `/v1beta/models/{model}`，回填值与出站都还是错的。
    #[test]
    fn resolve_gemini_does_not_leave_residual_models_segment() {
        let out = resolve_endpoint(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
            "gemini",
            "gemini-2.0-pro",
        );
        assert_eq!(
            out,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent"
        );
        // `models/` 只能出现一次
        assert_eq!(out.matches("/models/").count(), 1, "models 段重复: {out}");
        // `:generateContent` 只能出现一次
        assert_eq!(out.matches(":generateContent").count(), 1, "方法名重复: {out}");
    }

    /// 回归：openai 端点里 `/chat/completions` 只能出现一次
    ///
    /// 出站若无条件再拼一次，就会得到
    /// `…/v1/chat/completions/v1/chat/completions` → 404。
    #[test]
    fn resolve_openai_does_not_duplicate_chat_completions() {
        for input in [
            "https://api.openai.com/v1/chat/completions",
            "https://api.openai.com/v1",
            "https://api.openai.com",
            "https://api.openai.com/v1/embeddings",
            "https://proxy.example.com/openai/v1/chat/completions",
        ] {
            let out = resolve_endpoint(input, "openai", "gpt-4o");
            assert_eq!(
                out.matches("/chat/completions").count(),
                1,
                "chat/completions 重复: input={input} out={out}"
            );
        }
    }

    /// 回归：anthropic 不得拼出 `/v1/v1/messages`
    #[test]
    fn resolve_anthropic_does_not_duplicate_v1() {
        for input in [
            "https://api.anthropic.com",
            "https://api.anthropic.com/v1",
            "https://api.anthropic.com/v1/messages",
            "https://gw.example.com/ai/v1/messages",
        ] {
            let out = resolve_endpoint(input, "anthropic", "claude-sonnet-4");
            assert!(
                !out.contains("/v1/v1/"),
                "v1 段重复: input={input} out={out}"
            );
            assert_eq!(out.matches("/v1/messages").count(), 1, "out={out}");
        }
    }

    /// 回归：ollama 不得拼出 `/api/generate/api/generate`
    #[test]
    fn resolve_ollama_does_not_duplicate_api_generate() {
        for input in [
            "http://127.0.0.1:11434",
            "http://127.0.0.1:11434/api/chat",
            "http://127.0.0.1:11434/api/generate",
        ] {
            let out = resolve_endpoint(input, "ollama", "llama3");
            assert_eq!(
                out.matches("/api/generate").count(),
                1,
                "api/generate 重复: input={input} out={out}"
            );
        }
    }

    // ── query / fragment 统一口径的终局不变量 ────────────────
    //
    // 口径：**base_url 不含 query / fragment**。
    // - `validate_base_url` 对含 `?` / `#` 的输入**一律 Err**（出站闸门）；
    // - `parse_endpoint` **剥离** tail 并 push 可见提示（回填值必须自洽）；
    // - `strip_endpoint_suffix` / `resolve_endpoint` / `normalize_base_url`
    //   **丢弃** tail（出站拼装）。
    //
    // 核心不变量：**parse 产出的 base_url 一定能通过 validate** ——
    // 否则前端「回填基础地址 → 点保存」会被写入侧闸门拒掉，用户流程走不通。

    /// 四个函数的口径统一：query / fragment 一律不出现在结果里
    #[test]
    fn query_fragment_is_dropped_consistently_across_all_entry_points() {
        let u = "https://x.com/v1/chat/completions?a=1#f";

        // 1) parse：剥离，base_url 里不得残留
        let p = parse_endpoint(u).unwrap();
        assert_eq!(p.base_url, "https://x.com/v1");
        assert!(!p.base_url.contains('?') && !p.base_url.contains('#'));

        // 2) validate：一律 Err，且错误串不回显 query 内容
        let err = validate_base_url(u).unwrap_err();
        assert!(err.contains("不得包含查询参数或片段"), "{err}");
        assert!(!err.contains("a=1") && !err.contains("f"), "错误串不得回显 query: {err}");

        // 3) strip：丢弃
        assert_eq!(strip_endpoint_suffix(u, "openai"), "https://x.com/v1");
        assert!(!strip_endpoint_suffix(u, "openai").contains('?'));

        // 4) resolve：丢弃
        let out = resolve_endpoint(u, "openai", "m");
        assert_eq!(out, "https://x.com/v1/chat/completions");
        assert!(!out.contains('?') && !out.contains('#'));

        // 5) normalize：丢弃（它只做「去尾斜杠 + 按协议补版本段」，不剥接口后缀）
        let n = normalize_base_url(u, "openai");
        assert_eq!(n, "https://x.com/v1/chat/completions");
        assert!(!n.contains('?') && !n.contains('#'));
    }

    /// 往返自洽：parse 产出的 base_url 必须能通过 validate
    ///
    /// 这是本轮最关键的不变量 —— 前端「回填基础地址」把 parse 的结果填进表单，
    /// 用户点保存时写入侧闸门（`api.rs` 的 `check_write_side_base_urls`）会调
    /// `validate_base_url`。若 parse 产出的 base_url 含 query，往返不成立，
    /// 用户流程直接走不通。
    #[test]
    fn parse_base_url_round_trips_through_validate() {
        for u in [
            "https://x.com",
            "https://x.com/",
            "https://x.com?key=abc",
            "https://x.com/v1?key=abc",
            "https://x.com/v1/chat/completions?a=1#f",
            "http://127.0.0.1:11434/api/generate?x=1",
            "https://[::1]:8080/v1/chat/completions?k=1",
        ] {
            let p = parse_endpoint(u).unwrap();
            let v = validate_base_url(&p.base_url)
                .unwrap_or_else(|e| panic!("parse 产出的 base_url 必须能通过 validate: {u:?} → {e}"));
            assert_eq!(v, p.base_url, "往返后形态不一致: {u:?} → {v:?}");
        }
    }

    /// parse 剥离 query 时必须给出**可见提示**（不得静默丢弃）
    ///
    /// 静默丢弃会让用户以为鉴权参数已带上，实际发出的是无凭据请求。
    #[test]
    fn parse_notes_when_query_is_dropped() {
        for u in [
            "https://x.com/v1?key=abc",
            "https://x.com/v1#frag",
            "https://x.com/v1/chat/completions?a=1#f",
        ] {
            let p = parse_endpoint(u).unwrap();
            assert!(
                p.notes.iter().any(|n| n.contains("查询参数")),
                "剥离 query / fragment 必须提示: {u:?} → {:?}",
                p.notes
            );
        }
        // 不含 query 时不应出现该提示（避免噪声）
        let p = parse_endpoint("https://x.com/v1").unwrap();
        assert!(
            !p.notes.iter().any(|n| n.contains("查询参数")),
            "无 query 时不该有剥离提示: {:?}",
            p.notes
        );
    }

    /// P5：openai 接口后缀段必须**归一为小写**，版本段保持用户原样
    ///
    /// 用户粘贴的路径可能是 `/V1/Chat/Completions`。真实 OpenAI 兼容服务的
    /// 接口路径是小写的 `/chat/completions` → 原样带出去会 404。
    /// 但版本段 `/V1` **保持原样** —— 它可能是网关的真实路径。
    #[test]
    fn resolve_openai_normalizes_suffix_case_but_keeps_version_segment() {
        assert_eq!(
            resolve_endpoint("https://api.openai.com/V1/Chat/Completions", "openai", "m"),
            "https://api.openai.com/V1/chat/completions"
        );
        assert_eq!(
            resolve_endpoint("https://api.openai.com/v1/CHAT/completions", "openai", "m"),
            "https://api.openai.com/v1/chat/completions"
        );
        // 版本段原样保留（不被改成小写）
        let out = resolve_endpoint("https://api.openai.com/V1/Chat/Completions", "openai", "m");
        assert!(out.contains("/V1/"), "版本段必须保持用户原样: {out}");
    }

    /// P6：叠放的接口后缀必须**收敛到单层**
    ///
    /// `…/api/generate/api/generate` 出站仍是两层 → 404。
    /// `resolve_endpoint` 是出站拼装的最后一步，职责是产出可直接发请求的 URL，
    /// 因此这里循环剥到不再命中已知后缀（区别于 `strip_endpoint_suffix`
    /// 的「只剥一层」—— 后者约束的是**配置回填**场景）。
    #[test]
    fn resolve_endpoint_converges_duplicated_suffixes() {
        assert_eq!(
            resolve_endpoint(
                "http://127.0.0.1:11434/api/generate/api/generate",
                "ollama",
                "m"
            ),
            "http://127.0.0.1:11434/api/generate"
        );
        assert_eq!(
            resolve_endpoint(
                "https://api.anthropic.com/v1/messages/v1/messages",
                "anthropic",
                "m"
            ),
            "https://api.anthropic.com/v1/messages"
        );
        // 收敛后仍幂等
        for (u, proto) in [
            (
                "http://127.0.0.1:11434/api/generate/api/generate",
                "ollama",
            ),
            (
                "https://api.anthropic.com/v1/messages/v1/messages",
                "anthropic",
            ),
            (
                "https://api.openai.com/v1/chat/completions/v1/chat/completions",
                "openai",
            ),
        ] {
            let once = resolve_endpoint(u, proto, "m");
            let twice = resolve_endpoint(&once, proto, "m");
            assert_eq!(once, twice, "收敛后必须幂等: {u:?} → {once:?} → {twice:?}");
        }
    }

    /// P8：parse 与 validate 对同一地址给出**同一形态**
    ///
    /// `https://x.com` 与 `https://x.com/` 是同一地址，但两个函数若给出不同形态，
    /// 前端回填值与实际出站值就会长得不一样。
    #[test]
    fn parse_and_validate_agree_on_canonical_form() {
        for u in [
            "https://x.com",
            "https://x.com/",
            "https://x.com/v1",
            "https://x.com/v1/",
            "http://127.0.0.1:11434",
            "https://[::1]:8080/v1",
        ] {
            let p = parse_endpoint(u).unwrap();
            let v = validate_base_url(u).unwrap();
            assert_eq!(p.base_url, v, "parse 与 validate 形态不一致: {u:?} → {p:?} vs {v:?}");
        }
    }

    /// P9：畸形 fragment（两个 `#`）必须被拒绝
    ///
    /// RFC 3986 的 fragment 不允许含 `#`；`#a#b` 是畸形输入。
    #[test]
    fn validate_base_url_rejects_malformed_fragment() {
        assert!(validate_base_url("https://x.com/v1#a#b").is_err());
        // 正常的单 fragment 同样被拒（口径：base_url 不含 query / fragment）
        assert!(validate_base_url("https://x.com/v1#a").is_err());
    }

    // ── A：path 段的 `@` 是 RFC 3986 合法字符，不得被 validate 拒绝 ──
    //
    // 核心不变量：`parse_endpoint(u).base_url` 一定能通过 `validate_base_url`。
    // 若 validate 比 parse 更严（拒绝 path 里的 `@`），前端「回填基础地址 →
    // 点保存」会被写入侧闸门拒掉 → **用户流程断裂**。

    /// path 段含 `@` 及 `pchar` 里的其它 sub-delims 时，parse 产出必须能通过 validate
    #[test]
    fn parse_base_url_round_trips_through_validate_for_all_pchar() {
        for u in [
            "https://x.com/a@b",
            "https://x.com/a@b/v1",
            "https://x.com/v1@a",
            "https://x.com/a;b/c",
            "https://x.com/a,b/c",
            "https://x.com/a=b/c",
            "https://x.com/a&b/c",
            "https://x.com/a$b/c",
            "https://x.com/a!b/c",
            "https://x.com/a*b/c",
            "https://x.com/a+b/c",
            "https://x.com/a(b)/c",
        ] {
            let b = parse_endpoint(u).unwrap().base_url;
            assert!(
                validate_base_url(&b).is_ok(),
                "parse 产出的 base_url 必须能通过 validate: {u:?} → {b:?}"
            );
        }
    }

    /// authority 段的 userinfo 防线**未被削弱**
    ///
    /// 删掉 path 段的 `@` 拒绝后，`https://user:pass@host` 必须仍被拒绝 ——
    /// 那才是 userinfo confusion 的真正风险面。
    #[test]
    fn validate_base_url_still_rejects_authority_userinfo() {
        // authority 段含 `@` → 必须拒
        assert!(validate_base_url("https://user:pass@evil.com/v1").is_err());
        assert!(validate_base_url("https://user@evil.com/v1").is_err());
        // path 段的 `@` 放行（RFC 3986 `pchar` 合法字符）
        assert!(validate_base_url("https://x.com/v1/users/@me").is_ok());
        // parse 侧同样：authority 段的 `@` 让整个地址解析失败
        assert!(parse_endpoint("https://user:pass@evil.com/v1").is_none());
    }

    // ── C：parse 的后缀识别与 has_endpoint_suffix 必须同源同口径 ──

    /// parse 的「已识别接口路径」note ⇔ 存在某协议使 `has_endpoint_suffix` ���中
    ///
    /// server 侧 `api.rs` 用 `has_endpoint_suffix` 逐协议判定，把结果作为
    /// `stripped_protocol` / `stripped_protocols` 下发前端 —— 前端据此决定
    /// 「出站会不会自动去重」。若 parse 说剥了而 has 说没剥，
    /// 前端就拿不到权威 owner，只能走兜底判据。
    #[test]
    fn parse_suffix_detection_agrees_with_has_endpoint_suffix() {
        for u in [
            "https://api.openai.com/V1/Chat/Completions",
            "https://api.openai.com/v1/chat/completions",
            "http://127.0.0.1:11434/API/GENERATE",
            "http://127.0.0.1:11434/api/generate",
            "https://api.anthropic.com/v1/messages",
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
        ] {
            let p = parse_endpoint(u).unwrap();
            let noted = p.notes.iter().any(|n| n.contains("已识别接口路径"));
            let hit = ["openai", "anthropic", "ollama", "gemini"]
                .iter()
                .any(|proto| has_endpoint_suffix(u, proto));
            assert_eq!(
                noted, hit,
                "parse 的 note 与 has_endpoint_suffix 必须一致: {u:?} → noted={noted} hit={hit}"
            );
        }
    }

    /// 合并后缀表必须等于四个单协议表的并集（不得漂移）
    ///
    /// `PARSE_SUFFIXES` 是手写的合并表 —— 若某个单协议表新增了后缀而忘了加进
    /// 合并表，parse 就认不出该接口后缀（回填值会带上接口路径）。
    #[test]
    fn parse_suffix_table_matches_single_protocol_tables() {
        let mut expected: Vec<&str> = Vec::new();
        for table in [
            OPENAI_SUFFIXES,
            ANTHROPIC_SUFFIXES,
            OLLAMA_SUFFIXES,
            GEMINI_SUFFIXES,
        ] {
            for s in table {
                if !expected.contains(s) {
                    expected.push(s);
                }
            }
        }
        for s in &expected {
            assert!(
                PARSE_SUFFIXES.contains(s),
                "合并后缀表缺少 {s:?}（单协议表新增后缀后必须同步）"
            );
        }
        for s in PARSE_SUFFIXES {
            assert!(
                expected.contains(s),
                "合并后缀表里的 {s:?} 在任何单协议表里都不存在（应移出合并表）"
            );
        }
    }

    /// Gemini 方法名后缀在 parse 与 has 两侧都命中（大小写不敏感）
    #[test]
    fn gemini_method_suffix_is_recognised_consistently() {
        for u in [
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent",
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generatecontent",
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:streamGenerateContent",
        ] {
            let p = parse_endpoint(u).unwrap();
            assert!(
                p.notes.iter().any(|n| n.contains("已识别接口路径")),
                "gemini 方法名后缀必须被 parse 识别: {u:?}"
            );
            assert!(
                has_endpoint_suffix(u, "gemini"),
                "gemini 方法名后缀必须被 has_endpoint_suffix 命中: {u:?}"
            );
        }
    }

    /// `is_version_segment`：严格语法，大小写不敏感
    ///
    /// 这是给 server 侧复用的公开判定 —— 语义必须只存在于 core 一处，
    /// 因此这里把「是」与「不是」两组样例都钉死。
    #[test]
    fn is_version_segment_accepts_only_v_number_forms() {
        for seg in [
            "v1",
            "v2",
            "v10",
            "v1beta",
            "v1beta1",
            "v3rc1",
            "v1alpha",
            "V1",
            "V1BETA",
        ] {
            assert!(is_version_segment(seg), "{seg} 应是版本段");
        }
        for seg in [
            "", "v", "1", "video", "vue", "v1proxy", "api-v1", "v1betaX", "vrc", "chat",
        ] {
            assert!(!is_version_segment(seg), "{seg} 不应是版本段");
        }
    }

    /// `has_version_segment`：只看 path 部分，且任意一段命中即可
    #[test]
    fn has_version_segment_checks_path_segments_only() {
        assert!(has_version_segment("https://x.com/v1"));
        assert!(has_version_segment("https://x.com/v1beta"));
        assert!(has_version_segment("https://x.com/gemini/v1beta/models/x"));
        assert!(has_version_segment("https://x.com/v1/chat/completions"));
        // host 部分不算版本段
        assert!(!has_version_segment("https://v1.example.com/chat"));
        assert!(!has_version_segment("https://x.com/v1proxy"));
        assert!(!has_version_segment("https://x.com"));
    }

    /// 版本段判定与 `resolve_endpoint` 的补段行为必须一致
    ///
    /// `normalize_base_url`（openai）与 `resolve_endpoint`（gemini）
    /// 都以「已带版本段就不补」为原则 —— 判定规则只出自
    /// `is_version_segment` / `has_version_segment` 这两个函数。
    /// 若二者漂移，就会出现「判定说带了 → 不补」与「判定说没带 → 补了」
    /// 的不一致，出站 URL 随之出错。
    #[test]
    fn version_segment_judgement_is_shared_with_url_assembly() {
        // openai：带 v1 段不补、不带则补
        assert_eq!(
            normalize_base_url("https://x.com/v1", "openai"),
            "https://x.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://x.com", "openai"),
            "https://x.com/v1"
        );
        // gemini：带 v1beta 段不补版本段、不带则补
        let with = resolve_endpoint("https://x.com/v1beta", "gemini", "m");
        assert_eq!(with, "https://x.com/v1beta/models/m:generateContent");
        assert!(!with.contains("/v1beta/v1beta/"), "双版本段: {with}");
        let without = resolve_endpoint("https://x.com", "gemini", "m");
        assert_eq!(without, "https://x.com/v1beta/models/m:generateContent");
        assert!(!without.contains("/v1beta/v1beta/"), "双版本段: {without}");
    }
}
