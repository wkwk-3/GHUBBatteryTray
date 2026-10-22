//! Windows 起動時に開始 (HKCU\Software\Microsoft\Windows\CurrentVersion\Run)

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::*;
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("GHUBBatteryTray");

pub fn enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set(enable: bool) {
    let result = unsafe {
        if enable {
            let exe = std::env::current_exe().unwrap_or_default();
            let value: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ.0, Some(value.as_ptr() as _), (value.len() * 2) as u32)
        } else {
            // もともと登録がなければ成功扱い
            match RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE) {
                ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
                e => e,
            }
        }
    };
    let label = if enable { "オン" } else { "オフ" };
    if result.is_ok() {
        crate::log::info(format!("Windows 起動時に開始: {label}"));
    } else {
        crate::log::error(format!("Windows 起動時に開始を{label}にできませんでした: {}", result.to_hresult().message()));
    }
}
