//! LLM 输出规整层：三层防线中的「运行时剥离」环节。
//!
//! 对标 DSH 插件生态的共性模式（optimizer stripThinkBlocks / enhance stripWrappingQuotes）：
//! 模板纪律 → 运行时剥离 → 兜底判定。本模块实现中间层，纯函数、可独立验证。
//!
//! 管道顺序：剥 think 块 → 剥代码围栏 → 剥首尾引号 → 校验非空。

/// 剥除 `<think>...</think>` 思考块（reasoning 模型）。
/// 多块全剥；未闭合的块延伸到结尾，一并剥除（后面没有真实输出）。
pub fn strip_think_blocks(text: &str) -> String {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        match rest.find(OPEN) {
            None => {
                out.push_str(rest);
                return out;
            }
            Some(open) => {
                out.push_str(&rest[..open]);
                let after_open = &rest[open + OPEN.len()..];
                match after_open.find(CLOSE) {
                    None => return out, // 未闭合：思考延伸到结尾
                    Some(close) => {
                        rest = &after_open[close + CLOSE.len()..];
                    }
                }
            }
        }
    }
}

/// 剥除 Markdown 代码围栏：``` 或 ```lang 开头、``` 结尾，整个结果被围栏包裹时剥壳。
/// 仅当文本以围栏开头且以围栏结尾才剥（正文内部的围栏属于提示词内容，保留）。
pub fn strip_code_fence(text: &str) -> String {
    let t = text.trim();
    let Some(body) = t.strip_prefix("```") else {
        return t.to_string();
    };
    // 跳过语言标注行
    let body = match body.find('\n') {
        Some(i) => &body[i + 1..],
        None => return t.to_string(), // ```xxx 无闭合，属正文，不剥
    };
    match body.strip_suffix("```") {
        Some(inner) => inner.trim().to_string(), // 围栏包裹 → 剥壳
        None => t.to_string(),                   // 不以 ``` 结尾 → 不是围栏包裹，不剥
    }
}

/// 剥掉首尾各一层包裹引号（ASCII + 中文引号）。`""abc""` → `"abc"`。
pub fn strip_wrapping_quotes(text: &str) -> &str {
    const QUOTES: [char; 6] = ['"', '\'', '“', '”', '‘', '’'];
    let t = text.trim();
    let start_stripped = t.strip_prefix(|c: char| QUOTES.contains(&c)).unwrap_or(t);
    // 只剥一层：从剥过头的左端再剥右端
    start_stripped
        .strip_suffix(|c: char| QUOTES.contains(&c))
        .unwrap_or(start_stripped)
}

/// 完整规整管道。返回 Err 表示剥离后为空（判定为失败结果）。
pub fn normalize_output(raw: &str) -> Result<String, String> {
    let no_think = strip_think_blocks(raw);
    let no_fence = strip_code_fence(&no_think);
    let text = strip_wrapping_quotes(&no_fence);
    if text.trim().is_empty() {
        return Err("empty_result:模型返回空白内容".into());
    }
    Ok(text.trim().to_string())
}

// ── 自检用例（--selfcheck 走这里）─────────────────────────────

#[cfg(test)]
pub fn selfcheck_cases() -> Vec<(&'static str, Result<String, String>, bool)> {
    vec![
        ("think块", normalize_output("<think>推理中</think>真实输出"), true),
        ("未闭合think", normalize_output("<think>只想到一半"), false),
        ("围栏", normalize_output("```\n增强结果\n```"), true),
        ("围栏带语言", normalize_output("```text\n增强结果\n```"), true),
        ("双引号", normalize_output("\"增强结果\""), true),
        ("中文引号", normalize_output("“增强结果”"), true),
        ("双层引号", normalize_output("\"\"增强结果\"\""), true),
        ("纯空白", normalize_output("   \n  "), false),
        ("正常文本", normalize_output("普通输出不动"), true),
    ]
}
