//! 开机自启（Windows 注册表 `HKCU\...\Run`）。
//!
//! 历史版本用 Tauri autostart 插件实现；纯 Web 版本改为直接写注册表 Run 键，
//! 依赖 `winreg`（纯 Rust 调用 Win32 API，无 C 依赖、无 node-gyp）。
//!
//! 非 Windows 平台：不支持（返回明确错误，不静默假装成功）。

/// 注册表项名称
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 值名称
const VALUE_NAME: &str = "PromptForge";

/// 启用开机自启
#[cfg(target_os = "windows")]
pub fn enable() -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;

    let exe = std::env::current_exe().map_err(|e| format!("无法定位可执行文件: {e}"))?;
    // 带引号，兼容含空格的安装路径
    let cmd = format!("\"{}\"", exe.display());

    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(RUN_KEY)
        .map_err(|e| format!("打开注册表 Run 键失败: {e}"))?;
    key.set_value(VALUE_NAME, &cmd)
        .map_err(|e| format!("写入自启项失败: {e}"))?;
    Ok(())
}

/// 关闭开机自启（幂等：项不存在视为已关闭）
#[cfg(target_os = "windows")]
pub fn disable() -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let key = match hkcu.open_subkey_with_flags(RUN_KEY, winreg::enums::KEY_SET_VALUE) {
        Ok(k) => k,
        // Run 键或值不存在 → 已处于关闭状态，视为成功
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("打开注册表 Run 键失败: {e}")),
    };
    match key.delete_value(VALUE_NAME) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), // 已删除
        Err(e) => Err(format!("删除自启项失败: {e}")),
    }
}

/// 查询当前是否已启用自启
#[cfg(target_os = "windows")]
pub fn is_enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.open_subkey(RUN_KEY) {
        Ok(key) => key.get_value::<String, _>(VALUE_NAME).is_ok(),
        Err(_) => false,
    }
}

#[cfg(not(target_os = "windows"))]
pub fn enable() -> Result<(), String> {
    Err("当前平台不支持开机自启".into())
}

#[cfg(not(target_os = "windows"))]
pub fn disable() -> Result<(), String> {
    Err("当前平台不支持开机自启".into())
}

#[cfg(not(target_os = "windows"))]
pub fn is_enabled() -> bool {
    false
}

/// 按配置同步自启状态（保存配置时调用，即时生效）
pub fn apply(enabled: bool) -> Result<(), String> {
    if enabled {
        enable()
    } else {
        disable()
    }
}
