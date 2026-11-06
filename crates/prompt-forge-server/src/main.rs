//! Prompt Forge HTTP 服务入口。
//!
//! 用法：
//! ```text
//! prompt-forge [--port <端口>] [--strict-port] [--no-browser] [--web-dir <目录>]
//! ```
//!
//! - `--port`      监听端口，默认 8787
//! - `--strict-port` 严格模式：端口被占用时直接报错退出，不顺延
//! - `--no-browser` 启动后不自动打开浏览器
//! - `--web-dir`   从指定目录读取前端资源（开发期热更）；缺省用内嵌资源
//!
//! 环境变量：
//! - `PROMPT_FORGE_CONFIG_DIR` 覆盖配置目录（优先于平台默认路径；空值视为未设置）
//!
//! 端口被占用时的默认行为：**顺延到下一个可用端口并醒目告警**（与旧版一致，不破坏
//! 依赖「服务一定能起」的脚本）。需要确定端口的用户加 `--strict-port`。
//!
//! 行为对齐历史桌面版：启动即拉起本地服务 + 打开浏览器访问 UI。

mod api;
mod autostart;
mod llm;
mod paths;
mod security;
mod state;
mod static_files;
mod store;

use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::net::TcpListener;

use crate::state::AppState;

/// 默认端口
const DEFAULT_PORT: u16 = 8787;
/// 端口被占用时向后试探的最大次数
const PORT_PROBE_MAX: u16 = 20;

/// 命令行参数
struct Args {
    port: u16,
    /// 严格模式：端口被占用必须报错退出，不顺延。
    ///
    /// 默认关闭 —— 顺延能让服务在端口偶发被占用时照样起来，
    /// 依赖「服务一定能起」的自动化脚本不会因此整体失败。
    strict_port: bool,
    open_browser: bool,
    web_dir: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        port: DEFAULT_PORT,
        strict_port: false,
        open_browser: true,
        web_dir: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--port" => {
                let v = it.next().ok_or("--port 需要端口值")?;
                // 端口校验下沉到 core 的 parse_port：拒绝 0（系统随机分配语义，
                // 静默接受会让用户以为服务监听在 0 端口，实际监听号随机且无法访问）、
                // 拒绝 > 65535、拒绝非数字。
                args.port = prompt_forge_core::ports::parse_port(&v)?;
            }
            "--strict-port" => args.strict_port = true,
            "--no-browser" => args.open_browser = false,
            "--web-dir" => {
                let v = it.next().ok_or("--web-dir 需要目录路径")?;
                args.web_dir = Some(PathBuf::from(v));
            }
            "--help" | "-h" => {
                println!("Prompt Forge HTTP 服务\n");
                println!("用法: prompt-forge [--port <端口>] [--strict-port] [--no-browser] [--web-dir <目录>]");
                println!("  --port <端口>   监听端口（默认 {DEFAULT_PORT}）");
                println!("  --strict-port   严格模式：端口被占用时直接报错退出，不顺延");
                println!("  --no-browser    启动后不自动打开浏览器");
                println!("  --web-dir <目录> 从目录读取前端资源（开发热更）");
                println!("\n说明: 端口被占用时默认自动顺延到下一个可用端口，并在 stderr 告警、");
                println!("      stdout 输出 LISTENING <地址> 行（脚本可据此取实际端口）。");
                println!("\n环境变量:");
                println!(
                    "  {}  覆盖配置目录（优先于平台默认路径；空值视为未设置）",
                    paths::CONFIG_DIR_ENV
                );
                std::process::exit(0);
            }
            other => return Err(format!("未知参数: {other}")),
        }
    }
    Ok(args)
}

