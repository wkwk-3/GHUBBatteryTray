//! 設定ファイル (%LOCALAPPDATA%\GHUBBatteryTray\settings.json)。C# 版と同じ形式を読み書きする。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Rule {
    pub enabled: bool,
    pub battery: i32,
    pub message: String,
    /// 0=None 1=Info 2=Warning 3=Error (WinForms の ToolTipIcon と同じ値)
    pub balloon_tip_icon: i32,
}

impl Default for Rule {
    fn default() -> Self {
        Rule { enabled: true, battery: 20, message: "{name} のバッテリー残量が {battery}% を下回りました".into(), balloon_tip_icon: 2 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Settings {
    /// 旧バージョン互換 (先頭の 1 台)
    pub tray_display_device: String,
    /// トレイに表示するデバイス。1 台ごとに別のアイコンを出す
    pub tray_display_devices: Vec<String>,
    /// 旧バージョン互換
    pub selected_device: String,
    /// 旧バージョン互換
    pub notifications: Vec<Rule>,
    pub device_notifications: BTreeMap<String, Vec<Rule>>,
    /// 充電が完了したら通知する
    pub notify_full_charge: bool,
    /// 通知を Webhook にも送る
    pub webhook_enabled: bool,
    pub webhook_url: String,
    pub webhook_format: WebhookFormat,
    pub theme: Theme,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebhookFormat {
    #[default]
    Discord,
    Slack,
    /// {"title","message","device","battery","event"}
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn next(self) -> Theme {
        match self {
            Theme::System => Theme::Light,
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::System,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "システム",
            Theme::Light => "ライト",
            Theme::Dark => "ダーク",
        }
    }

    /// 実際にダーク表示にするか
    pub fn is_dark(self) -> bool {
        match self {
            Theme::Light => false,
            Theme::Dark => true,
            Theme::System => system_dark(),
        }
    }
}

/// Windows の「アプリモード」がダークか
pub fn system_dark() -> bool {
    use windows::Win32::System::Registry::*;
    use windows::core::w;
    let mut value = 1u32;
    let mut size = 4u32;
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        );
    }
    value == 0
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            tray_display_device: String::new(),
            tray_display_devices: Vec::new(),
            selected_device: String::new(),
            notifications: Vec::new(),
            device_notifications: BTreeMap::new(),
            notify_full_charge: true,
            webhook_enabled: false,
            webhook_url: String::new(),
            webhook_format: WebhookFormat::Discord,
            theme: Theme::System,
        }
    }
}

pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("GHUBBatteryTray")
}

fn path() -> PathBuf {
    dir().join("settings.json")
}

pub fn modified() -> Option<std::time::SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

impl Settings {
    pub fn load() -> Settings {
        let mut s: Settings = std::fs::read_to_string(path())
            .ok()
            .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default();

        if s.tray_display_device.is_empty() && !s.selected_device.is_empty() {
            s.tray_display_device = s.selected_device.clone();
        }
        if s.tray_display_devices.is_empty() && !s.tray_display_device.is_empty() {
            s.tray_display_devices.push(s.tray_display_device.clone());
        }
        if !s.notifications.is_empty() && !s.selected_device.is_empty() {
            let old = std::mem::take(&mut s.notifications);
            s.device_notifications.entry(s.selected_device.clone()).or_insert(old);
        }
        s
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        self.tray_display_device = self.tray_display_devices.first().cloned().unwrap_or_default();
        self.selected_device = self.tray_display_device.clone();
        self.notifications.clear();
        std::fs::create_dir_all(dir())?;
        std::fs::write(path(), serde_json::to_string_pretty(self).unwrap_or_default())
    }

    pub fn is_tray(&self, device: &str) -> bool {
        self.tray_display_devices.iter().any(|d| d.eq_ignore_ascii_case(device))
    }

    /// トレイ表示の対象に入れる / 外す
    pub fn set_tray(&mut self, device: &str, show: bool) {
        self.tray_display_devices.retain(|d| !d.eq_ignore_ascii_case(device));
        if show {
            self.tray_display_devices.push(device.to_string());
        }
    }

    pub fn rules(&self, device: &str) -> Option<&Vec<Rule>> {
        self.device_notifications.iter().find(|(k, _)| k.eq_ignore_ascii_case(device)).map(|(_, v)| v)
    }
}

pub fn log(message: &str) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(dir());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("GHUBBatteryTray.log")) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "unix={secs} {message}");
    }
}
