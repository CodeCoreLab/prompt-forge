//! 端口选择纯逻辑。
//!
//! 背景：启动时默认端口（8787）可能被占用，此时向后顺延是"开箱即用"的合理行为；
//! 但**用户显式指定** `--port N` 时，静默顺延到别的端口会破坏其明确意图 ——
//! 按该端口配置的防火墙规则、书签、自动化脚本全部失效，且提示只在启动日志里，
//! 用户几乎不会看到。因此两种场景必须区别对待：
//!
//! - 默认端口 → 顺延探测（`probe_sequence` 给出候选序列）
//! - 显式端口 → 精确绑定，失败即报错（`explicit_port_error`）
//!
//! ⚠️ Windows 特有陷阱：socket 层 `SO_REUSEADDR` 允许**劫持**已被其他进程监听的端口，
//! 即 `bind` 可能"成功"但请求被两个进程随机接收。因此占用判定**不能依赖 bind 是否失败**，
//! 必须先用 connect 主动探测（见 server 侧 `port_in_use`）；本模块只负责候选序列与决策。

/// 端口号上限（含）
pub const MAX_PORT: u16 = u16::MAX;

/// 生成端口候选序列：从 `start` 起递增，最多 `max` 个候选。
///
/// 与旧实现的差异：旧实现在 `start + offset` 溢出时用 `saturating_add` 固化为
/// `u16::MAX`，导致接近上限时**重复尝试同一个端口**（无效重试，且错误信息误导）。
/// 本实现越过 `MAX_PORT` 即停止，保证候选端口严格递增且互不相同。
pub fn probe_sequence(start: u16, max: u16) -> Vec<u16> {
    let mut out = Vec::new();
    if max == 0 {
        return out;
    }
    let mut port = start;
    for _ in 0..max {
        out.push(port);
        if port == MAX_PORT {
            break;
        }
        port += 1;
    }
    out
}

/// 显式指定端口被占用时的完整处置建议。
pub fn explicit_port_error(port: u16) -> String {
    let advice = if port >= MAX_PORT {
        "处置: 先释放该端口（已无可用替代端口）".to_string()
    } else {
        format!("处置: 改用其他端口（--port {}），或先释放该端口", port + 1)
    };
    format!(
        "端口 {port} 已被其他进程监听（--port 为显式指定，不再自动顺延）\n  \
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

    #[test]
    fn sequence_from_zero_port() {
        assert_eq!(probe_sequence(0, 3), vec![0, 1, 2]);
    }

    #[test]
    fn explicit_error_names_port_and_suggests_alternative() {
        let msg = explicit_port_error(8899);
        assert!(msg.contains("8899"));
        assert!(msg.contains("--port 8900"));
        assert!(msg.contains("不再自动顺延"));
    }

    #[test]
    fn explicit_error_at_max_port_has_no_self_reference() {
        let msg = explicit_port_error(MAX_PORT);
        assert!(msg.contains("已无可用替代端口"));
        assert!(!msg.contains("--port 65535"));
    }
}