/// 组装路由
///
/// `port` 用于来源守卫（Host / Origin 白名单必须带实际端口）。
fn build_router(state: AppState, web_dir: Option<PathBuf>, port: u16) -> Router {
    let web_dir = web_dir.clone();
    Router::new()
        // ── API ──
        .route("/api/modes", get(api::get_modes))
        .route("/api/config", get(api::get_config).post(api::save_config))
        .route("/api/test", post(api::test_llm))
        .route("/api/enhance", post(api::enhance))
        .route("/api/cancel", post(api::cancel))
        .route("/api/history", get(api::get_history))
        .route("/api/clipboard", post(api::copy_to_clipboard))
        // ── 增强选项（2026-11-05 新增）──
        .route("/api/methods", get(api::get_methods))
        .route("/api/roles", get(api::get_roles))
        .route("/api/parse-url", post(api::parse_url))
        // ── 前端静态资源（含 SPA 兜底）──
        .fallback(move |uri: axum::http::Uri| async move {
            static_files::serve(&uri, web_dir.as_deref())
        })
        .with_state(state)
        // ── 来源守卫（最外层）：Host / Origin 校验 ──
        // 必须挂在 with_state 之后，这样 layer 能拿到自己的 HostGuard 状态。
        .layer(axum::middleware::from_fn_with_state(
            security::HostGuard::new(port),
            security::guard,
        ))
}

/// 探测端口是否已被其他进程监听（主动 connect，不依赖 bind 是否失败）。
///
/// Windows 特有陷阱：socket 层 `SO_REUSEADDR` 允许**劫持**已被监听的端口，
/// `TcpListener::bind` 会"成功"但请求被两个进程随机接收 —— 因此占用判定必须主动探测。
///
/// 判定：
/// - connect 成功 → 有人在监听 → 占用
/// - ConnectionRefused → 无监听者 → 空闲
/// - AddrNotAvailable → 本机该地址/端口不可用（如探测端口 0）→ 空闲，
///   **不能**按「其他错误 → 保守视为占用」处理：那会把永远绑不上的端口判成占用，
///   导致启动失败且错误信息误导。
/// - 其他错误（超时等）→ 保守视为占用
async fn port_in_use(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    match tokio::net::TcpStream::connect(addr).await {
        Ok(_) => true,
        Err(e) => !matches!(
            e.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::AddrNotAvailable
        ),
    }
}

/// 尝试绑定端口：被占用则向后顺延（默认行为）。
///
/// 候选序列由 `prompt_forge_core::ports::probe_sequence` 生成 —— 严格递增、
/// 越过 65535 即停止（旧实现 saturating_add 会在上限处重复尝试同一端口）。
/// 占用判定用主动 connect 探测，避开 Windows 的 SO_REUSEADDR 劫持。
///
/// 顺延时把警告打到 stderr（含原端口、实际端口、影响与处置建议）——
/// 端口变了必须让人看见，不能只写在日志里。
async fn bind_with_probe(
    start_port: u16,
    strict: bool,
) -> Result<(TcpListener, SocketAddr), String> {
    let mut last_err = String::new();
    for port in prompt_forge_core::ports::probe_sequence(start_port, PORT_PROBE_MAX) {
        if port_in_use(port).await {
            if strict && port == start_port {
                return Err(prompt_forge_core::ports::strict_port_error(port));
            }
            continue;
        }
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        match TcpListener::bind(addr).await {
            Ok(listener) => {
                if port != start_port {
                    // 顺延发生时必须明确告知实际端口，否则用户会去访问错的端口
                    eprintln!(
                        "{}",
                        prompt_forge_core::ports::occupied_notice(start_port, port)
                    );
                }
                return Ok((listener, addr));
            }
            Err(e) => {
                last_err = format!("{e}");
                continue;
            }
        }
    }
    Err(format!(
        "端口 {start_port} 起 {PORT_PROBE_MAX} 个候选均不可用: {last_err}"
    ))
}

