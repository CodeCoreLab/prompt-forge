//! 端口选择纯逻辑。
//!
//! 背景：启动时端口（默认 8787）可能被占用。两种诉求必须同时满足：
//!
//! 1. **不破坏既有行为** —— 端口被占用时服务仍要能起来。
//!    若直接报错退出，那些依赖「服务一定能起」的自动化脚本会在端口
//!    偶发被占用时整体失败 —— 这是比「端口变了」更严重的问题。
//! 2. **不能静默** —— 端口变了却只写在启动日志里，用户会去访问错的地址，
//!    按该端口配置的防火墙规则、书签、脚本也会静默失效。
//!
//! 因此本方案的做法是：**顺延照旧，但必须喊出来**。
//!
//! - 端口被占用 → 顺延到下一个可用端口（与旧版行为一致，零破坏）
//! - stderr 输出醒目警告：原端口、实际端口、后续影响、处置建议
//! - stdout 输出机器可读的 `LISTENING <url>` 行，脚本可据此取实际端口
//! - 需要确定端口的用户显式加 `--strict-port` → 被占用即报错退出
//!
//! ⚠️ Windows 特有陷阱：socket 层 `SO_REUSEADDR` 允许**劫持**已被其他进程监听的端口，
//! 即 `bind` 可能"成功"但请求被两个进程随机接收。因此占用判定**不能依赖 bind 是否失败**，
//! 必须先用 connect 主动探测（见 server 侧 `port_in_use`）；本模块只负责候选序列与文案。

/// 端口号上限（含）
pub const MAX_PORT: u16 = u16::MAX;

/// 端口号下限（含）。
///
/// 端口 0 在 socket 语义里表示「由系统随机分配」，**不能**当作固定监听端口：
/// 静默接受会让用户以为服务监听在 0 端口，实际监听号随机、无法访问。
pub const MIN_PORT: u16 = 1;

/// 端口号是否可作为固定监听端口。
///
/// 0 = 由系统随机分配 → 不可作为固定端口；其余（1..=65535）均可。
pub fn is_valid_port(port: u16) -> bool {
    port >= MIN_PORT
}

/// 校验端口号合法性；非法时给出明确的中文原因。
///
/// - 0 → 非法（系统随机分配端口，不能显式监听）
/// - \> 65535 → 非法（超出 TCP/UDP 端口范围）
pub fn validate_port(port: u16) -> Result<(), String> {
    if port < MIN_PORT {
        return Err(format!(
            "端口号非法: {port}（0 表示由系统随机分配，不能作为固定监听端口）"
        ));
    }
    Ok(())
}

/// 解析命令行 / 配置里的端口字符串。
///
/// 覆盖全部非法输入：
/// - 非数字（`abc`）→ 报错
/// - 负数（`-1`）→ 报错
/// - 0 → 报错（系统随机分配端口）
/// - \> 65535（`65536`）→ 报错（超出端口范围）
/// - 空串 → 报错
pub fn parse_port(raw: &str) -> Result<u16, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("端口不能为空".into());
    }
    // 先按 u32 解析，才能把 65536 这类超出 u16 范围的值区分为「超范围」而非「格式错误」
    let n: u32 = s
        .parse()
        .map_err(|_| format!("端口格式错误: {raw}（应为 1-65535 的整数）"))?;
    if n == 0 {
        return Err("端口号非法: 0（0 表示由系统随机分配，不能作为固定监听端口）".into());
    }
    if n > MAX_PORT as u32 {
        return Err(format!("端口号越界: {n}（上限 {MAX_PORT}）"));
    }
    Ok(n as u16)
}

