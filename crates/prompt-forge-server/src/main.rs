//! Prompt Forge HTTP 服务入口。
//!
//! 用法：
//! ```text
//! prompt-forge [--port <端口>] [--no-browser] [--web-dir <目录>]
//! ```
//!
//! - `--port`      监听端口，默认 8787（被占用时自动向后试探）
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
    open_browser: bool,
    web_dir: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        port: DEFAULT_PORT,
        open_browser: true,
        web_dir: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--port" => {
                let v = it.next().ok_or("--port 需要端口值")?;
                args.port = v.parse().map_err(|_| format!("端口格式错误: {v}"))?;
            }
            "--no-browser" => args.open_browser = false,
            "--web-dir" => {
                let v = it.next().ok_or("--web-dir 需要目录路径")?;
                args.web_dir = Some(PathBuf::from(v));
            }
            "--help" | "-h" => {
                println!("Prompt Forge HTTP 服务\n");
                println!("用法: prompt-forge [--port <端口>] [--no-browser] [--web-dir <目录>]");
                println!("  --port <端口>   监听端口（默认 {DEFAULT_PORT}）");
                println!("  --no-browser    启动后不自动打开浏览器");
                println!("  --web-dir <目录> 从目录读取前端资源（开发热更）");
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

/// 尝试绑定端口：被占用则向后试探
async fn bind_listener(start_port: u16) -> Result<(TcpListener, SocketAddr), String> {
    let mut last_err = String::new();
    for offset in 0..PORT_PROBE_MAX {
        let port = start_port.saturating_add(offset);
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok((listener, addr)),
            Err(e) => {
                last_err = format!("{e}");
                continue;
            }
        }
    }
    Err(format!(
        "端口 {start_port}..{} 均不可用: {last_err}",
        start_port.saturating_add(PORT_PROBE_MAX - 1)
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

    let (listener, addr) = match bind_listener(args.port).await {
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