/// 用系统默认浏览器打开 URL
fn open_browser(url: &str) {
    #[cfg(target_os = "windows")]
    {
        // start 命令要求空标题参数，否则带空格的路径会被截断
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误: {e}");
            std::process::exit(2);
        }
    };

    let bound = bind_with_probe(args.port, args.strict_port).await;
    let (listener, addr) = match bound {
        Ok(v) => v,
        Err(e) => {
            eprintln!("启动失败: {e}");
            std::process::exit(1);
        }
    };

    let url = format!("http://{addr}");
    let state = AppState::new();

    println!("Prompt Forge 服务已启动");
    println!("  地址: {url}");
    // 配置路径打到 stderr 而非 stdout：stdout 是机器可读通道（LISTENING 行），
    // 绝对路径里含用户名，写进日志采集/截图会泄露本机账户信息。
    eprintln!("  配置: {}", paths::config_path().display());
    if let Some(dir) = &args.web_dir {
        println!("  前端: {} (开发模式)", dir.display());
    } else {
        println!("  前端: 内嵌资源");
    }
    // 机器可读行：端口被占用顺延时，脚本据此取实际端口（人类可读行会变，勿解析它）
    println!("LISTENING {url}");
    println!("按 Ctrl+C 退出");

    if args.open_browser {
        // 稍作延迟，确保监听已就绪再打开浏览器
        let target = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            open_browser(&target);
        });
    }

    let app = build_router(state, args.web_dir, addr.port());
    let server = axum::serve(listener, app);

    // 优雅退出：Ctrl+C
    if let Err(e) = server
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            println!("\n正在退出…");
        })
        .await
    {
        eprintln!("服务异常退出: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端口测试串行锁。
    ///
    /// 这些测试会 bind 真实端口并探测相邻端口；cargo test 默认并行执行，
    /// 多个测试同时在相邻端口上探测/绑定会互相干扰（实测出现
    /// "严格模式必须绑定到指定端口" 被并发占用打脸）。串行化后结果稳定。
    ///
    /// 用 `tokio::sync::Mutex` 而非 `std::sync::Mutex`：后者的守卫是
    /// 非 Send 且在 await 期间会阻塞整个 executor（clippy `await_holding_lock`
    /// 亦会报错）；异步感知 Mutex 的守卫可安全跨 await 持有，串行语义不变。
    static PORT_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn port_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        PORT_TEST_LOCK.lock().await
    }

    /// 占用一个动态端口（端口 0 = 由系统分配），返回端口号。
    /// listener 必须保持存活到调用方结束，否则探测会得到"空闲"结论。
    async fn occupy_dynamic_port() -> (u16, TcpListener) {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind dynamic port");
        let port = listener.local_addr().expect("local addr").port();
        (port, listener)
    }

    #[tokio::test]
    async fn port_in_use_detects_listening_port() {
        let _guard = port_test_guard().await;
        let (port, _listener) = occupy_dynamic_port().await;
        assert!(port_in_use(port).await, "已监听端口必须判定为占用");
    }

    #[tokio::test]
    async fn port_in_use_false_for_free_port() {
        let _guard = port_test_guard().await;
        // 先占用再释放，得到一个刚空闲的端口
        let (port, listener) = occupy_dynamic_port().await;
        drop(listener);
        assert!(!port_in_use(port).await, "已释放端口必须判定为空闲");
    }

    #[tokio::test]
    async fn bind_with_probe_skips_occupied_port() {
        let _guard = port_test_guard().await;
        let (port, listener) = occupy_dynamic_port().await;
        // 从被占端口起顺延：实际绑定端口必须是下一个空闲端口，而非被占端口
        let (bound_listener, bound_addr) =
            bind_with_probe(port, false).await.expect("顺延应成功");
        assert_ne!(bound_addr.port(), port, "不得绑定到已被占用的端口");
        assert!(bound_addr.port() > port, "顺延必须向后递增");
        drop(bound_listener);
        drop(listener);
    }

    #[tokio::test]
    async fn bind_with_probe_non_strict_still_starts_when_occupied() {
        let _guard = port_test_guard().await;
        // 回归：默认行为不得因端口被占用而启动失败（旧版行为兼容）
        let (port, listener) = occupy_dynamic_port().await;
        let (bound, addr) = bind_with_probe(port, false)
            .await
            .expect("非严格模式下端口被占用仍应启动成功");
        assert_ne!(addr.port(), port);
        drop(bound);
        drop(listener);
    }

    #[tokio::test]
    async fn bind_with_probe_strict_fails_when_occupied() {
        let _guard = port_test_guard().await;
        let (port, listener) = occupy_dynamic_port().await;
        let err = bind_with_probe(port, true)
            .await
            .expect_err("严格模式端口被占用必须失败");
        assert!(err.contains(&port.to_string()), "错误信息应含端口号: {err}");
        drop(listener);
    }

    #[tokio::test]
    async fn bind_with_probe_strict_succeeds_on_free_port() {
        let _guard = port_test_guard().await;
        let (port, listener) = occupy_dynamic_port().await;
        drop(listener);
        let (bound, addr) = bind_with_probe(port, true).await.expect("空闲端口应启动成功");
        assert_eq!(addr.port(), port, "严格模式必须绑定到指定端口");
        drop(bound);
    }
}
