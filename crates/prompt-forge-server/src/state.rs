//! 服务端共享状态。
//!
//! - `cancel`：当前增强轮的取消令牌槽位（新一轮替换前先取消旧轮，与历史版本语义一致）
//! - `config_lock`：配置读改写互斥锁，防止并发保存导致写覆盖

use std::sync::RwLock;

use tokio_util::sync::CancellationToken;

/// 应用共享状态
#[derive(Clone)]
pub struct AppState {
    /// 当前生效的取消令牌（RwLock 而非 Mutex：读多写少，且取消路径必须无阻塞）
    pub cancel: std::sync::Arc<RwLock<CancellationToken>>,
    /// 配置互斥锁
    pub config_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            cancel: std::sync::Arc::new(RwLock::new(CancellationToken::new())),
            config_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// 触发取消（幂等：可反复调用）
    pub fn trigger_cancel(&self) {
        if let Ok(token) = self.cancel.read() {
            token.cancel();
        }
    }

    /// 开始新一轮增强：先废弃旧轮次，再登记新令牌，返回新令牌
    pub fn begin_enhance(&self) -> CancellationToken {
        let new_token = CancellationToken::new();
        if let Ok(mut slot) = self.cancel.write() {
            slot.cancel(); // 废弃在途轮次
            *slot = new_token.clone();
        }
        new_token
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
