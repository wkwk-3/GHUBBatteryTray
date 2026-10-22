//! 起動時に、問い合わせの切り分けに使う環境と設定の概要をログに書く (URL などの秘密は書かない)

use crate::settings::{self, Settings};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::core::{HSTRING, w};

pub fn log_startup(s: &Settings) {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    crate::log::info("I001", format!(
        "環境: {} / 表示倍率 {}% / exe {} / 設定フォルダー {} / 起動オプション {}",
        os_version(),
        unsafe { GetDpiForSystem() } * 100 / 96,
        exe,
        settings::dir().display(),
        if args.is_empty() { "なし".into() } else { args.join(" ") },
    ));

    let rules: usize = s.device_notifications.values().map(Vec::len).sum();
    let on = |b: bool| if b { "オン" } else { "オフ" };
    crate::log::info("I002", format!(
        "設定: テーマ {} / アイコン表示 {} 台 / 通知ルール {} 件 ({} 台) / フルスクリーン中の保留 {} / 更新確認 {} / Webhook {} / Windows 起動時に開始 {} / 詳細ログ {} / 言語 {:?} ({:?})",
        format!("{:?}", s.theme),
        s.tray_display_devices.len(),
        rules,
        s.device_notifications.len(),
        on(s.defer_in_fullscreen),
        on(s.check_updates),
        on(s.webhook_enabled),
        on(crate::startup::enabled()),
        on(crate::log::debug_enabled()),
        s.language,
        crate::i18n::current(),
    ));
}

/// 例: "Windows 11 Pro 24H2 (ビルド 26100.4351)"
fn os_version() -> String {
    let key = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let name = reg_string(key, "ProductName").unwrap_or_else(|| "Windows".into());
    let display = reg_string(key, "DisplayVersion").unwrap_or_default();
    let build: u32 = reg_string(key, "CurrentBuildNumber").and_then(|b| b.parse().ok()).unwrap_or(0);
    // Windows 11 でも ProductName は "Windows 10" のままなのでビルド番号で直す
    let name = if build >= 22000 { name.replace("Windows 10", "Windows 11") } else { name };
    let ubr = reg_dword(key, "UBR").map(|u| format!(".{u}")).unwrap_or_default();
    format!("{name} {display} (ビルド {build}{ubr})").replace("  ", " ")
}

fn reg_string(key: windows::core::PCWSTR, value: &str) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let ok = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, &HSTRING::from(value), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut size)) };
    ok.is_ok().then(|| String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
}

fn reg_dword(key: windows::core::PCWSTR, value: &str) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    let ok = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, &HSTRING::from(value), RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as _), Some(&mut size)) };
    ok.is_ok().then_some(v)
}
