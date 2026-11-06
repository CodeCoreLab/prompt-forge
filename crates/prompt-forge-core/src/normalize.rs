//! LLM 输出规整层：三层防线中的「运行时剥离」环节。
//!
//! 对标 DSH 插件生态的共性模式（optimizer stripThinkBlocks / enhance stripWrappingQuotes）：
//! 模板纪律 → 运行时剥离 → 兜底判定。本模块实现中间层，纯函数、可独立验证。
//!
//! 管道顺序：剥 think 块 → 剥代码围栏 → 剥首尾引号 → 校验非空。
//!
//! **剥离原则（统一）**：只剥「结构完整、可确认边界」的外壳。
//! - 边界不可确认（引号不成对 / think 块未闭合）→ **原样保留**，绝不吞掉内容。
//! - 剥除后剩余内容为空 → **不接受剥除结果**，保留原文。
//!
//! 宁可留下一点噪声，也不能把模型的有效输出整段丢掉。

/// 在 `haystack` 中按字节做**大小写不敏感**查找（仅用于 ASCII 标签，如 `<think>`）。
///
/// 返回字节下标；未找到返回 `None`。
fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// 剥除 `<think>...</think>` 思考块（reasoning 模型）。
///
/// - 标签判定**大小写不敏感**：`<think>` / `<THINK>` / `</Think>` 均识别。
/// - 多块全剥。
/// - **未闭合**（只有开标签无闭标签）：无法确定块边界 → 保留剩余内容，
///   不把其后整段文本丢掉（丢掉等于让模型的有效输出凭空消失）。
pub fn strip_think_blocks(text: &str) -> String {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(open) = find_ascii_ci(rest, OPEN) else {
            out.push_str(rest);
            return out;
        };
        let after_open = &rest[open + OPEN.len()..];
        match find_ascii_ci(after_open, CLOSE) {
            Some(close) => {
                // 完整块 → 整块剥除
                out.push_str(&rest[..open]);
                rest = &after_open[close + CLOSE.len()..];
            }
            None => {
                // 未闭合：块边界不可确认 → 保留开标签之后的全部内容
                out.push_str(&rest[..open]);
                out.push_str(after_open);
                return out;
            }
        }
    }
}

/// 剥除 Markdown 代码围栏：``` 或 ```lang 开头、``` 结尾，整个结果被围栏包裹时剥壳。
/// 仅当文本以围栏开头且以围栏结尾才剥（正文内部的围栏属于提示词内容，保留）。
///
/// - 围栏不成对（只有开头没有结尾）→ 原样保留。
/// - 剥除后剩余内容为空 → 不接受剥除结果，保留原文（避免把内容剥没）。
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
        Some(inner) => {
            let inner = inner.trim();
            if inner.is_empty() {
                // 剥完就没了 → 不剥，保留原文
                t.to_string()
            } else {
                inner.to_string() // 围栏包裹 → 剥壳
            }
        }
        None => t.to_string(), // 不以 ``` 结尾 → 不是围栏包裹，不剥
    }
}

/// 引号配对表：(开引号, 闭引号)。
const QUOTE_PAIRS: [(char, char); 4] = [('"', '"'), ('\'', '\''), ('“', '”'), ('‘', '’')];

/// 剥掉首尾各一层包裹引号（ASCII + 中文引号）。`"abc"` → `abc`。
///
/// 只处理**成对**的引号：
/// - 首尾必须是同一对引号（`"`↔`"`、`'`↔`'`、`“`↔`”`、`‘`↔`’`），否则原样返回。
/// - 全文引号数量必须平衡（对称引号计数为偶数；中文引号开闭计数相等），
///   否则说明内容里的引号与外层引号错配（如 `"a" and "b`）→ 原样返回。
/// - 剥除后剩余内容必须非空，否则不接受剥除结果。
pub fn strip_wrapping_quotes(text: &str) -> &str {
    let t = text.trim();
    let Some(first) = t.chars().next() else {
        return t;
    };
    let Some((open, close)) = QUOTE_PAIRS.iter().find(|(o, _)| *o == first) else {
        return t;
    };
    let (open, close) = (*open, *close);
    let Some(last) = t.chars().last() else {
        return t;
    };
    if last != close {
        return t; // 收尾不是配对的闭引号 → 不成对，原样返回
    }
    if !quotes_balanced(t, open, close) {
        return t; // 引号数量失衡 → 内容与外层引号错配，原样返回
    }
    let inner = &t[first.len_utf8()..t.len() - last.len_utf8()];
    if inner.trim().is_empty() {
        return t; // 剥完就没了 → 不接受剥除结果
    }
    inner
}