/// 生成端口候选序列：从 `start` 起递增，最多 `max` 个候选。
///
/// 与旧实现的差异：旧实现在 `start + offset` 溢出时用 `saturating_add` 固化为
/// `u16::MAX`，导致接近上限时**重复尝试同一个端口**（无效重试，且错误信息误导）。
/// 本实现越过 `MAX_PORT` 即停止，保证候选端口严格递增且互不相同。
///
/// `start` 必须是合法监听端口（`is_valid_port`）；传 0（系统随机分配语义）
/// 时返回空序列 —— 与其静默产出一个语义错误的候选，不如让调用方显式失败。
pub fn probe_sequence(start: u16, max: u16) -> Vec<u16> {
    let mut out = Vec::new();
    if max == 0 {
        return out;
    }
    if !is_valid_port(start) {
        return out;
    }
    // 用 while 而非 `for _ in 0..max`：端口必须在到达 `MAX_PORT` 时停下，
    // 而 `start + 偏移` 在接近上限时会溢出 u16。
    let mut port = start;
    while out.len() < max as usize {
        out.push(port);
        if port == MAX_PORT {
            break;
        }
        port += 1;
    }
    out
}
/// 端口被占用、实际顺延到 `actual` 时的警告文案。
///
/// 必须同时说清三件事：**发生了什么**（原端口 → 实际端口）、
/// **有什么后果**（按原端口配置的规则会失效）、**怎么办**（两种处置路径）。
pub fn occupied_notice(original: u16, actual: u16) -> String {
    format!(
        "警告: 端口 {original} 已被其他进程监听，服务已自动顺延到 {actual}\n  \
         影响: 按端口 {original} 配置的防火墙规则、书签与自动化脚本不会自动跟随，\
         需改为 {actual}\n  \
         排查: netstat -ano | findstr :{original}\n  \
         处置: 释放端口 {original} 后重试；或用 --port {actual} 固定端口；\
         或加 --strict-port 使端口被占用时直接报错退出"
    )
}

