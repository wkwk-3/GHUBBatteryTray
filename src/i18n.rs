//! 多言語対応。表示する文言 (Texts) と、言語ごとの見た目の調整 (Style) を持つ。
//! 文言の中の {0} {1} ... は f() で置き換える。ログは日本語のまま (問い合わせ対応用)。

mod de;
mod en_gb;
mod en_us;
mod fr;
mod ja;
mod ko;
mod zh_cn;

use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

/// 設定に保存する言語 (System = Windows の表示言語に合わせる)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    System,
    De,
    EnGb,
    EnUs,
    Fr,
    Ja,
    Ko,
    ZhCn,
}

/// 選べる言語の並び (一般的な並び: 各言語の自称で、ラテン文字のアルファベット順 → CJK)
pub const CHOICES: [Language; 8] = [
    Language::System,
    Language::De,
    Language::EnGb,
    Language::EnUs,
    Language::Fr,
    Language::Ja,
    Language::Ko,
    Language::ZhCn,
];

impl Language {
    /// 選択肢に出す名前 (各言語の自称。System だけは今の言語で)
    pub fn label(self) -> &'static str {
        match self {
            Language::System => t().language_system,
            Language::De => "Deutsch",
            Language::EnGb => "English (UK)",
            Language::EnUs => "English (US)",
            Language::Fr => "Français",
            Language::Ja => "日本語",
            Language::Ko => "한국어",
            Language::ZhCn => "简体中文",
        }
    }

    /// 実際に使う言語 (System を解決する)
    pub fn resolve(self) -> Language {
        if self == Language::System { detect() } else { self }
    }
}

/// Windows の表示言語から選ぶ。対応していない言語なら英語 (US)
pub fn detect() -> Language {
    // テストは日本語の文言で比べる
    if cfg!(test) {
        return Language::Ja;
    }
    let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    from_langid(id)
}

/// LANGID (下位 10 ビットが言語、上位 6 ビットが地域) から
pub fn from_langid(id: u16) -> Language {
    let (primary, sub) = (id & 0x3FF, id >> 10);
    match primary {
        // 英国・アイルランド・オーストラリア・ニュージーランドは英国式
        0x09 if matches!(sub, 0x02 | 0x03 | 0x05 | 0x06) => Language::EnGb,
        0x07 => Language::De,
        0x0C => Language::Fr,
        0x11 => Language::Ja,
        0x12 => Language::Ko,
        // 中国語は簡体字で表示 (繁体字の地域も含む)
        0x04 => Language::ZhCn,
        _ => Language::EnUs,
    }
}

/// 今の言語 (0 = まだ決めていない)
static CURRENT: AtomicU8 = AtomicU8::new(0);

fn code(l: Language) -> u8 {
    CHOICES.iter().position(|c| *c == l).unwrap_or(0) as u8
}

/// 設定の言語を反映する
pub fn set(language: Language) {
    CURRENT.store(code(language.resolve()), Ordering::Relaxed);
}

pub fn current() -> Language {
    match CURRENT.load(Ordering::Relaxed) {
        0 => {
            let l = detect();
            CURRENT.store(code(l), Ordering::Relaxed);
            l
        }
        n => CHOICES[n as usize],
    }
}

/// 今の言語の文言
pub fn t() -> &'static Texts {
    texts(current())
}

pub fn texts(l: Language) -> &'static Texts {
    match l {
        Language::De => &de::T,
        Language::EnGb => &en_gb::T,
        Language::Fr => &fr::T,
        Language::Ja => &ja::T,
        Language::Ko => &ko::T,
        Language::ZhCn => &zh_cn::T,
        Language::EnUs | Language::System => &en_us::T,
    }
}

/// 今の言語の見た目の調整
pub fn style() -> &'static Style {
    style_of(current())
}

pub fn style_of(l: Language) -> &'static Style {
    &texts(l).style
}

/// "{0} / {1}" の番号を順に置き換える
pub fn f(template: &str, args: &[&dyn Display]) -> String {
    let mut out = template.to_string();
    for (i, a) in args.iter().enumerate() {
        out = out.replace(&format!("{{{i}}}"), &a.to_string());
    }
    out
}

/// 小数を今の言語の書き方で ("5.3" / "5,3")
pub fn dec(v: f64, places: usize) -> String {
    let s = format!("{v:.places$}");
    if style().decimal_comma { s.replace('.', ",") } else { s }
}

/// パーセントを今の言語の書き方で ("20%" / "20 %")
pub fn pct(v: impl Display) -> String {
    f(t().percent_fmt, &[&v])
}

/// 必要になったときだけ読み込む予備のフォント (韓国語・簡体字中国語)。1 つ 13〜19 MB あるので常には読まない
pub const EXTRA_FONTS: [&str; 2] = ["malgun.ttf", "msyh.ttc"];

