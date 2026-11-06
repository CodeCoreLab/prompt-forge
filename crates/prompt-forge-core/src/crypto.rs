//! API Key DPAPI 加密（P2-3）。
//!
//! Windows：CryptProtectData/CryptUnprotectData（当前用户作用域，跨进程不可解）。
//! 存储格式：`enc:<base64>`；读取时识别前缀解密，明文原样透传（向后兼容旧配置）。
//! 非 Windows：明文透传（桌面定位以 Windows 为主）。
//!
//! 实现用 Crypt32.dll 的 C ABI 直调（fn! 类型别名声明签名），不依赖 windows crate
//! 的版本敏感包装，规避 LocalFree/Option<Blob> 等签名漂移问题。
//!
//! **零 I/O 约束**：本模块属 core crate，不得直接写日志。运行期告警统一进入
//! [`drain_warnings`] 缓冲（缓冲有上限，不会无限增长），由 server 层调用
//! [`drain_warnings`] 取走后统一输出。
//!
//! **失败语义**：解密失败/加密失败一律上抛 `Err`，绝不静默变空串、绝不降级明文落盘。
//! 调用方（server）负责把错误显式呈现给用户。

use base64::Engine as _;
use std::collections::VecDeque;
use std::sync::Mutex;

const PREFIX: &str = "enc:";

/// 运行期告警缓冲（core 层不写日志，由 server 层取走后统一输出）
///
/// 容量上限：告警缓冲必须有界 —— 缓冲只在 server 侧调用
/// [`drain_warnings`] 时才被清空，若 server 未接线，长期运行下
/// 每次失败都追加会造成无界内存增长。
///
/// 用 `VecDeque` 而非 `Vec`：告警是「先入先出」语义，溢出的应当是最旧的
/// 一条；`Vec::remove(0)` 是 O(n) 的头部删除，`pop_front` 是 O(1)。
static WARNINGS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// 告警缓冲容量上限（超出后丢弃最旧的告警）
pub const WARNINGS_CAP: usize = 64;

/// 记一条运行期告警（core 层不写日志，由 server 层取走后统一输出）
///
/// 追加后若超出容量上限，则丢弃最旧的一条，保证缓冲长度恒有界。
fn push_warning(msg: impl Into<String>) {
    if let Ok(mut g) = WARNINGS.lock() {
        g.push_back(msg.into());
        while g.len() > WARNINGS_CAP {
            g.pop_front();
        }
    }
}

/// 取走并清空告警缓冲
pub fn drain_warnings() -> Vec<String> {
    WARNINGS
        .lock()
        .map(|mut g| std::mem::take(&mut *g).into_iter().collect())
        .unwrap_or_default()
}

/// 判断字符串是否为加密形态
pub fn is_encrypted(s: &str) -> bool {
    s.starts_with(PREFIX)
}

#[cfg(target_os = "windows")]
mod dpapi {
    use super::PREFIX;

    // CRYPTPROTECT_UI_FORBIDDEN
    const UI_FORBIDDEN: u32 = 0x1;

    #[repr(C)]
    struct CryptBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    // Win32 API 参数名保留 MSDN 原始拼写（与官方文档一一对应，便于比对）
    #[allow(non_snake_case)]
    type CryptProtectDataFn = unsafe extern "system" fn(
        pDataIn: *const CryptBlob,
        szDataDescr: *const u16,
        pOptionalEntropy: *const CryptBlob,
        pvReserved: *mut core::ffi::c_void,
        pPromptStruct: *mut core::ffi::c_void,
        dwFlags: u32,
        pDataOut: *mut CryptBlob,
    ) -> i32;

    #[allow(non_snake_case)]
    type CryptUnprotectDataFn = unsafe extern "system" fn(
        pDataIn: *const CryptBlob,
        ppszDataDescr: *mut *mut u16,
        pOptionalEntropy: *const CryptBlob,
        pvReserved: *mut core::ffi::c_void,
        pPromptStruct: *mut core::ffi::c_void,
        dwFlags: u32,
        pDataOut: *mut CryptBlob,
    ) -> i32;