/// 严格模式（`--strict-port`）下端口被占用的错误文案。
pub fn strict_port_error(port: u16) -> String {
    // `port >= MAX_PORT` 对 u16 恒等价于 `port == MAX_PORT`（MAX_PORT 即 u16::MAX），
    // 写成 `==` 以避免 clippy 的 absurd_extreme_comparisons（deny 级）。
    let advice = if port == MAX_PORT {
        "处置: 先释放该端口（已无可用替代端口）".to_string()
    } else {
        format!("处置: 改用其他端口（--port {}），或先释放该端口", port + 1)
    };
    format!(
        "端口 {port} 已被其他进程监听（--strict-port 要求严格使用该端口，不顺延）\n  \
         排查: netstat -ano | findstr :{port}\n  \
         {advice}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_starts_at_start_and_increments() {
        assert_eq!(probe_sequence(8787, 3), vec![8787, 8788, 8789]);
    }

    #[test]
    fn sequence_respects_max_count() {
        assert_eq!(probe_sequence(8787, 0), Vec::<u16>::new());
        assert_eq!(probe_sequence(8787, 1), vec![8787]);
    }

    #[test]
    fn sequence_stops_at_max_port_without_duplicates() {
        let seq = probe_sequence(65533, 10);
        assert_eq!(seq, vec![65533, 65534, 65535]);
        // 关键回归：旧实现 saturating_add 会让 65535 重复出现
        assert_eq!(seq.iter().filter(|&&p| p == 65535).count(), 1);
        let mut sorted = seq.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), seq.len());
    }

    /// 端口 0 = 系统随机分配语义，不能作为固定监听端口 → 不产出候选
    #[test]
    fn sequence_from_zero_port_is_empty() {
        assert_eq!(probe_sequence(0, 3), Vec::<u16>::new());
        assert!(!is_valid_port(0));
        assert!(validate_port(0).is_err());
        assert!(validate_port(0).unwrap_err().contains("0"));
    }

    // ── 端口合法性 ──────────────────────────────────────────────

    #[test]
    fn port_bounds_are_explicit() {
        // 1 与 65535 均合法
        assert!(is_valid_port(1));
        assert!(is_valid_port(MAX_PORT));
        assert!(validate_port(1).is_ok());
        assert!(validate_port(MAX_PORT).is_ok());
        // 0 非法
        assert!(!is_valid_port(0));
        assert!(validate_port(0).is_err());
    }

    #[test]
    fn parse_port_accepts_valid_values() {
        assert_eq!(parse_port("8787").unwrap(), 8787);
        assert_eq!(parse_port(" 8787 ").unwrap(), 8787);
        assert_eq!(parse_port("1").unwrap(), 1);
        assert_eq!(parse_port("65535").unwrap(), 65535);
    }

    #[test]
    fn parse_port_rejects_zero() {
        let e = parse_port("0").unwrap_err();
        assert!(e.contains('0'), "应指出 0 非法: {e}");
    }

    #[test]
    fn parse_port_rejects_out_of_range() {
        let e = parse_port("65536").unwrap_err();
        assert!(e.contains("65536"), "应指出越界值: {e}");
    }

    #[test]
    fn parse_port_rejects_non_numeric_and_negative() {
        assert!(parse_port("abc").is_err());
        assert!(parse_port("-1").is_err());
        assert!(parse_port("").is_err());
        assert!(parse_port("   ").is_err());
        assert!(parse_port("80.5").is_err());
    }

    /// 不变量：凡 `parse_port` 放行的端口，必然同时满足
    /// 「是合法监听端口」且「能产出至少一个候选」。
    ///
    /// 防止三处规则（`parse_port` / `is_valid_port` / `probe_sequence`）将来各自演进后漂移 ——
    /// 例如有人放宽 `parse_port` 让它接受 0，而 `probe_sequence` 仍返回空序列，
    /// 用户就会看到「起了 N 个候选均不可用」这种查不出原因的错误。
    #[test]
    fn every_parsable_port_yields_a_probe_candidate() {
        for raw in ["1", "80", "8787", "65535", " 8787 "] {
            let p = parse_port(raw).unwrap_or_else(|e| panic!("{raw} 应可解析: {e}"));
            assert!(is_valid_port(p), "{raw} → {p} 必须是合法监听端口");
            assert!(
                validate_port(p).is_ok(),
                "{raw} → {p} 必须通过 validate_port"
            );
            assert_eq!(
                probe_sequence(p, 1),
                vec![p],
                "{raw} → {p} 必须至少产出一个候选"
            );
        }
    }

    /// 不变量：`parse_port("0")` 与 `validate_port(0)` 对「0 非法」给出**同一套措辞**。
    ///
    /// 两处文案分开维护，很容易一个改了另一个没改，用户从不同入口会看到互相矛盾的解释。
    #[test]
    fn zero_is_rejected_consistently_by_both_apis() {
        let from_parse = parse_port("0").unwrap_err();
        let from_validate = validate_port(0).unwrap_err();
        for key in ["0", "随机分配"] {
            assert!(
                from_parse.contains(key),
                "parse_port 文案应含「{key}」: {from_parse}"
            );
            assert!(
                from_validate.contains(key),
                "validate_port 文案应含「{key}」: {from_validate}"
            );
        }
    }

    #[test]
    fn occupied_notice_states_original_and_actual_port() {
        let msg = occupied_notice(8787, 8788);
        assert!(msg.contains("8787"), "应含原端口: {msg}");
        assert!(msg.contains("8788"), "应含实际端口: {msg}");
    }

    #[test]
    fn occupied_notice_states_impact_and_both_remedies() {
        let msg = occupied_notice(8787, 8788);
        // 后果必须说明：按原端口配置的规则会失效
        assert!(msg.contains("防火墙"), "应说明影响: {msg}");
        // 两条处置路径：释放端口 / 用 --port 固定端口 / --strict-port
        assert!(msg.contains("--port 8788"), "应给出固定端口方案: {msg}");
        assert!(msg.contains("--strict-port"), "应给出严格模式方案: {msg}");
    }

    #[test]
    fn strict_error_names_port_and_suggests_alternative() {
        let msg = strict_port_error(8899);
        assert!(msg.contains("8899"));
        assert!(msg.contains("--port 8900"));
        assert!(msg.contains("不顺延"));
    }

    #[test]
    fn strict_error_at_max_port_has_no_self_reference() {
        let msg = strict_port_error(MAX_PORT);
        assert!(msg.contains("已无可用替代端口"));
        assert!(!msg.contains("--port 65535"));
    }
}