/// 言語ごとの見た目の調整 (フォント・文字の大きさ・文言の長さに合わせた幅)
pub struct Style {
    /// 使うフォント (C:\Windows\Fonts のファイル名、先頭から優先)。見つからなければ次へ
    /// 他の言語の文字 (言語の一覧の「한국어」「简体中文」など) 用のフォントは、必要になったときだけ足す (EXTRA_FONTS)
    pub fonts: &'static [&'static str],
    /// フォントの字面を枠の上下中央に合わせる量 (文字サイズ比)
    pub y_offset: f32,
    /// 本文・ボタンの文字の大きさ
    pub body: f32,
    /// 通知表の列幅: 有効 / 残量 / アイコン / 削除
    pub rule_cols: [f32; 4],
    /// フッターの「＋ 通知追加」「残量を再取得」の最小幅
    pub footer_button: f32,
    /// Webhook の「URL」「形式」の見出し幅
    pub field_label: f32,
    /// 補完一覧の説明の位置と、一覧の幅
    pub completion_desc_x: f32,
    pub completion_w: f32,
    /// 12 時間表記 (午前/午後) にするか
    pub clock_12h: bool,
    /// 小数点をコンマにするか (ドイツ語・フランス語)
    pub decimal_comma: bool,
}

/// 画面・トレイ・通知に出す文言
pub struct Texts {
    pub style: Style,