/// 全文引号是否平衡（用于判断外层引号与内容里的引号是否错配）。
///
/// - 对称引号（`"` / `'`）：该字符出现次数必须为偶数。
/// - 中文引号（`“…”` / `‘…’`）：开引号数必须等于闭引号数。
fn quotes_balanced(text: &str, open: char, close: char) -> bool {
    let opens = text.chars().filter(|c| *c == open).count();
    let closes = text.chars().filter(|c| *c == close).count();
    if open == close {
        opens % 2 == 0
    } else {
        opens == closes
    }
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
        // 未闭合 think：边界不可确认 → 保留剩余内容，不再整段丢弃
        ("未闭合think", normalize_output("<think>只想到一半"), true),
        ("围栏", normalize_output("```\n增强结果\n```"), true),
        ("围栏带语言", normalize_output("```text\n增强结果\n```"), true),
        ("双引号", normalize_output("\"增强结果\""), true),
        ("中文引号", normalize_output("“增强结果”"), true),
        ("双层引号", normalize_output("\"\"增强结果\"\""), true),
        ("纯空白", normalize_output("   \n  "), false),
        ("正常文本", normalize_output("普通输出不动"), true),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── think 块 ────────────────────────────────────────────────

    #[test]
    fn strips_closed_think_block() {
        assert_eq!(
            strip_think_blocks("<think>推理中</think>真实输出"),
            "真实输出"
        );
        assert_eq!(strip_think_blocks("<think>a</think><think>b</think>c"), "c");
    }

    /// 回归：think 标签大小写不敏感
    #[test]
    fn think_tags_are_case_insensitive() {
        assert_eq!(strip_think_blocks("<THINK>x</THINK>y"), "y");
        assert_eq!(strip_think_blocks("<Think>x</Think>y"), "y");
        assert_eq!(strip_think_blocks("<think>x</think>y"), "y");
    }

    /// 回归：未闭合 think 块不得把剩余内容整段丢掉
    #[test]
    fn unclosed_think_keeps_remaining_content() {
        let out = strip_think_blocks("<think>只想到一半");
        assert!(out.contains("只想到一半"), "剩余内容必须保留: {out}");
        assert!(!out.contains("<think>"), "开标签本身应剥除: {out}");
    }

    #[test]
    fn unclosed_think_after_closed_block_keeps_tail() {
        let out = strip_think_blocks("<think>a</think>正文<think>b");
        assert!(out.contains("正文"), "已闭合块前的正文应保留: {out}");
        assert!(out.contains("b"), "未闭合块的内容应保留: {out}");
    }

    #[test]
    fn text_without_think_unchanged() {
        assert_eq!(strip_think_blocks("普通输出"), "普通输出");
        assert_eq!(strip_think_blocks(""), "");
    }

    // ── 代码围栏 ────────────────────────────────────────────────

    #[test]
    fn strips_wrapped_code_fence() {
        assert_eq!(strip_code_fence("```\n结果\n```"), "结果");
        assert_eq!(strip_code_fence("```text\n结果\n```"), "结果");
    }

    /// 回归：不成对围栏不得吞掉内容
    #[test]
    fn unclosed_code_fence_keeps_content() {
        let out = strip_code_fence("```\n结果");
        assert!(out.contains("结果"), "不成对围栏必须原样保留: {out}");
    }

    /// 回归：剥空等于没剥（保留原文，不返回空串）
    #[test]
    fn empty_fenced_body_is_not_stripped() {
        let out = strip_code_fence("```\n```");
        assert!(!out.is_empty(), "空围栏不得被剥成空串: {out}");
        assert!(out.contains("```"), "应保留原文: {out}");
    }

    #[test]
    fn inline_fence_is_not_stripped() {
        assert_eq!(strip_code_fence("```abc"), "```abc");
    }

    // ── 包裹引号 ────────────────────────────────────────────────

    #[test]
    fn strips_paired_quotes() {
        assert_eq!(strip_wrapping_quotes("\"增强结果\""), "增强结果");
        assert_eq!(strip_wrapping_quotes("'增强结果'"), "增强结果");
        assert_eq!(strip_wrapping_quotes("“增强结果”"), "增强结果");
        assert_eq!(strip_wrapping_quotes("‘增强结果’"), "增强结果");
    }

    /// 回归：不成对的引号不得被剥（只开不闭 / 只闭不开）
    #[test]
    fn unmatched_quotes_are_kept_as_is() {
        assert_eq!(strip_wrapping_quotes("\"只开不闭"), "\"只开不闭");
        assert_eq!(strip_wrapping_quotes("只闭不开\""), "只闭不开\"");
        // 收尾不是引号 → 不成对，原样返回
        assert_eq!(strip_wrapping_quotes("\"开头是引号但结尾不是"), "\"开头是引号但结尾不是");
    }

    /// 回归：内容里的引号与外层引号错配 → 原样返回，不得错位剥离
    #[test]
    fn mismatched_inner_quotes_are_kept_as_is() {
        // 3 个直引号 → 数量失衡 → 外层引号与内容里的引号错配
        let s = "\"前半\" 后半\"";
        assert_eq!(strip_wrapping_quotes(s), s);
        // 中文引号开闭数量不等 → 同样失衡
        let s2 = "“前半” 后半“";
        assert_eq!(strip_wrapping_quotes(s2), s2);
    }

    /// 回归：剥除后剩余为空 → 不接受剥除结果
    #[test]
    fn empty_quote_content_is_not_stripped() {
        assert_eq!(strip_wrapping_quotes("\"\""), "\"\"");
    }

    #[test]
    fn double_layer_quotes_strip_one_layer() {
        assert_eq!(strip_wrapping_quotes("\"\"增强结果\"\""), "\"增强结果\"");
    }

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(strip_wrapping_quotes("普通输出不动"), "普通输出不动");
        assert_eq!(strip_wrapping_quotes("   "), "");
    }

    // ── 管道 ────────────────────────────────────────────────────

    #[test]
    fn pipeline_strips_all_layers() {
        assert_eq!(
            normalize_output("<think>思考</think>```\n\"结果\"\n```").unwrap(),
            "结果"
        );
    }

    /// 回归：管道不得因不成对外壳而丢内容
    #[test]
    fn pipeline_keeps_content_with_unclosed_shell() {
        let out = normalize_output("<think>只想到一半").unwrap();
        assert!(out.contains("只想到一半"), "内容必须保留: {out}");
    }

    #[test]
    fn pipeline_rejects_blank_output() {
        assert!(normalize_output("   \n  ").is_err());
        assert!(normalize_output("").is_err());
    }
}
