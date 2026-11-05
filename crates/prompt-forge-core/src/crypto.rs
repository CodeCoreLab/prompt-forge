//! API Key DPAPI 加密（P2-3）。
//!
//! Windows：CryptProtectData/CryptUnprotectData（当前用户作用域，跨进程不可解）。
//! 存储格式：`enc:<base64>`；读取时识别前缀解密，明文原样透传（向后兼容旧配置）。
//! 非 Windows：明文透传（桌面定位以 Windows 为主）。
//!
//! 实现用 Crypt32.dll 的 C ABI 直调（fn! 类型别名声明签名），不依赖 windows crate
//! 的版本敏感包装，规避 LocalFree/Option<Blob> 等签名漂移问题。

use base64::Engine as _;

const PREFIX: &str = "enc:";

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

    fn crypt32() -> &'static libloading::Library {
        use std::sync::OnceLock;
        static LIB: OnceLock<libloading::Library> = OnceLock::new();
        LIB.get_or_init(|| {
            unsafe { libloading::Library::new("crypt32.dll") }.expect("crypt32.dll 加载失败")
        })
    }

    /// 明文（UTF-16）→ DPAPI 密文字节
    pub fn protect_bytes(plain: &str) -> Result<Vec<u8>, String> {
        let lib = crypt32();
        let f: libloading::Symbol<CryptProtectDataFn> = unsafe {
            lib.get(b"CryptProtectData").map_err(|e| format!(" CryptProtectData 加载失败: {e}"))?
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
        let lib = crypt32();
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

    fn free_blob(b: CryptBlob) {
        if !b.pb_data.is_null() {
            let k32: libloading::Library =
                unsafe { libloading::Library::new("kernel32.dll").expect("kernel32") };
            let f: libloading::Symbol<LocalFreeFn> =
                unsafe { k32.get(b"LocalFree").expect("LocalFree") };
            unsafe { f(b.pb_data as *mut _) };
        }
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

/// 读取侧归一：加密形态 → 解密；明文 → 原样（兼容旧配置）。解密失败返回空串。
pub fn load_key(stored: &str) -> String {
    if is_encrypted(stored) {
        unprotect(stored).unwrap_or_default()
    } else {
        stored.to_string()
    }
}

/// 保存侧归一：明文 → 加密落盘（空串跳过）。加密失败降级明文（可用性优先，记告警）。
pub fn store_key(plain: &str) -> String {
    if plain.is_empty() {
        return String::new();
    }
    match protect(plain) {
        Ok(enc) => enc,
        Err(e) => {
            eprintln!("API Key 加密失败，降级明文存储: {e}");
            plain.to_string()
        }
    }
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
        assert_eq!(load_key(&enc), plain);
    }

    #[test]
    fn plaintext_passthrough() {
        assert_eq!(load_key("sk-plain"), "sk-plain");
        assert_eq!(store_key(""), "");
    }

    #[test]
    fn is_encrypted_check() {
        assert!(is_encrypted("enc:abcd"));
        assert!(!is_encrypted("sk-plain"));
    }
}