    type LocalFreeFn = unsafe extern "system" fn(hmem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;

    /// 系统 DLL 绝对路径（防 DLL 搜索顺序劫持：当前工作目录下的同名 DLL 会先于系统目录被加载）
    fn system_dll(name: &str) -> String {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        format!("{root}\\System32\\{name}")
    }

    fn crypt32() -> Result<&'static libloading::Library, String> {
        use std::sync::OnceLock;
        static LIB: OnceLock<std::result::Result<libloading::Library, String>> = OnceLock::new();
        LIB.get_or_init(|| {
            // 加载失败不是致命错误 —— 返回错误让调用方降级，而不是 panic 崩掉整个进程
            // 用系统目录绝对路径加载，避免工作目录下的同名 DLL 劫持
            let path = system_dll("crypt32.dll");
            unsafe { libloading::Library::new(&path) }
                .map_err(|e| format!("crypt32.dll 加载失败({path}): {e}"))
        })
        .as_ref()
        .map_err(|e| e.clone())
    }

    /// 明文（UTF-16）→ DPAPI 密文字节
    pub fn protect_bytes(plain: &str) -> Result<Vec<u8>, String> {
        let lib = crypt32()?;
        let f: libloading::Symbol<CryptProtectDataFn> = unsafe {
            lib.get(b"CryptProtectData").map_err(|e| format!("CryptProtectData 加载失败: {e}"))?
        };
        let mut input: Vec<u16> = plain.encode_utf16().collect();
        if input.last() != Some(&0) {
            input.push(0);
        }
        let mut out = CryptBlob { cb_data: 0, pb_data: std::ptr::null_mut() };
        let blob_in = CryptBlob {
            cb_data: (input.len() * 2) as u32,
            pb_data: input.as_mut_ptr() as *mut u8,
        };
        let ok = unsafe { f(&blob_in, std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), UI_FORBIDDEN, &mut out) };
        if ok == 0 {
            return Err("CryptProtectData 调用失败".into());
        }
        let bytes = unsafe { std::slice::from_raw_parts(out.pb_data, out.cb_data as usize) }.to_vec();
        free_blob(out);
        Ok(bytes)
    }

    /// DPAPI 密文字节 → 明文
    pub fn unprotect_bytes(data: &[u8]) -> Result<String, String> {
        let lib = crypt32()?;
        let f: libloading::Symbol<CryptUnprotectDataFn> = unsafe {
            lib.get(b"CryptUnprotectData").map_err(|e| format!("CryptUnprotectData 加载失败: {e}"))?
        };
        let blob_in = CryptBlob {
            cb_data: data.len() as u32,
            pb_data: data.as_ptr() as *mut u8,
        };
        let mut out = CryptBlob { cb_data: 0, pb_data: std::ptr::null_mut() };
        let ok = unsafe { f(&blob_in, std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), UI_FORBIDDEN, &mut out) };
        if ok == 0 {
            return Err("CryptUnprotectData 调用失败（密文可能来自其他用户/机器）".into());
        }
        let wide = unsafe { std::slice::from_raw_parts(out.pb_data as *const u16, out.cb_data as usize / 2) };
        let s = String::from_utf16_lossy(wide);
        free_blob(out);
        Ok(s.trim_end_matches('\0').to_string())
    }

    /// 释放 DPAPI 分配的内存。
    ///
    /// kernel32/LocalFree 加载失败时**不 panic**（release 是 `panic = "abort"`，
    /// 崩掉整个服务不值得）——改为记录告警并放弃释放（一次性小内存泄漏，
    /// 远优于进程崩溃）。告警由 server 层统一输出（core 层不写日志）。
    fn free_blob(b: CryptBlob) {
        if b.pb_data.is_null() {
            return;
        }
        let path = system_dll("kernel32.dll");
        let k32 = match unsafe { libloading::Library::new(&path) } {
            Ok(v) => v,
            Err(e) => {
                super::push_warning(format!("kernel32.dll 加载失败，DPAPI 缓冲区未释放（内存泄漏）: {e}"));
                return;
            }
        };
        let f: libloading::Symbol<LocalFreeFn> = match unsafe { k32.get(b"LocalFree") } {
            Ok(v) => v,
            Err(e) => {
                super::push_warning(format!("LocalFree 符号缺失，DPAPI 缓冲区未释放（内存泄漏）: {e}"));
                return;
            }
        };
        unsafe { f(b.pb_data as *mut _) };
    }

    pub fn wrap(b64: String) -> String {
        format!("{PREFIX}{b64}")
    }

    pub fn unwrap_prefix(s: &str) -> Option<&str> {
        s.strip_prefix(PREFIX)
    }
}

/// 明文 → `enc:<base64>`（Windows）；其他平台原样
pub fn protect(plain: &str) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let bytes = dpapi::protect_bytes(plain)?;
        Ok(dpapi::wrap(base64::engine::general_purpose::STANDARD.encode(bytes)))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(plain.to_string())
    }
}

