//! ログ (%LOCALAPPDATA%\GHUBBatteryTray\logs\GHUBBatteryTray.log)。
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

/// 詳細ログ。環境変数 GHBT_DEBUG=1 のときだけ書く
pub fn debug(message: impl AsRef<str>) {
    if debug_enabled() {
        write("DEBUG", message.as_ref());
    }
}

pub fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("GHBT_DEBUG").is_ok_and(|v| v == "1"))
}

/// 毎回起きる失敗を 1 回だけ書く。key ごとに、内容が変わったら WARN、直ったら INFO
pub fn problem(key: &str, problem: Option<String>, recovered: &str) {
    static LAST: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let Ok(mut last) = LAST.lock() else { return };
    let pos = last.iter().position(|(k, _)| k == key);
    match (problem, pos) {
        (Some(p), Some(i)) if last[i].1 == p => {}
        (Some(p), Some(i)) => {
            warn(&p);
            last[i].1 = p;
        }
        (Some(p), None) => {
            warn(&p);
            last.push((key.into(), p));
        }
        (None, Some(i)) => {
            last.remove(i);
            if !recovered.is_empty() {
                info(recovered);
            }
        }
        (None, None) => {}
    }
}

/// パニックしたら場所と内容を書く (release は panic = "abort" なので、ここが最後の手がかり)
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let place = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let text = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        error(format!("異常終了しました ({place}, スレッド {thread}): {text}"));
    }));
}

/// ログを置くフォルダー
pub fn dir() -> std::path::PathBuf {
    crate::settings::dir().join("logs")
}

fn path() -> std::path::PathBuf {
    dir().join("GHUBBatteryTray.log")
}

/// 以前の場所 (設定フォルダー直下) のログを logs に移す
fn migrate() {
    for name in ["GHUBBatteryTray.log", "GHUBBatteryTray.log.old"] {
        let old = crate::settings::dir().join(name);
        let new = dir().join(name);
        if old.exists() && !new.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }
}

/// 現地時刻の UTC からのずれ (秒)
pub fn utc_offset_secs() -> i64 {
    unsafe {
        let mut tz = TIME_ZONE_INFORMATION::default();
        let bias = if GetTimeZoneInformation(&mut tz) == 2 { tz.Bias + tz.DaylightBias } else { tz.Bias + tz.StandardBias };
        -(bias as i64) * 60
    }
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
    static MIGRATED: std::sync::Once = std::sync::Once::new();
    let _ = std::fs::create_dir_all(dir());
    MIGRATED.call_once(migrate);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} [{level}] {message}", timestamp());
    }
}
