//! prompt-forge-core — 纯逻辑层
//!
//! 设计动机：主 crate 依赖 tauri → 测试 exe 链 WebView2Loader →
//! windows-gnu + 沙箱环境下 test harness 起不来（STATUS_ENTRYPOINT_NOT_FOUND）。
//! 本 crate 零 Tauri 依赖，`cargo test -p prompt-forge-core` 不触发 WebView2 链接。
//!
//! 模块：
//! - normalize 输出规整管道（think块/围栏/引号/空判定）
//! - modes   增强模式模板（COMMON 保真纪律 + 策略段 + raw_prompt 包裹）
//! - config  LlmConfig/AppConfig 模型与校验
//! - llm     协议纯函数（endpoint 拼接/错误分类/体截断）

pub mod config;
pub mod crypto;
pub mod llm;
pub mod modes;
pub mod normalize;
