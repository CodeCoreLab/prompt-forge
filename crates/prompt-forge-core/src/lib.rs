//! prompt-forge-core — 纯逻辑层
//!
//! 设计动机：主 crate 依赖 tauri → 测试 exe 链 WebView2Loader →
//! windows-gnu + 沙箱环境下 test harness 起不来（STATUS_ENTRYPOINT_NOT_FOUND）。
//! 本 crate 零 Tauri 依赖，`cargo test -p prompt-forge-core` 不触发 WebView2 链接。
//!
//! 模块：
//! - normalize 输出规整管道（think块/围栏/引号/空判定）
//! - modes   增强模式模板（COMMON 保真纪律 + 策略段 + raw_prompt 包裹）
//! - methods 增强方式与深度思考强度分级
//! - roles   角色系统（内置角色 + 自定义角色）
//! - endpoint 端点解析（URL → 协议/主机/端口/路径，接口地址回填）
//! - thinking 推理参数的协议适配与降级判定
//! - config  LlmConfig/AppConfig 模型与校验
//! - llm     协议纯函数（endpoint 拼接/错误分类/体截断）

pub mod config;
pub mod crypto;
pub mod endpoint;
pub mod llm;
pub mod methods;
pub mod modes;
pub mod normalize;
pub mod roles;
pub mod thinking;
