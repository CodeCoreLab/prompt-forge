//! 应用命令层：前端通过 invoke 调用的 Tauri commands。

use crate::config::{AppConfig, ConfigIo, LlmConfig};
use crate::llm;
use prompt_forge_core::modes;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

/// 模式列表 = 内置 + 用户自定义（从 config.json 读取）
#[tauri::command]
pub fn get_modes(app: AppHandle) -> Result<Vec<Value>, String> {
    let cfg = AppConfig::load(&app).unwrap_or_default();
    let mut list: Vec<Value> = modes::MODES
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "name": m.name,
                "description": m.description,
            })
        })
        .collect();
    for m in &cfg.custom_modes {
        list.push(json!({
            "id": m.full_id(),
            "name": m.name,
            "description": format!("[自定义·{}]", if m.kind == "extend" { "继承" } else { "替换" }),
        }));
    }
    Ok(list)
}

#[tauri::command]
pub fn get_config(app: AppHandle) -> Result<Value, String> {
    let mut cfg = AppConfig::load(&app)?;
    // 返回给前端的 key 解密为明文（前端编辑需要；DPAPI 当前用户作用域内安全）
    for p in &mut cfg.profiles {
        p.api_key = prompt_forge_core::crypto::load_key(&p.api_key);
    }
    serde_json::to_value(cfg).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_config(app: AppHandle, config: Value) -> Result<(), String> {
    let mut cfg: AppConfig = serde_json::from_value(config).map_err(|e| e.to_string())?;
    // 落盘前把明文 key 加密（已是 enc: 形态的原样保留，避免重复加密）
    for p in &mut cfg.profiles {
        if !prompt_forge_core::crypto::is_encrypted(&p.api_key) {
            p.api_key = prompt_forge_core::crypto::store_key(&p.api_key);
        }
    }
    cfg.save(&app)?; // 校验在 AppConfig::save（入口一次校验）
    // 自启动偏好即时生效
    let autostart = tauri_plugin_autostart::ManagerExt::autolaunch(&app);
    if cfg.autostart {
        autostart.enable().map_err(|e| e.to_string())?;
    } else {
        autostart.disable().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn test_llm(profile: LlmConfig) -> Result<String, String> {
    llm::enhance(&profile, "回复：连接成功").await
}

// ── 历史记录（P2-1，对标 DSH optimizer 轻量记忆链）─────────────

/// 进程内历史栈：最近 20 条成功增强。持久化不做（会话级够用，避免密文/隐私落盘）。
const HISTORY_CAP: usize = 20;

#[derive(Clone, serde::Serialize)]
pub struct HistoryEntry {
    pub input: String,
    pub mode_id: String,
    pub output: String,
    pub ts: u64, // unix 秒
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── 请求取消（P2-2）────────────────────────────────────────────

/// 共享取消令牌：每次 enhance 前替换新令牌，cancel 按钮触发当前令牌
#[derive(Default)]
pub struct CancelState(pub std::sync::RwLock<tokio_util::sync::CancellationToken>);

/// 单轮快速增强：内置模式走模板库，custom-* 前缀走自定义模式；成功后入历史栈。
/// 可被 cancel_enhance 中断（中断返回 cancelled 码，不判失败不弹错误动作）。
#[tauri::command]
pub async fn enhance_prompt(
    app: AppHandle,
    state: tauri::State<'_, tokio::sync::Mutex<Vec<HistoryEntry>>>,
    cancel: tauri::State<'_, CancelState>,
    input: String,
    mode_id: String,
) -> Result<String, String> {
    let cfg = AppConfig::load(&app)?;
    let prompt = if let Some(custom_id) = mode_id.strip_prefix("custom-") {
        let mode = cfg
            .custom_modes
            .iter()
            .find(|m| m.id == custom_id)
            .ok_or_else(|| format!("自定义模式不存在: {mode_id}"))?;
        modes::build_custom_prompt(mode, &input)?
    } else {
        modes::build_prompt(&mode_id, &input)?
    };
    let profile = cfg
        .active()
        .ok_or_else(|| "no_profile:尚未配置大模型，请先在设置中添加配置".to_string())?
        .clone();
    // DPAPI 加密配置 → 解密出明文 key 供 HTTP 调用（P2-3）
    let mut profile = profile;
    profile.api_key = prompt_forge_core::crypto::load_key(&profile.api_key);

    // 新一轮开始：换新令牌（旧令牌先 cancel，废弃在途轮次）
    let token = tokio_util::sync::CancellationToken::new();
    cancel.0.write().unwrap().cancel();
    *cancel.0.write().unwrap() = token.clone();

    let result = llm::enhance_cancellable(&profile, &prompt, token).await;
    match result {
        Ok(output) => {
            // 成功才入栈；超限驱逐最旧
            let mut hist = state.lock().await;
            hist.push(HistoryEntry {
                input: input.clone(),
                mode_id,
                output: output.clone(),
                ts: now_secs(),
            });
            if hist.len() > HISTORY_CAP {
                let overflow = hist.len() - HISTORY_CAP;
                hist.drain(..overflow);
            }
            Ok(output)
        }
        Err(e) if e == "cancelled:已取消" => Err(e), // 取消：前端静默处理
        Err(e) => Err(e),
    }
}

/// 用户取消当前增强
#[tauri::command]
pub fn cancel_enhance(cancel: tauri::State<'_, CancelState>) {
    cancel.0.read().unwrap().cancel();
}

/// 读取历史（最新在前）
#[tauri::command]
pub async fn get_history(
    state: tauri::State<'_, tokio::sync::Mutex<Vec<HistoryEntry>>>,
) -> Result<Vec<HistoryEntry>, String> {
    let hist = state.lock().await;
    Ok(hist.iter().rev().cloned().collect())
}

/// 复制文本到系统剪贴板（无额外插件依赖）
#[tauri::command]
pub fn copy_to_clipboard(text: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        // clip.exe 读 stdin；UTF-16LE 才不乱码
        use std::io::Write;
        let mut child = Command::new("clip.exe")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        if let Some(stdin) = child.stdin.as_mut() {
            let wide: Vec<u8> = text
                .encode_utf16()
                .flat_map(|u| u.to_le_bytes())
                .collect();
            stdin.write_all(&wide).map_err(|e| e.to_string())?;
        }
        let _ = child.wait();
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = text;
        Err("当前平台未实现剪贴板".into())
    }
}

/// 设置窗口关闭行为：最小化到托盘 或 退出
#[tauri::command]
pub fn close_main_window(app: AppHandle) -> Result<(), String> {
    let cfg = AppConfig::load(&app).unwrap_or_default();
    if let Some(win) = app.get_webview_window("main") {
        if cfg.close_to_tray {
            win.hide().map_err(|e| e.to_string())?;
        } else {
            app.exit(0);
        }
    }
    Ok(())
}
