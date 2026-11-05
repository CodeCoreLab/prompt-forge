//! prompt-forge-core — 纯逻辑层
//!
//! 设计动机：把「零 I/O 的业务逻辑」与「HTTP 服务外壳」彻底分开。
//! core 不依赖 axum/tokio，任何涉及网络、文件、密钥的操作都在 server crate；
//! 因此 `cargo test -p prompt-forge-core` 可在任意平台独立跑完，
//! 也不受 server 侧链接链（native-tls 等）影响。
//!
//! 模块：
//! - normalize 输出规整管道（think块/围栏/引号/空判定）
//! - modes   增强模式模板（COMMON 保真纪律 + 策略段 + raw_prompt 包裹）
//! - methods 增强方式与深度思考强度分级
//! - roles   角色系统（内置角色 + 自定义角色）
//! - endpoint 端点解析（URL → 协议/主机/端口/路径，接口地址回填）
//! - thinking 推理参数的协议适配与降级判定
//! - ports   端口选择纯逻辑（默认端口顺延探测 vs 显式端口精确绑定）
//! - config  LlmConfig/AppConfig 模型与校验
//! - llm     协议纯函数（endpoint 拼接/错误分类/体截断）

pub mod config;
pub mod crypto;
pub mod endpoint;
pub mod llm;
pub mod methods;
pub mod modes;
pub mod normalize;
pub mod ports;
pub mod roles;
pub mod thinking;
