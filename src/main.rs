#![windows_subsystem = "windows"]

mod ghub;
mod hidpp;
mod history;
mod icon;
mod settings;
mod sources;
mod tray;
mod webhook;
mod ui;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

fn main() {
    let settings_mode = std::env::args().any(|a| a == "--settings");
    let name = if settings_mode { w!(r"Local\GHUBBatteryTray.Settings") } else { w!(r"Local\GHUBBatteryTray.Tray") };

    // 二重起動しない
    let _mutex = unsafe { CreateMutexW(None, true, name) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }

    if settings_mode { ui::run() } else { tray::run() }
}

#[cfg(test)]
mod tests {
    use crate::{ghub, hidpp, settings::Settings, sources};

    #[test]
    #[ignore = "実機テスト: 無線マウスがスリープしていないときに cargo test -- --ignored で実行"]
    fn hid_direct_reads_battery() {
        let mut reader = hidpp::Reader::new();
        let t = std::time::Instant::now();
        let list = reader.read_all();
        let first = t.elapsed();
        let t = std::time::Instant::now();
        let again = reader.read_all();
        println!("scan={first:?} cached={:?} {list:#?}", t.elapsed());
        assert_eq!(list.len(), again.len());
        assert!(list.iter().any(|b| (0..=100).contains(&b.percentage)));
    }

    #[test]
    fn agent_returns_batteries() {
        let list = ghub::fetch_agent().expect("G HUB API");
        assert!(list.iter().any(|b| b.connected && (0..=100).contains(&b.percentage)));
    }

    #[test]
    fn settings_db_fallback_reads() {
        assert!(!ghub::read_settings_db().expect("settings.db").is_empty());
    }

    #[test]
    #[ignore = "実機テスト: 無線マウスがスリープしていないときに cargo test -- --ignored で実行"]
    fn collect_prefers_hid_and_keeps_settings_key() {
        let list = sources::collect(&mut hidpp::Reader::new());
        println!("{list:#?}");
        let s = Settings::load();
        let tray = list.iter().find(|b| s.is_tray(&b.device_name)).expect("tray device key kept");
        assert_eq!(tray.source, Some(ghub::Source::Hid));
    }

    #[test]
    fn existing_settings_load() {
        let s = Settings::load();
        assert!(s.tray_display_devices.iter().any(|d| s.rules(d).is_some_and(|r| !r.is_empty())));
    }

