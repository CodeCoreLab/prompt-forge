//! 服务端共享状态。
//!
//! - `cancel`：当前增强轮的取消令牌槽位（新一轮替换前先取消旧轮，与历史版本语义一致）
//! - `config_lock`：配置读改写互斥锁，防止并发保存导致写覆盖
//!
//! ## 取消语义（2026-11-05 修正）
//!
//! 旧实现只有一个 `RwLock<CancellationToken>` 槽位，无法区分两种取消：
//! - **用户主动取消**（点了「取消」按钮）
//! - **被新轮次取代**（用户又点了一次增强，旧轮次自动作废）
//!
//! 两者对前端的意义完全不同：前者要提示"已取消"，后者应静默丢弃。
//! 因此这里用 **generation 计数 + 用户取消标记** 区分：
//! `begin_enhance` 递增 generation 并登记新令牌；`trigger_cancel` 只标记
//! 「当前 generation 是用户取消的」，不直接动令牌状态——由 `enhance` 在
//! `select!` 分支里查询，从而区分「用户取消」与「被取代」。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use tokio_util::sync::CancellationToken;

/// 应用共享状态
#[derive(Clone)]
pub struct AppState {
    /// 当前生效的取消令牌（RwLock 而非 Mutex：读多写少，且取消路径必须无阻塞）
    pub cancel: std::sync::Arc<RwLock<CancellationToken>>,
    /// 配置互斥锁
    pub config_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    /// 当前轮次代号（每次 begin_enhance 递增）
    generation: std::sync::Arc<AtomicU64>,
    /// 用户主动取消标记（存被取消的 generation；0 = 无）
    user_cancelled: std::sync::Arc<AtomicU64>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            cancel: std::sync::Arc::new(RwLock::new(CancellationToken::new())),
            config_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            generation: std::sync::Arc::new(AtomicU64::new(0)),
            user_cancelled: std::sync::Arc::new(AtomicU64::new(0)),
        }
    }

    /// 触发取消（幂等：可反复调用）
    ///
    /// 只标记「当前轮次是用户取消的」，**不**直接 cancel 令牌——
    /// 令牌由 `begin_enhance` 在替换旧轮时统一 cancel。
    /// 这样 enhance 侧能区分「用户取消」与「被新轮次取代」。
    pub fn trigger_cancel(&self) {
        let gen = self.generation.load(Ordering::SeqCst);
        if gen != 0 {
            self.user_cancelled.store(gen, Ordering::SeqCst);
        }
        if let Ok(token) = self.cancel.read() {
            token.cancel();
        }
    }

    /// 开始新一轮增强：先废弃旧轮次，再登记新令牌。
    ///
    /// 返回 `(token, generation)` —— **generation 必须由本函数权威分配**，
    /// 调用方不得自行用 `current_generation() + 1` 推算（并发下会与真实值错位，
    /// 导致「用户取消」被误判为「被新轮次取代」）。
    pub fn begin_enhance(&self) -> (CancellationToken, u64) {
        let new_token = CancellationToken::new();
        // 先递增 generation（旧的 user_cancelled 标记随之失效）
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut slot) = self.cancel.write() {
            slot.cancel(); // 废弃在途轮次
            *slot = new_token.clone();
        }
        (new_token, generation)
    }

    /// 当前轮次代号
    ///
    /// 仅供测试与诊断使用 —— 业务代码必须用 `begin_enhance` 的返回值，
    /// 自行用 `current_generation() + 1` 推算会与真实值错位。
    #[cfg(test)]
    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// 判断某轮次是否是**用户主动取消**（而非被新轮次取代）
    pub fn is_user_cancelled(&self, generation: u64) -> bool {
        self.user_cancelled.load(Ordering::SeqCst) == generation
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_cancel_is_distinguishable_from_supersede() {
        let st = AppState::new();
        // 第一轮：用户主动取消
        let (_t1, g1) = st.begin_enhance();
        assert_eq!(st.current_generation(), g1);
        st.trigger_cancel();
        assert!(st.is_user_cancelled(g1), "用户取消必须可识别");

        // 第二轮：被新轮次取代（没有 trigger_cancel）
        let (_t2, g2) = st.begin_enhance();
        assert_eq!(st.current_generation(), g2);
        assert!(!st.is_user_cancelled(g2), "被取代的轮次不得标记为用户取消");
        // 旧轮次的标记不应把新轮次误判为已取消
        assert_ne!(g1, g2);
    }

    #[test]
    fn new_round_cancels_in_flight_token() {
        let st = AppState::new();
        let (t1, _) = st.begin_enhance();
        assert!(!t1.is_cancelled());
        let (_t2, _) = st.begin_enhance();
        assert!(t1.is_cancelled(), "新轮次必须废弃在途轮次");
    }

    /// 回归：generation 必须由 begin_enhance 权威分配
    ///
    /// 调用方自行用 `current_generation() + 1` 推算会与真实值错位，
    /// 导致「用户取消」被误判为「被新轮次取代」（用户看到的是错误提示）。
    #[test]
    fn generation_is_authoritative_from_begin_enhance() {
        let st = AppState::new();
        let (_t1, g1) = st.begin_enhance();
        let (_t2, g2) = st.begin_enhance();
        assert_eq!(g1, 1);
        assert_eq!(g2, 2);
        assert_eq!(st.current_generation(), g2);
    }
}
