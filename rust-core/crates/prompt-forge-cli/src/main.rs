use prompt_forge_core::config::{AppConfig, LlmConfig};
use prompt_forge_core::crypto;
use prompt_forge_core::modes;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tokio_util::sync::CancellationToken;

mod llm;

/// 全局取消令牌：enhance 前换新令牌，cancel 动作触发当前令牌。
/// 与 Tauri 版 CancelState 同构。
fn cancel_slot() -> &'static Arc<Mutex<CancellationToken>> {
    static SLOT: OnceLock<Arc<Mutex<CancellationToken>>> = OnceLock::new();
    SLOT.get_or_init(|| Arc::new(Mutex::new(CancellationToken::new())))
}

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    id: String,
    action: String,
    #[serde(flatten)]
    payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct Response {
    id: String,
    success: bool,
    data: serde_json::Value,
    error: Option<String>,
}

/// stdout 写锁：多任务并发响应时保证单行完整。
fn stdout_lock() -> &'static Mutex<io::Stdout> {
    static LOCK: OnceLock<Mutex<io::Stdout>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(io::stdout()))
}

/// 配置文件互斥锁：请求并发执行，读改写必须原子，否则后写覆盖前写。
fn config_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn write_response(resp: &Response) {
    let mut out = stdout_lock().lock().unwrap();
    if let Ok(line) = serde_json::to_string(resp) {
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let stdin = io::stdin();

    // 每行请求独立 spawn：enhance 阻塞期间仍能接收 cancel 等并发指令。
    // 响应带 id，由 Electron 侧按 id 匹配，乱序无影响。
    for line in stdin.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        tokio::spawn(async move {
            let req: Request = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    write_response(&Response {
                        id: String::new(),
                        success: false,
                        data: serde_json::Value::Null,
                        error: Some(format!("Invalid request: {e}")),
                    });
                    return;
                }
            };
            let result = handle_action(&req.action, &req.payload).await;
            let resp = match result {
                Ok(data) => Response {
                    id: req.id,
                    success: true,
                    data,
                    error: None,
                },
                Err(e) => Response {
                    id: req.id,
                    success: false,
                    data: serde_json::Value::Null,
                    error: Some(e.to_string()),
                },
            };
            write_response(&resp);
        });
    }

    Ok(())
}

