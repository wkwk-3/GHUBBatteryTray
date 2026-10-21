//! ログ (%LOCALAPPDATA%\GHUBBatteryTray\GHUBBatteryTray.log)。
//! 1 行 = "2026-10-22 01:20:00.123 +09:00 [INFO] メッセージ"。1 MB を超えたら .log.old に移して書き直す。
//! Webhook の URL などの秘密はログに書かないこと。

use std::io::Write;
use std::sync::Mutex;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

const MAX_BYTES: u64 = 1024 * 1024;
static LOCK: Mutex<()> = Mutex::new(());

pub fn info(message: impl AsRef<str>) {
    write("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write("WARN", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write("ERROR", message.as_ref());
}

fn path() -> std::path::PathBuf {
    crate::settings::dir().join("GHUBBatteryTray.log")
}

/// 現地時刻と UTC からのずれ
fn timestamp() -> String {
    unsafe {
        let t = GetLocalTime();
        let mut tz = TIME_ZONE_INFORMATION::default();
        // 2 = 夏時間中
        let bias = if GetTimeZoneInformation(&mut tz) == 2 { tz.Bias + tz.DaylightBias } else { tz.Bias + tz.StandardBias };
        let offset = -bias;
        let sign = if offset >= 0 { '+' } else { '-' };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {sign}{:02}:{:02}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds,
            offset.abs() / 60,
            offset.abs() % 60
        )
    }
}

fn write(level: &str, message: &str) {
    let _guard = LOCK.lock();
    let path = path();
    let _ = std::fs::create_dir_all(crate::settings::dir());
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} [{level}] {message}", timestamp());
    }
}
