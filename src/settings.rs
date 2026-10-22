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
    /// 残量ごとの色
    pub level_colors: LevelColors,
    /// 起動時と 1 日 1 回、GitHub で新しいバージョンを確認する
    pub check_updates: bool,
}

/// 残量 max_percent % 以下ならこの色
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LevelColor {
    pub max_percent: i32,
    pub color: [u8; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct LevelColors {
    /// 小さい順。最後の段階は 100% まで
    pub levels: Vec<LevelColor>,
    pub charging: [u8; 3],
    pub disconnected: [u8; 3],
}

impl Default for LevelColors {
    fn default() -> Self {
        LevelColors {
            levels: vec![
                LevelColor { max_percent: 20, color: [214, 48, 49] },
                LevelColor { max_percent: 50, color: [225, 130, 0] },
                LevelColor { max_percent: 100, color: [32, 150, 80] },
            ],
            charging: [30, 120, 220],
            disconnected: [110, 116, 128],
        }
    }
}

impl LevelColors {
    /// 残量に対応する色
    pub fn for_percent(&self, percent: i32) -> [u8; 3] {
        let mut levels: Vec<&LevelColor> = self.levels.iter().collect();
        levels.sort_by_key(|l| l.max_percent);
        levels.iter().find(|l| percent <= l.max_percent).or(levels.last()).map(|l| l.color).unwrap_or([32, 150, 80])
    }

    /// 段階を小さい順に並べ、最後を 100% にそろえる
    pub fn normalize(&mut self) {
        self.levels.sort_by_key(|l| l.max_percent);
        self.levels.dedup_by_key(|l| l.max_percent);
        if self.levels.is_empty() {
            *self = LevelColors::default();
            return;
        }
        if let Some(last) = self.levels.last_mut() {
            last.max_percent = 100;
        }
    }
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
            level_colors: LevelColors::default(),
            check_updates: true,
        }
    }
}

/// 設定フォルダーごとの識別子 (二重起動防止・ウィンドウ名に使う)
pub fn instance_id() -> String {
    let dir = dir().to_string_lossy().to_lowercase();
    let hash = dir.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    format!("{hash:016x}")
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
        self.level_colors.normalize();
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
