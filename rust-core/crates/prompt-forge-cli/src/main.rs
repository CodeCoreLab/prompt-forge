use prompt_forge_core::*;
use prompt_forge_core::config::{AppConfig, LlmConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use uuid::Uuid;

mod llm;

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout_lock = stdout.lock();

    for line in stdin.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response {
                    id: Uuid::new_v4().to_string(),
                    success: false,
                    data: serde_json::Value::Null,
                    error: Some(format!("Invalid request: {e}")),
                };
                writeln!(stdout_lock, "{}", serde_json::to_string(&resp)?)?;
                stdout_lock.flush()?;
                continue;
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

        writeln!(stdout_lock, "{}", serde_json::to_string(&resp)?)?;
        stdout_lock.flush()?;
    }

    Ok(())
}

async fn handle_action(action: &str, payload: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    match action {
        "enhance" => {
            let text = payload["text"].as_str().ok_or_else(|| anyhow::anyhow!("Missing text"))?;
            let mode = payload["mode"].as_str().ok_or_else(|| anyhow::anyhow!("Missing mode"))?;
            
            // 构建提示词
            let prompt = if let Some(custom_id) = mode.strip_prefix("custom-") {
                let config = load_config()?;
                let custom_mode = config.custom_modes
                    .iter()
                    .find(|m| m.id == custom_id)
                    .ok_or_else(|| anyhow::anyhow!("自定义模式不存在: {}", mode))?;
                modes::build_custom_prompt(custom_mode, text).map_err(|e| anyhow::anyhow!(e))?
            } else {
                modes::build_prompt(mode, text).map_err(|e| anyhow::anyhow!(e))?
            };
            
            // 获取活跃配置
            let config = load_config()?;
            let profile = config.active()
                .ok_or_else(|| anyhow::anyhow!("no_profile:尚未配置大模型，请先在设置中添加配置"))?
                .clone();
            
            // 调用 LLM
            let result = llm::enhance(&profile, &prompt).await.map_err(|e| anyhow::anyhow!(e))?;
            
            // 保存历史记录
            let mut config = load_config()?;
            config.history.insert(0, prompt_forge_core::HistoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                original: text.to_string(),
                enhanced: result.clone(),
                mode: mode.to_string(),
                created_at: chrono::Local::now().to_rfc3339(),
            });
            if config.history.len() > 20 {
                config.history.truncate(20);
            }
            save_config(&config)?;
            
            Ok(serde_json::json!({
                "original": text,
                "enhanced": result,
                "mode": mode,
            }))
        }
        "get_modes" => {
            let modes = get_modes();
            Ok(serde_json::to_value(modes)?)
        }
        "get_config" => {
            let config = load_config()?;
            Ok(serde_json::to_value(config)?)
        }
        "save_config" => {
            let config: AppConfig = serde_json::from_value(payload.clone())?;
            save_config(&config)?;
            Ok(serde_json::json!({"status": "ok"}))
        }
        "test_llm" => {
            let profile: LlmConfig = serde_json::from_value(payload.clone())?;
            let result = llm::enhance(&profile, "连接测试").await.map_err(|e| anyhow::anyhow!(e))?;
            Ok(serde_json::json!({"status": "ok", "response": result}))
        }
        "get_history" => {
            let config = load_config()?;
            let history = config.history.iter().map(|h| {
                serde_json::json!({
                    "id": h.id,
                    "input": h.original,
                    "output": h.enhanced,
                    "mode": h.mode,
                    "ts": h.created_at,
                })
            }).collect();
            Ok(serde_json::json!(history))
        }
        "copy_to_clipboard" => {
            // Clipboard is handled in Electron main process
            Ok(serde_json::json!({"status": "ok"}))
        }
        _ => Err(anyhow::anyhow!("Unknown action: {}", action)),
    }
}

// ── 配置管理 ──

fn config_dir() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Home directory not found"))?;
    let dir = home.join("AppData").join("Roaming").join("prompt-forge");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn config_path() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("config.json"))
}

fn load_config() -> anyhow::Result<AppConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let content = fs::read_to_string(&path)?;
    let config: AppConfig = serde_json::from_str(&content)?;
    Ok(config)
}

fn save_config(config: &AppConfig) -> anyhow::Result<()> {
    let path = config_path()?;
    let json = serde_json::to_string_pretty(config)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

// ── 模式管理 ──

fn get_modes() -> Vec<serde_json::Value> {
    let mut list = Vec::new();
    
    // 内置模式
    for m in modes::MODES {
        list.push(serde_json::json!({
            "id": m.id,
            "name": m.name,
            "description": m.description,
        }));
    }
    
    // 自定义模式
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
