//! prompt-forge-core — 业务逻辑层
//!
//! 设计动机：把「业务逻辑」与「HTTP 服务外壳」彻底分开。
//! core 不依赖 axum/tokio，任何涉及网络、文件的操作都在 server crate；
//! 因此 `cargo test -p prompt-forge-core` 可在任意平台独立跑完，
//! 也不受 server 侧链接链（native-tls 等）影响。
//!
//! ## 分层边界（准确描述）
//!
//! - **不做文件 I/O**：core 不读写任何文件，不含 `std::fs` 调用；
//!   配置的加载/保存、原子写、损坏留档全部在 server crate（`store.rs`）。
//! - **不做网络 I/O**：core 不发起任何 HTTP 请求，不含网络客户端依赖；
//!   出站协议适配在 server crate（`llm.rs`）。
//! - **不做日志 I/O**：core 不写 stdout/stderr，运行期告警统一进
//!   `crypto::drain_warnings()` 缓冲。缓冲的**读取与输出由 server 层负责**
//!   （server 需在合适时机调用 `crypto::drain_warnings()` 取走并打印，
//!   否则告警既不呈现给用户，也会滞留在缓冲里）。
//! - **例外：Windows DPAPI**（`crypto` 模块）——为 API Key 加密会动态加载
//!   `crypt32.dll` 调用系统 API（`CryptProtectData` / `CryptUnprotectData`）。
//!   这属于**操作系统副作用**（密钥由用户会话密钥保护），但**不涉及文件 I/O**
//!   （密文仍由 server 层写入磁盘）。非 Windows 平台该模块为明文透传。
//!
//! 模块：
//! - normalize 输出规整管道（think块/围栏/引号/空判定）
//! - modes   增强模式模板（COMMON 保真纪律 + 策略段 + raw_prompt 包裹）
//! - methods 增强方式与深度思考强度分级（含按协议的推理预算上限收敛）
//! - roles   角色系统（内置角色 + 自定义角色）
//! - endpoint 端点解析（URL → 协议/主机/端口/路径，接口地址回填）
//!   与**出站 URL 归一**（`resolve_endpoint`：把用户填的 base_url
//!   —— 可能是完整端点 URL —— 解析成该协议的最终请求 URL；
//!   版本段判定 `has_version_segment` / `is_version_segment` 也在此处，
//!   供 server 侧复用同一套规则，避免同一语义在两个 crate 各写一份）
//! - thinking 推理参数的协议适配与降级判定
//! - ports   端口选择纯逻辑（默认端口顺延探测 vs 显式端口精确绑定）
//! - config  LlmConfig/AppConfig 模型与校验
//! - llm     协议纯函数（endpoint 拼接/错误分类/体截断）
//! - crypto  API Key 加解密（Windows DPAPI；OS 副作用，无文件 I/O）
//! - mime    Content-Type 映射（单一真源，build.rs 与运行时共用）

pub mod config;
pub mod crypto;
pub mod endpoint;
pub mod llm;
pub mod methods;
pub mod mime;
pub mod modes;
pub mod normalize;
pub mod ports;
pub mod roles;
pub mod thinking;
