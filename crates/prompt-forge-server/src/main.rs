//! Prompt Forge HTTP 服务入口。
//!
//! 用法：
//! ```text
//! prompt-forge [--port <端口>] [--no-browser] [--web-dir <目录>]
//! ```
//!
//! - `--port`      监听端口，默认 8787
//!                · 显式指定时：端口被占用 → 直接报错退出（不静默换端口）
//!                · 未指定时：默认端口被占用 → 自动顺延到下一个可用端口并打印提示
//! - `--no-browser` 启动后不自动打开浏览器
//! - `--web-dir`   从指定目录读取前端资源（开发期热更）；缺省用内嵌资源
//!
//! 行为对齐历史桌面版：启动即拉起本地服务 + 打开浏览器访问 UI。

mod api;
mod autostart;
mod llm;
mod paths;
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
    /// `--port` 是否为用户显式指定。
    /// 显式指定时端口被占用必须 fail-fast（尊重用户明确意图）；
    /// 未显式指定（走默认值）时才允许自动顺延（开箱即用的便利）。
    port_explicit: bool,
    open_browser: bool,
    web_dir: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        port: DEFAULT_PORT,
        port_explicit: false,
        open_browser: true,
        web_dir: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--port" => {
                let v = it.next().ok_or("--port 需要端口值")?;
                args.port = v.parse().map_err(|_| format!("端口格式错误: {v}"))?;
                args.port_explicit = true;
            }
            "--no-browser" => args.open_browser = false,
            "--web-dir" => {
                let v = it.next().ok_or("--web-dir 需要目录路径")?;
                args.web_dir = Some(PathBuf::from(v));
            }
            "--help" | "-h" => {
                println!("Prompt Forge HTTP 服务\n");
                println!("用法: prompt-forge [--port <端口>] [--no-browser] [--web-dir <目录>]");
                println!("  --port <端口>   监听端口（默认 {DEFAULT_PORT}）；显式指定时若被占用将直接报错退出");
                println!("  --no-browser    启动后不自动打开浏览器");
                println!("  --web-dir <目录> 从目录读取前端资源（开发热更）");
                println!("\n说明: 未指定 --port 时，默认端口被占用会自动顺延到下一个可用端口（并打印提示）。");
                std::process::exit(0);
            }
            other => return Err(format!("未知参数: {other}")),
        }
    }
    Ok(args)
}

/// 组装路由
fn build_router(state: AppState, web_dir: Option<PathBuf>) -> Router {
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
}

/// 探测端口是否已被其他进程监听（主动 connect，不依赖 bind 是否失败）。
///
/// Windows 特有陷阱：socket 层 `SO_REUSEADDR` 允许**劫持**已被监听的端口，
/// `TcpListener::bind` 会"成功"但请求被两个进程随机接收 —— 因此占用判定必须主动探测。
///
/// 判定：
/// - connect 成功 → 有人在监听 → 占用
/// - ConnectionRefused → 无监听者 → 空闲
/// - 其他错误（超时等）→ 保守视为占用
async fn port_in_use(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    match tokio::net::TcpStream::connect(addr).await {
        Ok(_) => true,
        Err(e) => !matches!(e.kind(), std::io::ErrorKind::ConnectionRefused),
    }
}

/// 精确绑定单个端口：已被监听或绑定失败即报错，不做任何顺延。
///
/// 用于 `--port` 显式指定场景 —— 用户明确指定了端口，静默换端口会让
/// 按该端口配置的防火墙规则、书签、自动化脚本全部失效。
async fn bind_exact(port: u16) -> Result<(TcpListener, SocketAddr), String> {
    if port_in_use(port).await {
        return Err(prompt_forge_core::ports::explicit_port_error(port));
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    match TcpListener::bind(addr).await {
        Ok(listener) => Ok((listener, addr)),
        Err(e) => Err(format!(
            "端口 {port} 绑定失败: {e}\n  \
             排查: netstat -ano | findstr :{port}"
        )),
    }
}

/// 尝试绑定端口：被占用则向后顺延（仅用于默认端口）。
///
/// 候选序列由 `prompt_forge_core::ports::probe_sequence` 生成 —— 严格递增、
/// 越过 65535 即停止（旧实现 saturating_add 会在上限处重复尝试同一端口）。
/// 占用判定用主动 connect 探测，避开 Windows 的 SO_REUSEADDR 劫持。
async fn bind_with_probe(start_port: u16) -> Result<(TcpListener, SocketAddr), String> {
    let mut last_err = String::new();
    for port in prompt_forge_core::ports::probe_sequence(start_port, PORT_PROBE_MAX) {
        if port_in_use(port).await {
            continue;
        }
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        match TcpListener::bind(addr).await {
            Ok(listener) => {
                if port != start_port {
                    // 顺延发生时必须明确告知实际端口，否则用户会去访问错的端口
                    eprintln!("提示: 默认端口 {start_port} 已被占用，已自动顺延到 {port}");
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

    let bound = if args.port_explicit {
        // 显式指定：被占用即失败，不静默换端口
        bind_exact(args.port).await
    } else {
        // 默认端口：允许顺延，保证「双击即用」不被端口占用打断
        bind_with_probe(args.port).await
    };
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
    println!("  配置: {}", paths::config_path().display());
    if let Some(dir) = &args.web_dir {
        println!("  前端: {} (开发模式)", dir.display());
    } else {
        println!("  前端: 内嵌资源");
    }
    println!("按 Ctrl+C 退出");

    if args.open_browser {
        // 稍作延迟，确保监听已就绪再打开浏览器
        let target = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            open_browser(&target);
        });
    }

    let app = build_router(state, args.web_dir);
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
        let (port, _listener) = occupy_dynamic_port().await;
        assert!(port_in_use(port).await, "已监听端口必须判定为占用");
    }

    #[tokio::test]
    async fn port_in_use_false_for_free_port() {
        // 先占用再释放，得到一个刚空闲的端口
        let (port, listener) = occupy_dynamic_port().await;
        drop(listener);
        assert!(!port_in_use(port).await, "已释放端口必须判定为空闲");
    }

    #[tokio::test]
    async fn bind_exact_fails_fast_when_port_occupied() {
        let (port, _listener) = occupy_dynamic_port().await;
        let err = bind_exact(port).await.expect_err("显式端口被占用必须失败");
        assert!(err.contains(&port.to_string()), "错误信息应含端口号: {err}");
        assert!(err.contains("不再自动顺延"), "错误信息应说明不顺延: {err}");
    }

    #[tokio::test]
    async fn bind_with_probe_skips_occupied_port() {
        let (port, listener) = occupy_dynamic_port().await;
        // 从被占端口起顺延：实际绑定端口必须是下一个空闲端口，而非被占端口
        let (bound_listener, bound_addr) =
            bind_with_probe(port).await.expect("顺延应成功");
        assert_ne!(bound_addr.port(), port, "不得绑定到已被占用的端口");
        assert!(bound_addr.port() > port, "顺延必须向后递增");
        drop(bound_listener);
        drop(listener);
    }

    #[tokio::test]
    async fn bind_exact_succeeds_on_free_port() {
        let (port, listener) = occupy_dynamic_port().await;
        drop(listener);
        let (bound, addr) = bind_exact(port).await.expect("空闲端口应绑定成功");
        assert_eq!(addr.port(), port);
        drop(bound);
    }
}