async fn handle_action(
    action: &str,
    payload: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    match action {
        "enhance" => {
            let text = payload["text"].as_str().ok_or_else(|| anyhow::anyhow!("Missing text"))?;
            let mode = payload["mode"].as_str().ok_or_else(|| anyhow::anyhow!("Missing mode"))?;

            let config = load_config()?;
            let prompt = if let Some(custom_id) = mode.strip_prefix("custom-") {
                let custom_mode = config
                    .custom_modes
                    .iter()
                    .find(|m| m.id == custom_id)
                    .ok_or_else(|| anyhow::anyhow!("自定义模式不存在: {}", mode))?;
                modes::build_custom_prompt(custom_mode, text).map_err(|e| anyhow::anyhow!(e))?
            } else {
                modes::build_prompt(mode, text).map_err(|e| anyhow::anyhow!(e))?
            };

            let mut profile = config
                .active()
                .ok_or_else(|| anyhow::anyhow!("no_profile:尚未配置大模型，请先在设置中添加配置"))?
                .clone();
            // 落盘为 DPAPI 密文，调用前解密出明文
            profile.api_key = crypto::load_key(&profile.api_key);

            // 新一轮换新令牌（旧令牌先 cancel，废弃在途轮次）
            let token = CancellationToken::new();
            {
                let mut slot = cancel_slot().lock().unwrap();
                slot.cancel();
                *slot = token.clone();
            }

            let result = llm::enhance_cancellable(&profile, &prompt, token)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;

            // 仅成功入历史；超限驱逐最旧
            append_history(prompt_forge_core::config::HistoryEntry {
                input: text.to_string(),
                mode_id: mode.to_string(),
                output: result.clone(),
                ts: now_secs(),
            })?;

            Ok(serde_json::to_value(result)?)
        }
        "cancel" => {
            cancel_slot().lock().unwrap().cancel();
            Ok(serde_json::json!({"status": "ok"}))
        }
        "get_modes" => {
            let modes = get_modes();
            Ok(serde_json::to_value(modes)?)
        }
        "get_config" => {
            let mut config = load_config()?;
            // 返回前端可编辑形态：key 解密为明文（DPAPI 当前用户作用域内安全）
            for p in &mut config.profiles {
                p.api_key = crypto::load_key(&p.api_key);
            }
            Ok(serde_json::to_value(config)?)
        }
        "save_config" => {
            let mut config: AppConfig = serde_json::from_value(payload.clone())
                .map_err(|e| anyhow::anyhow!("配置格式错误: {e}"))?;
            // 落盘前加密明文 key（已是 enc: 形态的原样保留，避免重复加密）
            for p in &mut config.profiles {
                if !crypto::is_encrypted(&p.api_key) {
                    p.api_key = crypto::store_key(&p.api_key);
                }
            }
            save_config(&config)?;
            Ok(serde_json::json!({"status": "ok"}))
        }
        "test_llm" => {
            let mut profile: LlmConfig = serde_json::from_value(payload.clone())
                .map_err(|e| anyhow::anyhow!("配置格式错误: {e}"))?;
            profile.api_key = crypto::load_key(&profile.api_key);
            let result = llm::enhance(&profile, "回复：连接成功")
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            Ok(serde_json::json!({"status": "ok", "response": result}))
        }
        "get_history" => {
            let config = load_config()?;
            let history: Vec<serde_json::Value> = config
                .history
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "input": h.input,
                        "mode_id": h.mode_id,
                        "output": h.output,
                        "ts": h.ts,
                    })
                })
                .collect();
            Ok(serde_json::to_value(history)?)
        }
        _ => Err(anyhow::anyhow!("Unknown action: {}", action)),
    }
}

// ── 配置管理 ──

fn config_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("无法定位配置目录"))?;
    let dir = base.join("prompt-forge");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn config_path() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("config.json"))
}

fn load_config() -> anyhow::Result<AppConfig> {
    let _guard = config_lock().lock().unwrap();
    load_config_unlocked()
}

fn load_config_unlocked() -> anyhow::Result<AppConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let content = std::fs::read_to_string(&path)?;
    let config: AppConfig =
        serde_json::from_str(&content).map_err(|e| anyhow::anyhow!("配置文件解析失败: {e}"))?;
    Ok(config)
}

/// 入口校验 + 原子写（先写 .tmp 再 rename，避免半截文件）。
fn save_config(config: &AppConfig) -> anyhow::Result<()> {
    let _guard = config_lock().lock().unwrap();
    save_config_unlocked(config)
}

fn save_config_unlocked(config: &AppConfig) -> anyhow::Result<()> {
    config
        .validate()
        .map_err(|e| anyhow::anyhow!("配置校验失败: {e}"))?;
    let path = config_path()?;
    let json = serde_json::to_string_pretty(config)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// 历史追加：读改写整体加锁，避免与并发 save_config 互相覆盖。
fn append_history(entry: prompt_forge_core::config::HistoryEntry) -> anyhow::Result<()> {
    let _guard = config_lock().lock().unwrap();
    let mut config = load_config_unlocked()?;
    config.history.insert(0, entry);
    config.history.truncate(20);
    save_config_unlocked(&config)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── 模式管理 ──

fn get_modes() -> Vec<serde_json::Value> {
    let mut list = Vec::new();

    for m in modes::MODES {
        list.push(serde_json::json!({
            "id": m.id,
            "name": m.name,
            "description": m.description,
        }));
    }

    if let Ok(config) = load_config() {
        for m in config.custom_modes {
            list.push(serde_json::json!({
                "id": format!("custom-{}", m.id),
                "name": m.name,
                "description": format!("[自定义·{}]", if m.kind == "extend" { "继承" } else { "替换" }),
            }));
        }
    }

    list
}