    /// ローカルに受け口を立てて、curl.exe 経由で JSON が届くか確かめる
    fn capture_post(format: crate::settings::WebhookFormat) -> (Result<u16, String>, String) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/hook", listener.local_addr().unwrap().port());
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            sock.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).unwrap_or(0);
                data.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&data).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len: usize = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                    if body.len() >= len || n == 0 {
                        sock.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n").unwrap();
                        return body.to_string();
                    }
                }
                if n == 0 { return String::new(); }
            }
        });
        let m = crate::webhook::Message { title: "PRO 2 LIGHTSPEED".into(), text: "残り 20% です".into(), device: "pro2wirelessmouse".into(), battery: 20, event: "low_battery" };
        let result = crate::webhook::send(&url, format, &m);
        (result, server.join().unwrap())
    }

    #[test]
    fn webhook_json_is_posted() {
        let (result, body) = capture_post(crate::settings::WebhookFormat::Json);
        println!("{result:?} {body}");
        assert_eq!(result, Ok(204));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["battery"], 20);
        assert_eq!(v["event"], "low_battery");
        assert_eq!(v["message"], "残り 20% です");
    }

    #[test]
    fn webhook_discord_format() {
        let (result, body) = capture_post(crate::settings::WebhookFormat::Discord);
        println!("{result:?} {body}");
        assert_eq!(result, Ok(204));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["content"].as_str().unwrap().contains("残り 20% です"));
    }

    #[test]
    fn webhook_rejects_bad_url() {
        let m = crate::webhook::Message { title: "t".into(), text: "t".into(), device: "d".into(), battery: 1, event: "test" };
        assert!(crate::webhook::send("ftp://x", crate::settings::WebhookFormat::Json, &m).is_err());
        assert!(crate::webhook::send("http://127.0.0.1:1/", crate::settings::WebhookFormat::Json, &m).is_err());
    }

    #[test]
    fn old_single_tray_setting_migrates_to_list() {
        let s: Settings = serde_json::from_str(r#"{"TrayDisplayDevice":"pro2wirelessmouse"}"#).unwrap();
        let path = std::env::temp_dir().join("ghbt_migrate_test");
        let _ = std::fs::create_dir_all(path.join("GHUBBatteryTray"));
        std::fs::write(path.join("GHUBBatteryTray").join("settings.json"), serde_json::to_string(&s).unwrap()).unwrap();
        // Settings::load は LOCALAPPDATA を見るので、このテストの間だけ差し替える
        let old = std::env::var("LOCALAPPDATA").unwrap();
        unsafe { std::env::set_var("LOCALAPPDATA", &path) };
        let loaded = Settings::load();
        unsafe { std::env::set_var("LOCALAPPDATA", old) };
        assert_eq!(loaded.tray_display_devices, vec!["pro2wirelessmouse".to_string()]);
    }

    #[test]
    fn set_tray_adds_removes_and_keeps_first_for_old_versions() {
        let mut s = Settings::default();
        s.set_tray("mouse", true);
        s.set_tray("headset", true);
        s.set_tray("MOUSE", true); // 大文字小文字違いは重複させない
        assert_eq!(s.tray_display_devices, vec!["headset".to_string(), "MOUSE".to_string()]);
        assert!(s.is_tray("mouse") && s.is_tray("headset"));
        s.set_tray("headset", false);
        assert_eq!(s.tray_display_devices, vec!["MOUSE".to_string()]);
    }

    #[test]
    fn icon_slots_one_per_device_or_placeholder() {
        let mut s = Settings::default();
        assert_eq!(crate::tray::icon_slots(&s), vec![String::new()]);
        s.set_tray("mouse", true);
        s.set_tray("headset", true);
        assert_eq!(crate::tray::icon_slots(&s), vec!["mouse".to_string(), "headset".to_string()]);
    }

    #[test]
    fn theme_defaults_to_system() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.theme, crate::settings::Theme::System);
        assert_eq!(Settings::default().theme, crate::settings::Theme::System);
    }

    fn battery(percent: i32, charging: bool, remaining: Option<f64>) -> ghub::Battery {
        ghub::Battery { device_name: "mouse".into(), percentage: percent, charging, connected: true, remaining_hours: remaining, ..Default::default() }
    }

    #[test]
    fn history_records_changes_and_loads_back() {
        let path = std::env::temp_dir().join(format!("ghbt_history_{}.csv", std::process::id()));
        let _ = std::fs::write(&path, "");
        let mut rec = crate::history::Recorder::default();
        rec.record_to(&path, &[battery(80, false, None)], 1000);
        rec.record_to(&path, &[battery(80, false, None)], 1060); // 変化なし -> 書かない
        rec.record_to(&path, &[battery(79, false, None)], 1120);
        rec.record_to(&path, &[battery(79, false, None)], 1120 + 15 * 60); // 15 分経過 -> 書く
        let map = crate::history::load_all(&path);
        let v = &map["mouse"];
        assert_eq!(v.iter().map(|s| (s.t, s.percent)).collect::<Vec<_>>(), vec![(1000, 80), (1120, 79), (2020, 79)]);
    }

    #[test]
    fn trend_uses_actual_slope_after_last_charge() {
        use crate::history::{Sample, trend};
        let h = 3600;
        let samples = vec![
            Sample { t: 0, percent: 100, charging: true },  // 充電中 -> ここより前は使わない
            Sample { t: h, percent: 90, charging: false },
            Sample { t: 2 * h, percent: 85, charging: false },
            Sample { t: 3 * h, percent: 80, charging: false },
        ];
        let tr = trend(&samples, 4 * h, &battery(75, false, Some(100.0)));
        assert_eq!(tr.actual.len(), 4); // 1h,2h,3h + 現在
        let rate = tr.drain_per_hour.unwrap();
        assert!((rate - 5.0).abs() < 0.01, "{rate}");
        assert_eq!(tr.empty_at(4 * h, 75), Some(4 * h + 15 * h)); // 75% / 5%毎時 = 15 時間後
    }

    #[test]
    fn trend_falls_back_to_ghub_remaining_time() {
        let tr = crate::history::trend(&[], 10_000, &battery(60, false, Some(12.0)));
        assert!((tr.drain_per_hour.unwrap() - 5.0).abs() < 0.01);
        let charging = crate::history::trend(&[], 10_000, &battery(60, true, Some(12.0)));
        assert_eq!(charging.drain_per_hour, None);
    }
}