    // 共通
    pub on: &'static str,
    pub off: &'static str,
    pub unknown: &'static str,
    /// パーセントの書き方 ("{0}%" / ドイツ語・フランス語は数字との間に改行しない空白)
    pub percent_fmt: &'static str,
    pub icon_names: [&'static str; 4],
    pub theme_system: &'static str,
    pub theme_light: &'static str,
    pub theme_dark: &'static str,
    pub theme_fmt: &'static str,
    pub source_hid: &'static str,
    pub charging: &'static str,
    pub disconnected: &'static str,
    pub asleep: &'static str,
    pub fully_charged: &'static str,
    pub remaining_fmt: &'static str,
    pub power_fmt: &'static str,
    pub no_devices: &'static str,
    pub loading: &'static str,
    pub full_charge_done: &'static str,
    pub default_rule_message: &'static str,

    // 時間・日付の書き方
    pub about_fmt: &'static str,
    pub minutes_fmt: &'static str,
    pub hours_fmt: &'static str,
    pub days_fmt: &'static str,
    pub per_hour: &'static str,
    pub around_time_fmt: &'static str,
    /// {0} = 年, {1} = 月の名前
    pub month_year_fmt: &'static str,
    pub months: [&'static str; 12],
    pub tomorrow_fmt: &'static str,
    /// {0} = 月, {1} = 日
    pub date_md_fmt: &'static str,
    pub am: &'static str,
    pub pm: &'static str,
    pub cycles_fmt: &'static str,

    // 置換文字
    pub ph_bar: &'static str,
    pub ph_battery: &'static str,
    pub ph_cycles: &'static str,
    pub ph_empty_at: &'static str,
    pub ph_health: &'static str,
    pub ph_name: &'static str,
    pub ph_now: &'static str,
    pub ph_pace: &'static str,
    pub ph_power: &'static str,
    pub ph_rate: &'static str,
    pub ph_remain: &'static str,
    pub ph_replace: &'static str,
    pub ph_rn: &'static str,
    pub ph_since: &'static str,
    pub ph_source: &'static str,
    pub ph_streak: &'static str,
    pub ph_time: &'static str,
    pub rn_sample: &'static str,
    pub pace_faster: &'static str,
    pub pace_usual: &'static str,
    pub pace_slower: &'static str,

    // 設定画面: 全体
    pub window_title: &'static str,
    pub theme_hover_fmt: &'static str,
    pub back: &'static str,
    pub back_hover: &'static str,
    pub settings: &'static str,
    pub placeholder_hint: &'static str,
    pub example_fmt: &'static str,
    pub add_rule: &'static str,
    pub refresh: &'static str,
    pub open_logs: &'static str,
    pub join_discord: &'static str,
    pub close: &'static str,
    pub save: &'static str,
    pub unsaved: &'static str,
    pub saved: &'static str,
    pub save_failed_fmt: &'static str,
    pub no_battery_info: &'static str,
    pub source_fmt: &'static str,

    // 設定画面: デバイス
    pub no_devices_long: &'static str,
    pub rules_title_fmt: &'static str,
    pub notify_full_charge: &'static str,
    pub show_icon: &'static str,
    pub show_icon_hover: &'static str,
    pub col_enabled: &'static str,
    pub col_threshold: &'static str,
    pub col_icon: &'static str,
    pub col_message: &'static str,
    pub no_rules: &'static str,
    pub delete: &'static str,
    pub preview_fmt: &'static str,
    pub completion_help: &'static str,
    pub icon_shown: &'static str,
    pub collapse: &'static str,
    pub expand: &'static str,

    // 設定画面: 詳細
    pub trend_title: &'static str,
    pub trend_info: &'static str,
    pub health_title: &'static str,
    pub health_info: &'static str,
    pub est_capacity: &'static str,
    pub cap_measured_fmt: &'static str,
    pub cap_ghub_fmt: &'static str,
    pub ghub_estimate_info: &'static str,
    pub cap_unknown_info: &'static str,
    pub full_runtime: &'static str,
    pub runtime_info: &'static str,
    pub runtime_unknown_info: &'static str,
    pub capacity: &'static str,
    pub capacity_info: &'static str,
    pub recording: &'static str,
    pub capacity_pending_fmt: &'static str,
    pub cycles_label: &'static str,
    pub cycles_info: &'static str,
    pub reach_fmt: &'static str,
    pub reached: &'static str,
    pub basis_trend: &'static str,
    pub basis_cycles: &'static str,
    pub need_a_day: &'static str,
    pub degrade: &'static str,
    pub replace: &'static str,
    pub no_data: &'static str,
    pub since_fmt: &'static str,
    pub recording_history: &'static str,
    pub empty_in_fmt: &'static str,
    pub calculating: &'static str,
    pub per_charge: &'static str,
    pub per_charge_info_fmt: &'static str,
    pub charge_time: &'static str,
    pub charge_time_info: &'static str,
    pub profile_title: &'static str,
    pub profile_info: &'static str,
    pub daily_title: &'static str,
    pub daily_info: &'static str,

    // 設定画面: 歯車
    pub language_title: &'static str,
    pub language_info: &'static str,
    pub language_system: &'static str,
    pub icon_section_info: &'static str,
    pub notify_title: &'static str,
    pub notify_info: &'static str,
    pub defer_fullscreen: &'static str,
    pub webhook_title: &'static str,
    pub webhook_info: &'static str,
    pub webhook_enable: &'static str,
    pub hide: &'static str,
    pub show: &'static str,
    pub format: &'static str,
    pub help_discord: &'static str,
    pub help_slack: &'static str,
    pub help_json: &'static str,
    pub json_label: &'static str,
    pub test_send: &'static str,
    pub test_message: &'static str,
    pub sending: &'static str,
    pub sent_fmt: &'static str,
    pub failed_fmt: &'static str,
    pub url_warning: &'static str,
    pub startup_title: &'static str,
    pub startup_info: &'static str,
    pub start_with_windows: &'static str,
    pub colors_title: &'static str,
    pub colors_info: &'static str,
    pub preview: &'static str,
    pub charging_short: &'static str,
    pub add_level: &'static str,
    pub reset_default: &'static str,
    pub update_title: &'static str,
    pub update_info: &'static str,
    pub current_version_fmt: &'static str,
    pub check_now: &'static str,
    pub auto_check: &'static str,
    pub new_version_fmt: &'static str,
    pub update: &'static str,
    pub downloading: &'static str,
    pub release_page: &'static str,
    pub release_page_hover: &'static str,
    pub release_notes: &'static str,
    pub open_release_page: &'static str,
    pub update_available: &'static str,
    pub up_to_date: &'static str,
    pub up_to_date_fmt: &'static str,
    pub check_failed_fmt: &'static str,
    pub update_failed_fmt: &'static str,

    // トレイ
    pub pending_fmt: &'static str,
    pub updated_fmt: &'static str,
    pub tip_no_info: &'static str,
    pub tip_pick: &'static str,
    pub menu_devices: &'static str,
    pub menu_hide: &'static str,
    pub menu_settings: &'static str,
    pub menu_update_fmt: &'static str,
    pub menu_refresh: &'static str,
    pub menu_webhook: &'static str,
    pub menu_webhook_no_url: &'static str,
    pub menu_exit: &'static str,
    pub update_balloon_title: &'static str,
    pub update_balloon_fmt: &'static str,

    // 起動時の確認
    pub mismatch_title: &'static str,
    /// {0} = 今のバージョン, {1} = タグ, {2} = GitHub のハッシュ値, {3} = この exe のハッシュ値
    pub mismatch_fmt: &'static str,

    // 更新・Webhook のエラー
    pub err_curl_fmt: &'static str,
    pub err_release_not_found: &'static str,
    pub err_network: &'static str,
    pub err_no_tag: &'static str,
    pub err_no_exe: &'static str,
    pub err_no_url: &'static str,
    pub err_read_releases_fmt: &'static str,
    pub err_read_download_fmt: &'static str,
    pub err_size_fmt: &'static str,
    pub err_not_exe: &'static str,
    pub err_sha: &'static str,
    pub err_replace_fmt: &'static str,
    pub err_place_fmt: &'static str,
    pub err_launch_fmt: &'static str,
    pub err_url_scheme: &'static str,
    pub err_connect: &'static str,
}
