//! 开机自启。
//!
//! Windows：注册表 `HKCU\...\Run`，依赖 `winreg`（纯 Rust 调用 Win32 API，无 C 依赖）。
//! Linux（信创麒麟/UOS）：写 XDG autostart 规范的 `.desktop` 文件，桌面会话登录后自动拉起。
//! 两个平台均为幂等实现：重复启用/关闭结果一致。

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

/// Linux autostart 目录（`$XDG_CONFIG_HOME/autostart`，缺省 `~/.config/autostart`）
#[cfg(not(target_os = "windows"))]
fn xdg_autostart_dir() -> Result<std::path::PathBuf, String> {
    if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
        if !v.is_empty() {
            return Ok(std::path::PathBuf::from(v).join("autostart"));
        }
    }
    let home = std::env::var_os("HOME")
        .ok_or("无法定位 HOME 目录，无法写入自启项")?
        .to_string_lossy()
        .into_owned();
    Ok(std::path::PathBuf::from(home).join(".config/autostart"))
}

/// 启用开机自启（Linux：写 XDG autostart `.desktop`）
#[cfg(not(target_os = "windows"))]
pub fn enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法定位可执行文件: {e}"))?;
    let dir = xdg_autostart_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 autostart 目录失败: {e}"))?;
    let file = dir.join("prompt-forge.desktop");
    // Exec 按 .desktop 规范转义双引号，兼容含空格路径
    let content = format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName=Prompt Forge\n\
         Comment=提示词增强工具\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display().to_string().replace('"', "\\\"")
    );
    std::fs::write(&file, content).map_err(|e| format!("写入自启项失败: {e}"))?;
    Ok(())
}

/// 关闭开机自启（Linux：删 `.desktop`；幂等，不存在视为已关闭）
#[cfg(not(target_os = "windows"))]
pub fn disable() -> Result<(), String> {
    let dir = xdg_autostart_dir()?;
    match std::fs::remove_file(dir.join("prompt-forge.desktop")) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("删除自启项失败: {e}")),
    }
}

/// 查询当前是否已启用自启（Linux：`.desktop` 文件存在即启用）
#[cfg(not(target_os = "windows"))]
pub fn is_enabled() -> bool {
    match xdg_autostart_dir() {
        Ok(dir) => dir.join("prompt-forge.desktop").exists(),
        Err(_) => false,
    }
}

/// 按配置同步自启状态（保存配置时调用，即时生效）
pub fn apply(enabled: bool) -> Result<(), String> {
    if enabled {
        enable()
    } else {
        disable()
    }
}