/// `enc:<base64>` → 明文
pub fn unprotect(stored: &str) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let b64 = dpapi::unwrap_prefix(stored).ok_or("非加密格式")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("base64 解码失败: {e}"))?;
        dpapi::unprotect_bytes(&bytes)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = stored;
        Err("当前平台不支持解密".into())
    }
}

/// 读取侧归一：加密形态 → 解密；明文 → 原样（兼容旧配置）。
///
/// 解密失败**显式报错**，不静默变空串（静默空串会让用户面对莫名其妙的 401）。
pub fn load_key(stored: &str) -> Result<String, String> {
    if is_encrypted(stored) {
        unprotect(stored)
    } else {
        Ok(stored.to_string())
    }
}

/// 保存侧归一：明文 → 加密落盘（空串跳过）。
///
/// 加密失败**显式报错**，不降级明文落盘（明文落盘 = 密钥裸奔）。
pub fn store_key(plain: &str) -> Result<String, String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    protect(plain)
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn dpapi_roundtrip() {
        let plain = "sk-test-key-12345";
        let enc = protect(plain).unwrap();
        assert!(enc.starts_with(PREFIX));
        assert!(!enc.contains(plain)); // 密文不含明文
        assert_eq!(load_key(&enc).unwrap(), plain);
    }

    #[test]
    fn plaintext_passthrough() {
        assert_eq!(load_key("sk-plain").unwrap(), "sk-plain");
        assert_eq!(store_key("").unwrap(), "");
    }

    #[test]
    fn is_encrypted_check() {
        assert!(is_encrypted("enc:abcd"));
        assert!(!is_encrypted("sk-plain"));
    }

    #[test]
    fn load_key_corrupted_ciphertext_is_error_not_empty() {
        // 密文损坏必须显式报错，而不是静默返回空串
        let err = load_key("enc:!!!not-base64!!!").unwrap_err();
        assert!(err.contains("base64"), "实际错误: {err}");
    }

    #[test]
    fn load_key_other_user_ciphertext_is_error() {
        // DPAPI 密文被篡改/来自其他机器 → CryptUnprotectData 失败
        let err = load_key("enc:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn store_key_roundtrip() {
        let enc = store_key("sk-roundtrip").unwrap();
        assert!(is_encrypted(&enc));
        assert_eq!(load_key(&enc).unwrap(), "sk-roundtrip");
    }
}

/// 告警缓冲的行为约束（与平台无关，单独成模块以便在任意平台跑）
#[cfg(test)]
mod warning_tests {
    use super::*;

    /// `WARNINGS` 是**全局 static**，cargo test 默认并行执行线程 ——
    /// 两个用例同时读写会互相干扰（flaky）。用一把测试内锁把
    /// 「涉及告警缓冲」的用例串行化。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// 回归：告警缓冲必须有界 —— 缓冲只在 server 侧调用 `drain_warnings()` 时
    /// 才被清空，若无上限，长期运行下每次失败都追加 → 无界内存增长。
    #[test]
    fn warnings_buffer_is_bounded() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = drain_warnings(); // 清场，避免与其他测试互相干扰
        for i in 0..(WARNINGS_CAP + 50) {
            push_warning(format!("w{i}"));
        }
        let all = drain_warnings();
        assert!(
            all.len() <= WARNINGS_CAP,
            "告警缓冲必须有界，实际 {} 条",
            all.len()
        );
        assert_eq!(all.len(), WARNINGS_CAP, "应恰好填满上限");
        // 保留的应是最新的告警（最旧的被丢弃）
        assert_eq!(
            all.last().map(|s| s.as_str()),
            Some(format!("w{}", WARNINGS_CAP + 49).as_str()),
            "应保留最新告警"
        );
        assert!(
            !all.iter().any(|s| s == "w0"),
            "最旧的告警应被丢弃"
        );
    }

    #[test]
    fn drain_clears_buffer() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = drain_warnings();
        push_warning("x");
        assert_eq!(drain_warnings().len(), 1);
        assert!(drain_warnings().is_empty(), "drain 后缓冲必须为空");
    }

    /// 回归：`drain_warnings` 必须把全部内容取走（而不是只取一部分）
    #[test]
    fn drain_takes_everything() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = drain_warnings();
        for i in 0..10 {
            push_warning(format!("m{i}"));
        }
        let all = drain_warnings();
        assert_eq!(all.len(), 10);
        assert!(drain_warnings().is_empty(), "取走后缓冲必须为空");
    }
}
