#![windows_subsystem = "windows"]

mod fullscreen;
mod envinfo;
mod ghub;
mod hidpp;
mod health;
mod history;
mod i18n;
mod icon;
mod live;
mod log;
mod migrate;
mod placeholders;
mod settings;
mod sources;
mod startup;
mod stats;
mod tray;
mod updater;
mod webhook;
mod ui;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::HSTRING;

fn main() {
    log::install_panic_hook();
    let settings_mode = std::env::args().any(|a| a == "--settings");

    // 二重起動しない (設定フォルダーごと。テスト用に別フォルダーで動かすときはぶつからない)
    let name = HSTRING::from(format!(r"Local\GHUBBatteryTray.{}.{}", if settings_mode { "Settings" } else { "Tray" }, settings::instance_id()));
    // 更新直後は、古い常駐が終わるまで最大 20 秒待つ
    let after_update = std::env::args().any(|a| a == "--after-update");
    let mut tries = if after_update { 100 } else { 1 };
    let _mutex = loop {
        let mutex = unsafe { CreateMutexW(None, true, &name) };
        if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            break mutex;
        }
        if let Ok(h) = mutex {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
        tries -= 1;
        if tries == 0 {
            if after_update {
                log::warn("W005", "更新前のアプリが終了しないため、起動を中止しました");
            }
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };

    // 表示言語を設定から決める (読み込むと反映される)
    let _ = settings::Settings::load();

    if !settings_mode && !integrity_ok() {
        return;
    }

    if settings_mode { ui::run() } else { tray::run() }
}

/// exe が GitHub で公開しているものと同じか確かめる。一致しなければ知らせて false
fn integrity_ok() -> bool {
    // 開発中 (Debug ビルド / GHBT_SKIP_INTEGRITY=1) は確認しない
    if cfg!(debug_assertions) || std::env::var("GHBT_SKIP_INTEGRITY").is_ok_and(|v| v == "1") {
        return true;
    }
    match updater::verify_self() {
        updater::Integrity::Verified => {
            log::info("I008", format!("exe のハッシュ値が GitHub の v{} と一致しました", updater::CURRENT));
            true
        }
        updater::Integrity::Unverified(reason) => {
            log::warn("W006", format!("exe のハッシュ値を確認できませんでした ({reason})。起動を続けます"));
            true
        }
        updater::Integrity::Mismatch { tag, expected, actual, page_url } => {
            log::error("E004", format!("exe のハッシュ値が GitHub の {tag} と一致しません。起動を中止します (GitHub: {expected} / この exe: {actual})"));
            use windows::Win32::UI::WindowsAndMessaging::*;
            let t = i18n::t();
            let text = HSTRING::from(i18n::f(t.mismatch_fmt, &[&updater::CURRENT, &tag, &expected, &actual]));
            let answer = unsafe { MessageBoxW(None, &text, &HSTRING::from(t.mismatch_title), MB_YESNO | MB_ICONERROR | MB_SETFOREGROUND) };
            if answer == IDYES {
                updater::open_page(Some(&updater::Release { page_url, ..Default::default() }));
            }
            false
        }
    }
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

    /// LOCALAPPDATA を差し替えるテストと、それを読むテストを同時に動かさない
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn settings_db_fallback_reads() {
        let _env = env_lock();
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
        let _env = env_lock();
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
        let _env = env_lock();
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

    #[test]
    fn status_labels_power_as_discharge() {
        let mut b = battery(55, false, Some(10.0));
        b.power_mw = Some(14.74);
        assert!(b.status().contains("消費電力 14.7 mW"), "{}", b.status());
    }

    #[test]
    fn last_run_extracts_discharge_before_charging() {
        use crate::history::Sample;
        let h = 3600;
        let s = |t, percent, charging| Sample { t, percent, charging };
        let samples = vec![s(0, 100, true), s(h, 95, false), s(6 * h, 55, false), s(6 * h + 60, 55, true)];
        let run = crate::health::last_run(&samples, Some(15.0)).unwrap();
        assert_eq!((run.start_pct, run.end_pct), (95, 55));
        assert!((run.hours - 5.0).abs() < 1e-9);
        // 40% を 5 時間 -> 満充電で 12.5 時間、15mW なら 187.5mWh
        assert!((run.full_runtime_h() - 12.5).abs() < 1e-9);
        assert!((run.capacity_mwh().unwrap() - 187.5).abs() < 1e-6);
        // 減りが小さすぎる区間は記録しない
        assert!(crate::health::last_run(&[s(0, 80, false), s(2 * h, 75, false)], None).is_none());
    }

    #[test]
    fn recorder_logs_run_when_charging_starts() {
        let dir = std::env::temp_dir().join(format!("ghbt_health_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("history.csv");
        let _ = std::fs::write(&path, "");
        let _ = std::fs::write(crate::health::path_beside(&path), "");
        let mut rec = crate::history::Recorder::default();
        let mut b = battery(90, false, None);
        b.power_mw = Some(14.0);
        rec.record_to(&path, &[b.clone()], 0);
        b.percentage = 60;
        rec.record_to(&path, &[b.clone()], 4 * 3600);
        b.charging = true;
        rec.record_to(&path, &[b.clone()], 4 * 3600 + 60);
        let runs = crate::health::load_all(&crate::health::path_beside(&path));
        let r = &runs["mouse"][0];
        assert_eq!((r.start_pct, r.end_pct, r.power_mw), (90, 60, Some(14.0)));
    }

    #[test]
    fn health_trend_predicts_80_and_70_percent() {
        use crate::health::{Run, analyze, Basis};
        let day = 86400u64;
        // 30 日で持ち時間が 20h -> 17h (毎回 30% 消費) に落ちていく
        let runs: Vec<Run> = (0..8)
            .map(|i| {
                let runtime = 20.0 - 3.0 * i as f64 / 7.0;
                Run { end: i * 4 * day + day, start_pct: 90, end_pct: 60, hours: runtime * 0.3, power_mw: None }
            })
            .collect();
        let h = analyze(&runs, 30 * day);
        assert!(h.health.unwrap() < 0.95);
        let (t80, basis) = h.degrade_at.unwrap();
        assert_eq!(basis, Basis::Trend);
        assert!(t80 > 30 * day && h.replace_at.unwrap().0 > t80);
        assert!((h.cycles - 2.4).abs() < 1e-9);
    }

    #[test]
    fn health_falls_back_to_cycle_pace() {
        use crate::health::{Run, analyze, Basis};
        let day = 86400u64;
        let runs = vec![Run { end: day, start_pct: 100, end_pct: 50, hours: 10.0, power_mw: Some(15.0) }];
        let h = analyze(&runs, 10 * day);
        let (t80, basis) = h.degrade_at.unwrap();
        assert_eq!(basis, Basis::Cycles);
        assert!(t80 > 10 * day);
        assert_eq!(h.health, None);
        assert_eq!(crate::health::year_month(1_792_627_200), (2026, 10));
    }

    #[test]
    fn no_runs_shows_zero_cycles_not_negative() {
        let h = crate::health::analyze(&[], 1000);
        assert_eq!(format!("{:.1}", h.cycles), "0.0");
    }

    #[test]
    fn level_colors_default_matches_old_thresholds() {
        let c = crate::settings::LevelColors::default();
        assert_eq!(c.for_percent(85), [32, 150, 80]);
        assert_eq!(c.for_percent(51), [32, 150, 80]);
        assert_eq!(c.for_percent(50), [225, 130, 0]);
        assert_eq!(c.for_percent(21), [225, 130, 0]);
        assert_eq!(c.for_percent(20), [214, 48, 49]);
        assert_eq!(c.for_percent(0), [214, 48, 49]);
        let charging = ghub::Battery { charging: true, ..battery(10, true, None) };
        assert_eq!(crate::icon::level_color(Some(&charging), &c), (30, 120, 220));
        assert_eq!(crate::icon::level_color(None, &c), (110, 116, 128));
    }

    #[test]
    fn level_colors_custom_and_normalize() {
        use crate::settings::{LevelColor, LevelColors};
        let mut c = LevelColors { levels: vec![LevelColor { max_percent: 90, color: [1, 1, 1] }, LevelColor { max_percent: 10, color: [2, 2, 2] }], ..Default::default() };
        c.normalize();
        // 小さい順に並び、最後は 100%
        assert_eq!(c.levels.iter().map(|l| l.max_percent).collect::<Vec<_>>(), vec![10, 100]);
        assert_eq!(c.for_percent(5), [2, 2, 2]);
        assert_eq!(c.for_percent(95), [1, 1, 1]);
        // 古い設定ファイル (色の項目なし) は既定の色
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.level_colors, LevelColors::default());
    }

    #[test]
    fn updater_versions() {
        use crate::updater::{parse_version, Release};
        assert_eq!(parse_version("v2.10.3"), Some((2, 10, 3)));
        assert_eq!(parse_version("2.1"), Some((2, 1, 0)));
        assert_eq!(parse_version("v3.0.0-beta"), Some((3, 0, 0)));
        assert_eq!(parse_version("latest"), None);
        assert_eq!(crate::updater::CURRENT, "3.3.0");
        let r = |tag: &str| Release { tag: tag.into(), ..Default::default() };
        assert!(r("v3.3.1").is_newer() && r("v3.4.0").is_newer() && r("v10.0.0").is_newer());
        assert!(!r("v3.3.0").is_newer() && !r("v3.3").is_newer() && !r("v3.2.1").is_newer() && !r("nightly").is_newer());
    }

    #[test]
    fn updater_parses_github_release() {
        let hash = "a".repeat(64);
        let json = serde_json::json!({
            "tag_name": "v2.2.0",
            "html_url": "https://github.com/wkwk-3/GHUBBatteryTray/releases/tag/v2.2.0",
            "body": format!("新機能
SHA256: {}", hash.to_uppercase()),
            "assets": [
                {"name": "README.md", "browser_download_url": "https://x/readme", "size": 10},
                {"name": "GHUBBatteryTray.exe", "browser_download_url": "https://x/GHUBBatteryTray.exe", "size": 1234}
            ]
        });
        let r = crate::updater::parse_release(&json).unwrap();
        assert_eq!((r.tag.as_str(), r.exe_url.as_str(), r.exe_size), ("v2.2.0", "https://x/GHUBBatteryTray.exe", 1234));
        assert_eq!(r.sha256, Some(hash));
        let no_exe = serde_json::json!({"tag_name": "v2.2.0", "assets": []});
        assert!(crate::updater::parse_release(&no_exe).is_err());
    }

    #[test]
    fn updater_verify_and_install() {
        use crate::updater::{install, old_path, verify, Release};
        use sha2::Digest;
        let dir = std::env::temp_dir().join(format!("ghbt_update_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let new_exe = dir.join("new.exe");
        let content = b"MZ new version".to_vec();
        std::fs::write(&new_exe, &content).unwrap();
        let sha: String = sha2::Sha256::digest(&content).iter().map(|b| format!("{b:02x}")).collect();
        let good = Release { exe_size: content.len() as u64, sha256: Some(sha), ..Default::default() };
        assert!(verify(&new_exe, &good).is_ok());
        assert!(verify(&new_exe, &Release { exe_size: 1, ..Default::default() }).is_err());
        assert!(verify(&new_exe, &Release { sha256: Some("0".repeat(64)), ..Default::default() }).is_err());
        std::fs::write(dir.join("not.exe"), b"<html>").unwrap();
        assert!(verify(&dir.join("not.exe"), &Release::default()).is_err());

        // 入れ替え: 今の exe は .old に、新しい中身が同じ場所に
        let current = dir.join("GHUBBatteryTray.exe");
        std::fs::write(&current, b"MZ old version").unwrap();
        install(&new_exe, &current).unwrap();
        assert_eq!(std::fs::read(&current).unwrap(), content);
        assert_eq!(std::fs::read(old_path(&current)).unwrap(), b"MZ old version");
        // 新しい exe が無いときは元に戻る
        let _ = std::fs::remove_file(old_path(&current));
        assert!(install(&dir.join("missing.exe"), &current).is_err());
        assert_eq!(std::fs::read(&current).unwrap(), content);
    }

    /// GitHub の API が返すリリース一覧と同じ形 (2026-10 時点の実データを元にしたもの)
    fn github_releases() -> serde_json::Value {
        serde_json::json!([
            {"tag_name": "v2.1", "draft": false, "html_url": "https://github.com/wkwk-3/GHUBBatteryTray/releases/tag/v2.1", "body": "",
             "assets": [{"name": "GHUBBatteryTray.exe", "size": 5954048, "browser_download_url": "https://x/v2.1/GHUBBatteryTray.exe",
                         "digest": "sha256:c411c77567d9f22c6dbf13d72d881f3154f6ed1d1e735738a0ff8edcf3e46bda"}]},
            {"tag_name": "2.0", "draft": false, "body": "",
             "assets": [{"name": "GHUBBatteryTray.exe", "size": 5778944, "browser_download_url": "https://x/2.0/GHUBBatteryTray.exe",
                         "digest": "sha256:54365574b744745e8295001c53c0d52036242550d0071ec2ce78dba70550c352"}]}
        ])
    }

    #[test]
    fn digest_from_github_is_used_for_download() {
        let list = github_releases();
        let r = crate::updater::parse_release(&list[0]).unwrap();
        assert_eq!(r.sha256.as_deref(), Some("c411c77567d9f22c6dbf13d72d881f3154f6ed1d1e735738a0ff8edcf3e46bda"));
    }

    #[test]
    fn integrity_judgement() {
        use crate::updater::{judge, Integrity};
        let good = "c411c77567d9f22c6dbf13d72d881f3154f6ed1d1e735738a0ff8edcf3e46bda";
        let bad = "0".repeat(64);
        // タグ v2.1 とバージョン 2.1.0 は同じ
        assert_eq!(judge(good, Ok(github_releases()), "2.1.0", ""), Integrity::Verified);
        assert!(matches!(judge(&bad, Ok(github_releases()), "2.1.0", ""), Integrity::Mismatch { ref tag, .. } if tag == "v2.1"));
        // 2.0 のタグも見つかる
        assert!(matches!(judge(&bad, Ok(github_releases()), "2.0.0", ""), Integrity::Mismatch { ref tag, .. } if tag == "2.0"));
        // リリースが無いバージョン / オフラインは起動を止めない
        assert!(matches!(judge(&bad, Ok(github_releases()), "9.9.9", ""), Integrity::Unverified(_)));
        assert!(matches!(judge(&bad, Err("offline".into()), "2.1.0", ""), Integrity::Unverified(_)));
        // オフラインでも、前回一致を確認できたハッシュ値なら確認済み
        assert_eq!(judge(good, Err("offline".into()), "2.1.0", good), Integrity::Verified);
        assert!(matches!(judge(good, Err("offline".into()), "2.1.0", &bad), Integrity::Unverified(_)));
    }

    #[test]
    fn pending_notifications_are_combined() {
        use crate::tray::{combine, Pending};
        let n = |title: &str, text: &str, kind: i32| Pending { title: title.into(), text: text.into(), kind, is_update: false };
        // 1 件ならそのまま
        assert_eq!(combine(&[n("PRO 2", "残り 20%", 2)]), ("PRO 2".into(), "残り 20%".into(), 2));
        // 複数はまとめて、アイコンは一番重いもの
        let (title, text, kind) = combine(&[n("PRO 2", "残り 50%", 1), n("PRO 2", "残り 20%", 3), n("Headset", "充電が完了しました", 1)]);
        assert_eq!(title, "保留中の通知 (3 件)");
        assert_eq!(text, "・PRO 2: 残り 50%
・PRO 2: 残り 20%
・Headset: 充電が完了しました");
        assert_eq!(kind, 3);
        // 長すぎる本文は切り詰める (バルーンは 255 文字まで)
        let many: Vec<Pending> = (0..30).map(|i| n("PRO 2 LIGHTSPEED", &format!("通知 {i}"), 1)).collect();
        let (_, text, _) = combine(&many);
        assert!(text.chars().count() <= 250 && text.ends_with('…'));
    }

    #[test]
    fn defer_in_fullscreen_defaults_on() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.defer_in_fullscreen);
    }

    #[test]
    fn placeholders_expand_all_keys() {
        use crate::placeholders::{expand, Context, Pace};
        let h = 3600u64;
        let now = 1_792_627_200 + 9 * h; // 2026-10-22 18:00 (JST)
        let ctx = Context {
            name: "PRO 2".into(),
            threshold: Some(20),
            now_pct: Some(18),
            remain_h: Some(3.2),
            empty_at: Some(now + 3 * h + 40 * 60),
            rate: Some(5.25),
            power_mw: Some(14.74),
            source: Some("直接通信"),
            time: now,
            since_secs: Some(9 * h),
            health: Some(0.97),
            cycles: Some(7.9),
            replace_at: Some(1_812_000_000),
            pace: Some(Pace::Faster),
            streak_secs: Some(3 * 24 * h),
            utc_offset: 9 * 3600,
        };
        let t = "{name}|{battery}|{now}|{remain}|{empty_at}|{rate}|{power}|{source}|{time}|{since}|{health}|{cycles}|{replace}|{bar}|{pace}|{streak}|{rn}|{unknown}|{";
        assert_eq!(
            expand(t, &ctx),
            "PRO 2|20|18|約3時間|21:40ごろ|-5.2%/時|14.7 mW|直接通信|18:00|9時間|97%|7.9回|2027年6月ごろ|■■□□□□□□□□|平均より速い|3日|\r\n|{unknown}|{"
        );
        // 分からない値は「不明」
        let empty = Context { name: "X".into(), time: now, utc_offset: 9 * 3600, ..Default::default() };
        assert_eq!(expand("{now}/{remain}/{pace}/{health}", &empty), "不明/不明/不明/不明");
    }

    #[test]
    fn placeholders_formatting() {
        use crate::placeholders::{bar_text, clock_text, duration_text};
        assert_eq!(bar_text(0), "□□□□□□□□□□");
        assert_eq!(bar_text(100), "■■■■■■■■■■");
        assert_eq!(bar_text(55), "■■■■■■□□□□");
        assert_eq!(duration_text(30 * 60), "30分");
        assert_eq!(duration_text(9 * 3600), "9時間");
        assert_eq!(duration_text(3 * 86400), "3日");
        let now = 1_792_627_200 + 9 * 3600; // 18:00 JST
        assert_eq!(clock_text(now + 3600, now, 9 * 3600), "19:00");
        assert_eq!(clock_text(now + 12 * 3600, now, 9 * 3600), "明日 6:00");
        assert_eq!(clock_text(now + 50 * 3600, now, 9 * 3600), "10/24 20:00");
    }

    #[test]
    fn placeholder_candidates_order() {
        use crate::placeholders::{candidates, DEFS};
        let keys = |q: &str| candidates(q).iter().map(|d| d.key).collect::<Vec<_>>();
        // 何も入力していなければ名前順で全部
        assert_eq!(keys("").len(), DEFS.len());
        assert_eq!(keys("")[..3], ["bar", "battery", "cycles"]);
        // 前方一致 → 部分一致 の順
        assert_eq!(keys("s")[..3], ["since", "source", "streak"]);
        assert_eq!(keys("re")[..2], ["remain", "replace"]);
        assert_eq!(keys("at")[..3], ["rate", "battery", "empty_at"]); // 前方一致なし → 部分一致を近い順 (編集距離)
        // 説明文でも探せる
        assert_eq!(keys("残量")[0], "bar");
        // 綴りが近いもの (順番どおりに文字を含む)
        assert_eq!(keys("pwr")[0], "power");
    }

    #[test]
    fn placeholder_context_from_history() {
        use crate::history::Sample;
        use crate::placeholders::{Context, Pace};
        let h = 3600u64;
        let now = 100 * h;
        let mut b = battery(70, false, Some(10.0));
        b.power_mw = Some(14.7);
        // 2 時間前に充電を外し、そこから 1 時間に 10% ずつ減っている
        let samples = vec![Sample { t: now - 2 * h - 60, percent: 90, charging: true }, Sample { t: now - 2 * h, percent: 90, charging: false }, Sample { t: now - h, percent: 80, charging: false }];
        // いつもは 1 時間に 5% (満充電で 20 時間)
        let runs: Vec<crate::health::Run> = (0..3).map(|i| crate::health::Run { end: i * 24 * h, start_pct: 100, end_pct: 50, hours: 10.0, power_mw: Some(14.7) }).collect();
        let ctx = Context::build(&b, Some(20), &samples, &runs, &[], now);
        assert_eq!(ctx.pace, Some(Pace::Faster));
        assert_eq!(ctx.since_secs, Some(2 * h + 60));
        assert_eq!(ctx.streak_secs, Some(10 * h)); // 記録の 10 時間 > 今の 2 時間
        assert_eq!(ctx.value("now").as_deref(), Some("70"));
    }

    #[test]
    fn language_from_windows() {
        use crate::i18n::{from_langid, Language::*};
        // 対応言語 (英語は地域で米国式/英国式) と、それ以外は英語 (US)
        for (id, l) in [(0x0411, Ja), (0x0409, EnUs), (0x0809, EnGb), (0x0C09, EnGb), (0x1009, EnUs), (0x0407, De), (0x0C07, De), (0x040C, Fr), (0x0C0C, Fr), (0x0412, Ko), (0x0804, ZhCn), (0x0404, ZhCn), (0x0410, EnUs), (0x0419, EnUs)] {
            assert_eq!(from_langid(id), l, "{id:#06x}");
        }
    }

    #[test]
    fn every_language_formats() {
        use crate::i18n::{f, texts, CHOICES};
        for l in CHOICES {
            let t = texts(l);
            // 置き換え番号が残らない
            for (fmt, n) in [(t.pending_fmt, 1), (t.empty_in_fmt, 3), (t.mismatch_fmt, 4), (t.month_year_fmt, 2), (t.date_md_fmt, 2), (t.err_size_fmt, 2)] {
                let args: Vec<String> = (0..n).map(|i| format!("<{i}>")).collect();
                let refs: Vec<&dyn std::fmt::Display> = args.iter().map(|a| a as &dyn std::fmt::Display).collect();
                let out = f(fmt, &refs);
                assert!(!out.contains('{') || fmt.contains("\"title\""), "{l:?}: {out}");
                for a in &args {
                    assert!(out.contains(a.as_str()), "{l:?}: {fmt} に {a} がない");
                }
            }
            assert!(t.default_rule_message.contains("{name}") && t.default_rule_message.contains("{battery}"), "{l:?}");
        }
    }

    #[test]
    fn log_ids_are_unique() {
        // ログの ID (E001 など) は 1 か所でだけ使う
        let sources = [
            include_str!("envinfo.rs"), include_str!("ghub.rs"), include_str!("health.rs"), include_str!("hidpp.rs"), include_str!("live.rs"), include_str!("migrate.rs"), include_str!("history.rs"),
            include_str!("log.rs"), include_str!("main.rs"), include_str!("settings.rs"), include_str!("sources.rs"), include_str!("startup.rs"), include_str!("stats.rs"),
            include_str!("tray.rs"), include_str!("ui.rs"), include_str!("updater.rs"), include_str!("webhook.rs"),
        ];
        let mut seen = std::collections::HashSet::new();
        for src in sources {
            let code = src.split("#[cfg(test)]").next().unwrap_or_default();
            for (i, _) in code.match_indices('"') {
                let id = &code[i + 1..];
                let ok = id.len() > 5 && matches!(id.as_bytes()[0], b'E' | b'W' | b'I' | b'D') && id.as_bytes()[1..4].iter().all(u8::is_ascii_digit) && id.as_bytes()[4] == b'"';
                if ok {
                    assert!(seen.insert(&id[..4]), "ID {} が重複しています", &id[..4]);
                }
            }
        }
        assert!(seen.len() >= 70, "{}", seen.len());
    }

    fn stats_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ghbt_stats_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn mouse(pct: i32, charging: bool, power: Option<f64>) -> crate::ghub::Battery {
        crate::ghub::Battery { device_name: "mouse".into(), percentage: pct, connected: true, charging, power_mw: power, ..Default::default() }
    }

    #[test]
    fn stats_records_hourly_with_exact_totals() {
        use crate::stats::{Recorder, load, path_in};
        let dir = stats_dir("hourly");
        let mut r = Recorder::new(dir.clone());
        // 10:30 から 3 時間、1 分ごとに読み取り。30 分で 1% ずつ減る (放電量 15 mW)
        let t0 = 1_800_000_000u64 / 3600 * 3600 + 1800;
        for m in 0..=180u64 {
            let pct = 90 - (m / 30) as i32;
            r.observe(&[mouse(pct, false, Some(15.0))], t0 + m * 60);
        }
        r.flush_all(t0 + 180 * 60);
        let hours = load(&path_in(&dir, "mouse"));
        assert_eq!(hours.len(), 4, "{hours:?}");
        let use_secs: u32 = hours.iter().map(|h| h.use_secs as u32).sum();
        let drain: u32 = hours.iter().map(|h| h.drain_m as u32).sum();
        assert_eq!(use_secs, 180 * 60);
        assert_eq!(drain, 6000); // 6% (整数 % の読み取りでも合計はずれない)
        assert!(hours.iter().all(|h| h.power_cw == 1500 || h.power_secs == 0));
        // 時の境目で分かれている (最初の時は 30 分ぶん)
        assert_eq!(hours[0].use_secs, 1800);
        assert_eq!(hours[0].hour as u64, t0 / 3600);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stats_counts_unobserved_drop_as_gap() {
        use crate::stats::{Recorder, load, path_in};
        let dir = stats_dir("gap");
        let mut r = Recorder::new(dir.clone());
        let t0 = 1_800_000_000u64;
        r.observe(&[mouse(80, false, None)], t0);
        // 8 時間読めなかった間に 3% 減っていた
        r.observe(&[mouse(77, false, None)], t0 + 8 * 3600);
        r.flush_all(t0 + 8 * 3600);
        let hours = load(&path_in(&dir, "mouse"));
        assert_eq!(hours.iter().map(|h| h.gap_m as u32).sum::<u32>(), 3000);
        assert_eq!(hours.iter().map(|h| h.use_secs as u32).sum::<u32>(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stats_file_overwrites_same_hour_and_drops_broken_tail() {
        use crate::stats::{Recorder, load, path_in};
        let dir = stats_dir("upsert");
        let mut r = Recorder::new(dir.clone());
        let t0 = 1_800_000_000u64 / 3600 * 3600;
        r.observe(&[mouse(50, false, None)], t0);
        r.observe(&[mouse(49, false, None)], t0 + 600);
        r.flush_all(t0 + 600);
        r.observe(&[mouse(48, false, None)], t0 + 1200);
        r.flush_all(t0 + 1200);
        let path = path_in(&dir, "mouse");
        assert_eq!(load(&path).len(), 1, "同じ時は上書き");
        // 途中で落ちて半端なバイトが残っても、次の書き込みで捨てる
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&[1, 2, 3]).unwrap();
        assert_eq!(load(&path).len(), 1);
        r.observe(&[mouse(47, false, None)], t0 + 3600 + 60);
        r.flush_all(t0 + 3600 + 60);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 8 + 20 * 2);
        let _ = std::fs::remove_dir_all(dir);
        use std::io::Write;
    }

    /// 合成データ: days 日、毎日 20〜24 時に使用。容量は 1 日 0.1% ずつ減る (放電量 15 mW)
    fn synthetic_hours(days: u64, start: u64) -> Vec<crate::stats::Hour> {
        let mut v = Vec::new();
        for d in 0..days {
            let cap = 300.0 * (1.0 - 0.001 * d as f64); // mWh
            let per_hour = 15.0 / cap * 100.0; // 使用中 1 時間で減る %
            for hr in 0..24u64 {
                let t = start + d * 86400 + hr * 3600;
                let mut h = crate::stats::Hour { hour: (t / 3600) as u32, gap_m: 50, ..Default::default() };
                if (20..24).contains(&hr) {
                    h.use_secs = 3600;
                    h.drain_m = (per_hour * 1000.0) as u16;
                    h.power_cw = 1500;
                    h.power_secs = 3600;
                    h.power_drain_m = h.drain_m;
                }
                v.push(h);
            }
        }
        v
    }

    #[test]
    fn stats_estimates_capacity_health_and_dates() {
        let start = 1_800_000_000u64 / 86400 * 86400;
        let days = 120;
        let hours = synthetic_hours(days, start);
        let now = start + days * 86400;
        let h = crate::stats::analyze(&hours, &[], now, 0);
        // 直近 30 日の容量は 300 × (1 - 0.001 × 約 105) 前後
        let cap = h.capacity_mwh.unwrap();
        assert!((cap - 300.0 * (1.0 - 0.001 * 105.0)).abs() < 3.0, "{cap}");
        let health = h.health.unwrap();
        assert!(health < 0.93 && health > 0.85, "{health}");
        let (deg, basis) = h.degrade_at.unwrap();
        assert_eq!(basis, crate::health::Basis::Trend);
        // 80% になるのは開始から約 200 日後
        let deg_day = (deg - start) as f64 / 86400.0;
        assert!((deg_day - 200.0).abs() < 15.0, "{deg_day}");
        assert!(h.replace_at.unwrap().0 > deg);
        // 時間帯ごとの減り方: 使う 20〜23 時が高い
        let p = h.profile.unwrap();
        assert!(p[21] > p[3] * 10.0, "{p:?}");
        assert!(h.daily_drain.unwrap() > 10.0);
        assert_eq!(h.daily.len(), 30);
        // 予測は 0% まで届く
        let f = crate::stats::forecast(&h, now, 50.0, None, 0).unwrap();
        assert!(crate::stats::empty_at(&f).is_some());
    }

    #[test]
    fn stats_needs_days_before_health() {
        let start = 1_800_000_000u64 / 86400 * 86400;
        let hours = synthetic_hours(5, start);
        let h = crate::stats::analyze(&hours, &[], start + 5 * 86400, 0);
        assert!(h.health.is_none());
        assert_eq!(h.need_days, Some(23));
        // 寿命の目安は充電ペースから
        assert_eq!(h.degrade_at.unwrap().1, crate::health::Basis::Cycles);
    }

    #[test]
    fn hidpp_notices_from_real_device() {
        use crate::hidpp::{Notice, parse_notice};
        // PRO 2 LIGHTSPEED (受信機の番号 1、統合バッテリー 0x1004 は機能番号 7) で実際に届いたデータ
        let battery = [(1u8, 0x1004u16, 7u8)];
        let charging = [0x11, 0x01, 0x07, 0x00, 0x25, 0x04, 0x01, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_notice(&charging, &battery), Some(Notice::Battery(1, 37, true, false)));
        let discharging = [0x11, 0x01, 0x07, 0x00, 0x30, 0x04, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_notice(&discharging, &battery), Some(Notice::Battery(1, 48, false, false)));
        let full = [0x11, 0x01, 0x07, 0x00, 0x64, 0x08, 0x03, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_notice(&full, &battery), Some(Notice::Battery(1, 100, true, true)));
        // マウスを動かしたときの別の機能 (0x10) の通知は無視する
        let other = [0x11, 0x01, 0x10, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_notice(&other, &battery), None);
        // 取得スレッドへの応答 (software id 付き) は通知ではない
        let reply = [0x11, 0x01, 0x07, 0x1B, 0x25, 0x04, 0x01, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_notice(&reply, &battery), None);
        // 受信機の接続通知 (短い報告): bit6 が立っていれば切断
        assert_eq!(parse_notice(&[0x10, 0x01, 0x41, 0x04, 0x01, 0x8A, 0x40], &battery), Some(Notice::Link(1, true)));
        assert_eq!(parse_notice(&[0x10, 0x01, 0x41, 0x04, 0x41, 0x8A, 0x40], &battery), Some(Notice::Link(1, false)));
    }

    #[test]
    fn status_shows_charge_power_and_time_to_full() {
        let b = crate::ghub::Battery { connected: true, charging: true, percentage: 75, charge_rate: Some(97.0), charge_power_mw: Some(271.0), ..Default::default() };
        assert_eq!(b.status(), "75% / 充電中 / 充電電力 約271 mW / 満充電まで約15分");
        // 上がり方がまだ分からないうちは「充電中」だけ
        let early = crate::ghub::Battery { connected: true, charging: true, percentage: 75, ..Default::default() };
        assert_eq!(early.status(), "75% / 充電中");
    }

    #[test]
    fn same_device_across_hid_and_ghub() {
        use crate::ghub::Battery;
        use crate::sources::same_device;
        // G703: 名前が違う (直接通信は長い正式名、G HUB は短い表示名と別の内部名)
        let hid = Battery { device_name: "g703lightspeedwirelessgamingmousewhero".into(), display_name: "G703 LIGHTSPEED Wireless Gaming Mouse w/ HERO".into(), ..Default::default() };
        let ghub = Battery { device_name: "g703hero".into(), display_name: "G703 LIGHTSPEED".into(), ..Default::default() };
        assert!(same_device(&hid, &ghub), "名前の先頭一致");
        // unitId が分かれば、名前に関係なく unitId で判断する (PRO 2 LIGHTSPEED の実機の値)
        let hid2 = Battery { display_name: "PRO 2 LIGHTSPEED".into(), unit_id: Some("748211122".into()), pids: vec![0x40A8, 0xC09A], ..Default::default() };
        let ghub2 = Battery { device_name: "pro2wirelessmouse".into(), display_name: "Something else".into(), unit_id: Some("748211122".into()), pids: vec![16552], ..Default::default() };
        assert!(same_device(&hid2, &ghub2));
        // 同じ機種でも別の個体 (unitId が違う) はまとめない
        let other = Battery { unit_id: Some("123".into()), ..ghub2.clone() };
        assert!(!same_device(&hid2, &other));
        // unitId が片方にしかなければ製品 ID で判断する
        let ghub3 = Battery { unit_id: None, ..ghub2.clone() };
        assert!(same_device(&hid2, &ghub3));
        // 関係ないデバイス
        let headset = Battery { device_name: "proxwireless".into(), display_name: "PRO X Wireless".into(), ..Default::default() };
        assert!(!same_device(&hid, &headset));
    }

    #[test]
    fn voltage_estimate_uses_ghub_level_while_charging() {
        use crate::ghub::Battery;
        // G703 の実際のログ: 電圧からの推定は充電中に 100%、G HUB は 65%
        let mut hid = Battery { device_name: "g703lightspeedwirelessgamingmousewhero".into(), percentage: 100, charging: true, connected: true, voltage_estimate: true, ..Default::default() };
        let ghub = Battery { device_name: "g703hero".into(), display_name: "G703 LIGHTSPEED".into(), percentage: 65, charging: true, connected: true, ..Default::default() };
        crate::sources::merge(&mut hid, &ghub);
        assert_eq!((hid.percentage, hid.charging, hid.fully_charged), (65, true, false));
        assert_eq!(hid.device_name, "g703hero", "設定・統計は G HUB の名前にまとまる");
        // 残量を % で返すデバイスは直接通信の値のまま
        let mut unified = Battery { percentage: 40, connected: true, ..Default::default() };
        crate::sources::merge(&mut unified, &ghub);
        assert_eq!(unified.percentage, 40);
    }

    #[test]
    fn merge_split_records_into_ghub_name() {
        use crate::migrate::{merge_health, merge_history, merge_settings, merge_stats};
        use crate::stats::{Hour, load, path_in, save_all};
        let dir = stats_dir("merge");
        let old = "g703lightspeedwirelessgamingmousewhero";
        let new = "g703hero";
        let h0 = 1_800_000_000u32 / 3600;

        // 統計: 2 時間は両方の名前で記録 (同じ時間の二重記録)、1 時間は古い名前だけ
        let rec = |hour: u32, use_secs: u16, drain_m: u16| Hour { hour, use_secs, drain_m, ..Default::default() };
        save_all(&path_in(&dir, old), &[rec(h0, 3600, 5000), rec(h0 + 1, 1800, 2000), rec(h0 + 5, 3600, 4000)]).unwrap();
        save_all(&path_in(&dir, new), &[rec(h0, 3000, 4800), rec(h0 + 1, 3600, 4100)]).unwrap();
        assert_eq!(merge_stats(&path_in(&dir, old), &path_in(&dir, new)).unwrap(), 1);
        let merged = load(&path_in(&dir, new));
        assert_eq!(merged.iter().map(|h| (h.hour - h0, h.use_secs)).collect::<Vec<_>>(), vec![(0, 3600), (1, 3600), (5, 3600)], "同じ時は観測が長いほう");
        assert!(!path_in(&dir, old).exists() && path_in(&dir, old).with_extension("bin.merged").exists());

        // 履歴: G HUB 側の記録の近く (15 分以内) の行は重複として捨て、離れた時間の行だけ名前を書き換えて残す
        let t = 1_800_000_000u64;
        let history = dir.join("history.csv");
        std::fs::write(&history, format!(
            "{t},{new},65,1\n{},{old},100,1\n{},{old},90,0\n{},mouse,50,0\n",
            t + 60, t + 3 * 3600, t + 100
        )).unwrap();
        assert_eq!(merge_history(&history, old, new).unwrap(), 1);
        let text = std::fs::read_to_string(&history).unwrap();
        assert!(!text.contains(old));
        assert!(text.contains(&format!("{},{new},90,0", t + 3 * 3600)) && text.contains("mouse,50,0"));

        // 放電の記録: G HUB 側に終わりが近い記録があれば捨てる
        let health = dir.join("health.csv");
        std::fs::write(&health, format!("{t},{new},90,40,10.000,\n{},{old},95,45,10.000,\n{},{old},80,30,8.000,\n", t + 600, t + 86400)).unwrap();
        assert_eq!(merge_health(&health, old, new).unwrap(), 1);
        let runs = crate::health::load_all(&health);
        assert!(!runs.contains_key(old));
        assert_eq!(runs[new].len(), 2);

        // 設定: 古い名前の通知ルールのうち、G HUB 側に無いものだけ足す。トレイ表示は G HUB 側の名前に
        let mut s = crate::settings::Settings::default();
        let rule = |battery: i32, message: &str| crate::settings::Rule { enabled: true, battery, message: message.into(), balloon_tip_icon: 2 };
        s.device_notifications.insert(new.into(), vec![rule(20, "低い")]);
        s.device_notifications.insert(old.into(), vec![rule(20, "低い"), rule(5, "もうすぐ切れる")]);
        s.set_tray(old, true);
        assert!(merge_settings(&mut s, old, new));
        assert_eq!(s.device_notifications[new].len(), 2);
        assert!(!s.device_notifications.contains_key(old));
        assert!(s.is_tray(new) && !s.is_tray(old));
        // 2 回目は何もしない
        assert!(!merge_settings(&mut s, old, new));
        assert_eq!(merge_history(&history, old, new).unwrap(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
