//! 設定画面。トレイから `--settings` 付きで別プロセスとして起動され、閉じると終了する。

use crate::ghub::{Battery, Source};
use crate::icon::level_color;
use crate::settings::{LevelColor, LevelColors};
use crate::{log, startup, updater};
use crate::settings::{Rule, Settings, Theme, WebhookFormat};
use crate::health::{self, Basis, Run};
use crate::history::{self, Sample};
use crate::{hidpp, sources, webhook};
use eframe::egui::{self, Color32, CornerRadius, Pos2, RichText, Stroke, Vec2};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

const ICON_NAMES: [&str; 4] = ["なし", "情報", "警告", "エラー"];
/// ボタン・入力欄・プルダウンの高さをそろえる
const CONTROL_H: f32 = 30.0;
/// 游ゴシックの字面を枠の上下中央に来るように下げる量 (文字サイズ比)
const JP_FONT_Y_OFFSET: f32 = 0.32;
const ACCENT: Color32 = Color32::from_rgb(0, 120, 212);

/// ライト/ダークの配色
#[derive(Clone, Copy)]
struct Palette {
    bg: Color32,
    card: Color32,
    card_hover: Color32,
    border: Color32,
    track: Color32,
    text: Color32,
    sub: Color32,
    danger: Color32,
    ok: Color32,
    /// 減り具合グラフ (残量の色に関係なく固定)
    chart_bg: Color32,
    chart_grid: Color32,
    chart_line: Color32,
}

impl Palette {
    fn new(dark: bool) -> Self {
        if dark {
            Palette {
                bg: Color32::from_rgb(28, 29, 33),
                card: Color32::from_rgb(40, 42, 47),
                card_hover: Color32::from_rgb(48, 50, 56),
                border: Color32::from_rgb(62, 65, 72),
                track: Color32::from_rgb(60, 63, 70),
                text: Color32::from_rgb(232, 234, 238),
                sub: Color32::from_rgb(160, 166, 178),
                danger: Color32::from_rgb(255, 110, 100),
                ok: Color32::from_rgb(90, 200, 120),
                chart_bg: Color32::from_rgb(33, 35, 40),
                chart_grid: Color32::from_rgb(58, 61, 68),
                chart_line: Color32::from_rgb(110, 180, 255),
            }
        } else {
            Palette {
                bg: Color32::from_rgb(243, 244, 247),
                card: Color32::WHITE,
                card_hover: Color32::from_rgb(250, 251, 253),
                border: Color32::from_rgb(222, 225, 231),
                track: Color32::from_rgb(232, 234, 239),
                text: Color32::from_rgb(28, 31, 38),
                sub: Color32::from_rgb(105, 112, 125),
                danger: Color32::from_rgb(196, 43, 28),
                ok: Color32::from_rgb(20, 130, 60),
                chart_bg: Color32::from_rgb(246, 247, 250),
                chart_grid: Color32::from_rgb(226, 229, 235),
                chart_line: Color32::from_rgb(0, 95, 184),
            }
        }
    }
}

pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GHUB Battery 設定")
            .with_inner_size([860.0, 780.0])
            .with_min_inner_size([660.0, 480.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("GHUB Battery 設定", options, Box::new(|cc| Ok(Box::new(SettingsApp::new(cc))))) {
        crate::log::error(format!("設定画面を開けませんでした: {e}"));
    }
}

struct Loaded {
    devices: Vec<Battery>,
    history: HashMap<String, Vec<Sample>>,
    runs: HashMap<String, Vec<Run>>,
}

#[derive(PartialEq)]
enum Tab {
    Devices,
    Webhook,
    /// 歯車ボタンで開くアプリの設定
    App,
}

struct SettingsApp {
    settings: Settings,
    devices: Vec<Battery>,
    /// デバイス名 (小文字) -> 残量の履歴
    history: HashMap<String, Vec<Sample>>,
    /// デバイス名 (小文字) -> 放電区間の記録 (健康状態の推定用)
    runs: HashMap<String, Vec<Run>>,
    loading: Option<Receiver<Loaded>>,
    /// 選択中のデバイスの詳細 (アコーディオン) を開いているか
    expanded: bool,
    selected: String,
    status: String,
    dirty: bool,
    tab: Tab,
    show_url: bool,
    webhook_test: Option<Receiver<Result<u16, String>>>,
    webhook_result: Option<Result<u16, String>>,
    dark: bool,
    dark_checked: Instant,
    /// Windows 起動時に開始 (レジストリの状態)
    startup: bool,
    /// 更新の確認結果
    update_state: updater::State,
    /// 確認・更新を実行中
    update_job: Option<Receiver<UpdateJob>>,
    /// 直前の確認・更新の結果 (成功か, 文言)
    update_message: Option<(bool, String)>,
}

enum UpdateJob {
    Checked(Result<updater::State, String>),
    Installed(Result<(), String>),
}

impl SettingsApp {
    fn new(cc: &eframe::CreationContext) -> Self {
        let settings = Settings::load();
        let dark = settings.theme.is_dark();
        setup_style(&cc.egui_ctx, dark);
        let mut app = SettingsApp {
            selected: settings.tray_display_devices.first().cloned().unwrap_or_default(),
            settings,
            devices: Vec::new(),
            history: HashMap::new(),
            runs: HashMap::new(),
            // `--expand` で詳細を開いた状態で起動
            expanded: std::env::args().any(|a| a == "--expand"),
            loading: None,
            status: String::new(),
            dirty: false,
            // `--tab webhook` で Webhook タブから開く
            tab: if std::env::args().any(|a| a == "webhook") {
                Tab::Webhook
            } else if std::env::args().any(|a| a == "app") {
                Tab::App
            } else {
                Tab::Devices
            },
            show_url: false,
            webhook_test: None,
            webhook_result: None,
            dark,
            dark_checked: Instant::now(),
            startup: startup::enabled(),
            update_state: updater::State::load(),
            update_job: None,
            update_message: None,
        };
        app.refresh(&cc.egui_ctx);
        log::info("設定画面を開きました");
        app
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let devices = sources::collect(&mut hidpp::Reader::new());
            let history = history::load_all(&history::path());
            let runs = health::load_all(&health::path_beside(&history::path()));
            let _ = tx.send(Loaded { devices, history, runs });
            ctx.request_repaint();
        });
        self.loading = Some(rx);
        self.status = "取得中...".into();
    }

    /// カードに並べるデバイス: 取得できたもの + 通知設定だけあるもの
    fn device_names(&self) -> Vec<(String, Option<Battery>)> {
        let mut list: Vec<(String, Option<Battery>)> = self.devices.iter().map(|b| (b.device_name.clone(), Some(b.clone()))).collect();
        for key in self.settings.device_notifications.keys() {
            if !list.iter().any(|(n, _)| n.eq_ignore_ascii_case(key)) {
                list.push((key.clone(), None));
            }
        }
        list.sort_by(|a, b| label(a).cmp(label(b)));
        list
    }

    fn save(&mut self) {
        match self.settings.save() {
            Ok(()) => {
                self.dirty = false;
                self.status = "保存しました".into();
                log::info("設定を保存しました");
                notify_tray();
            }
            Err(e) => {
                log::error(format!("設定の保存に失敗しました: {e}"));
                self.status = format!("保存に失敗しました: {e}");
            }
        }
    }

    /// テーマはすぐ反映・すぐ保存 (編集中の他の設定は保存しない)
    fn set_theme(&mut self, ctx: &egui::Context, theme: Theme) {
        self.settings.theme = theme;
        let mut on_disk = Settings::load();
        on_disk.theme = theme;
        let _ = on_disk.save();
        notify_tray();
        self.dark = theme.is_dark();
        setup_style(ctx, self.dark);
        log::info(format!("テーマ: {}", theme.label()));
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.loading {
            if let Ok(Loaded { devices, history, runs }) = rx.try_recv() {
                self.devices = devices;
                self.history = history;
                self.runs = runs;
                self.loading = None;
                let mut kinds: Vec<&str> = self.devices.iter().filter_map(|b| b.source.map(Source::label)).collect();
                kinds.dedup();
                self.status = if kinds.is_empty() { "バッテリー情報を取得できませんでした".into() } else { format!("取得元: {}", kinds.join(" / ")) };
                let names = self.device_names();
                if !names.iter().any(|(n, _)| n.eq_ignore_ascii_case(&self.selected)) {
                    self.selected = names.first().map(|(n, _)| n.clone()).unwrap_or_default();
                }
            }
        }
        if let Some(rx) = &self.webhook_test {
            if let Ok(r) = rx.try_recv() {
                match &r {
                    Ok(code) => log::info(format!("Webhook のテスト送信に成功しました (HTTP {code})")),
                    Err(e) => log::error(format!("Webhook のテスト送信に失敗しました: {e}")),
                }
                self.webhook_result = Some(r);
                self.webhook_test = None;
            }
        }
        if let Some(rx) = &self.update_job {
            if let Ok(job) = rx.try_recv() {
                self.update_job = None;
                match job {
                    UpdateJob::Checked(Ok(state)) => {
                        self.update_message = Some((true, if state.available().is_some() { "新しいバージョンがあります".into() } else { "最新のバージョンです".into() }));
                        self.update_state = state;
                    }
                    UpdateJob::Checked(Err(e)) => {
                        log::warn(format!("更新の確認に失敗しました: {e}"));
                        self.update_message = Some((false, format!("確認できませんでした: {e}")));
                    }
                    UpdateJob::Installed(Ok(())) => {
                        // 新しいバージョンが起動するので、この画面は閉じる
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    UpdateJob::Installed(Err(e)) => {
                        log::error(format!("更新に失敗しました: {e}"));
                        self.update_message = Some((false, format!("更新できませんでした: {e}")));
                    }
                }
            }
        }
        // 「システム」のときは Windows の切り替えに追従する
        if self.settings.theme == Theme::System && self.dark_checked.elapsed().as_secs() >= 2 {
            self.dark_checked = Instant::now();
            let dark = Theme::System.is_dark();
            if dark != self.dark {
                self.dark = dark;
                setup_style(ctx, dark);
            }
        }
    }
}

fn label(d: &(String, Option<Battery>)) -> &str {
    d.1.as_ref().map(|b| b.label()).unwrap_or(&d.0)
}

/// 起動中のトレイに設定の再読み込みを知らせる
fn notify_tray() {
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    if let Some(hwnd) = crate::tray::find_window() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), crate::tray::WM_APP_RELOAD, WPARAM(0), LPARAM(0));
        }
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        let p = Palette::new(self.dark);

        egui::Frame::new().fill(p.bg).inner_margin(20).show(ui, |ui| {
            ui.set_min_size(ui.available_size());

            // ヘッダー: タブとテーマ切り替え
            egui::Panel::top("header").frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { bottom: 12, ..Default::default() })).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (tab, text) in [(Tab::Devices, "デバイスと通知"), (Tab::Webhook, "Webhook")] {
                        let selected = self.tab == tab;
                        let rt = RichText::new(text).size(15.0).color(if selected { p.text } else { p.sub });
                        if ui.add(egui::Button::selectable(selected, if selected { rt.strong() } else { rt })).clicked() {
                            self.tab = tab;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if gear_button(ui, p, self.tab == Tab::App).on_hover_text("設定").clicked() {
                            self.tab = if self.tab == Tab::App { Tab::Devices } else { Tab::App };
                        }
                        let next = self.settings.theme.next();
                        let text = format!("テーマ: {}", self.settings.theme.label());
                        if ui.add(egui::Button::new(text)).on_hover_text(format!("クリックで「{}」に切り替え", next.label())).clicked() {
                            self.set_theme(&ctx, next);
                        }
                    });
                });
            });

            // フッター (下端に固定)
            egui::Panel::bottom("footer").frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { top: 10, ..Default::default() })).show(ui, |ui| {
                if self.tab == Tab::Devices {
                    ui.label(RichText::new("使用可能置換:  {name} デバイス名   {battery} しきい値   {rn} 改行").color(p.sub).size(12.5));
                    ui.add_space(8.0);
                }
                ui.horizontal(|ui| {
                    if self.tab == Tab::Devices {
                        if ui.add_enabled(!self.selected.is_empty(), egui::Button::new("＋ 通知追加").min_size(Vec2::new(110.0, 34.0))).clicked() {
                            self.settings.device_notifications.entry(self.selected.clone()).or_default().push(Rule::default());
                            self.dirty = true;
                        }
                        if ui.add_enabled(self.loading.is_none(), egui::Button::new("残量を再取得").min_size(Vec2::new(110.0, 34.0))).clicked() {
                            self.refresh(&ctx);
                        }
                    }
                    ui.label(RichText::new(&self.status).color(p.sub));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new("閉じる").min_size(Vec2::new(96.0, 34.0))).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        let save = egui::Button::new(RichText::new("保存").color(Color32::WHITE).strong()).fill(ACCENT).min_size(Vec2::new(96.0, 34.0));
                        if ui.add(save).clicked() {
                            self.save();
                        }
                        if self.dirty {
                            ui.label(RichText::new("未保存の変更があります").color(p.danger));
                        }
                    });
                });
            });

            egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| match self.tab {
                Tab::Devices => self.devices_tab(ui, p),
                Tab::Webhook => self.webhook_tab(ui, p),
                Tab::App => self.app_tab(ui, p),
            });
        });
    }
}

impl SettingsApp {
    fn devices_tab(&mut self, ui: &mut egui::Ui, p: Palette) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.devices_tab_body(ui, p));
    }

    fn devices_tab_body(&mut self, ui: &mut egui::Ui, p: Palette) {
        let names = self.device_names();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(12.0, 12.0);
            for (name, battery) in &names {
                let is_tray = self.settings.is_tray(name);
                let selected = name.eq_ignore_ascii_case(&self.selected);
                if device_card(ui, p, &self.settings.level_colors, name, battery.as_ref(), selected, selected && self.expanded, is_tray).clicked() {
                    // 同じカードなら開閉、別のカードなら選んで開く
                    self.expanded = !(selected && self.expanded);
                    self.selected = name.clone();
                }
            }
            if names.is_empty() && self.loading.is_none() {
                ui.label(RichText::new("バッテリー対応デバイスが見つかりません。受信機が挿さっているか確認してください。").color(p.sub));
            }
        });

        // 詳細 (アコーディオン)
        let key = self.selected.to_ascii_lowercase();
        let battery = names.iter().find(|(n, _)| n.eq_ignore_ascii_case(&self.selected)).and_then(|(_, b)| b.clone());
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), ui.make_persistent_id("device_detail"), false);
        state.set_open(self.expanded && !self.selected.is_empty());
        state.show_body_unindented(ui, |ui| {
            ui.add_space(12.0);
            let samples = self.history.get(&key).map(Vec::as_slice).unwrap_or(&[]);
            let runs = self.runs.get(&key).map(Vec::as_slice).unwrap_or(&[]);
            detail_panel(ui, p, battery.as_ref(), samples, runs);
        });
        state.store(ui.ctx());

        ui.add_space(14.0);
        if self.selected.is_empty() {
            return;
        }

        let selected_label = names.iter().find(|(n, _)| n == &self.selected).map(label).unwrap_or(&self.selected).to_string();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("通知設定  —  {selected_label}")).size(18.0).strong().color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.checkbox(&mut self.settings.notify_full_charge, "充電完了通知").changed() {
                    self.dirty = true;
                }
                let mut tray = self.settings.is_tray(&self.selected);
                if ui.checkbox(&mut tray, "アイコン表示").on_hover_text("タスクトレイにこのデバイスのアイコンを表示します (デバイスごとに別のアイコン)").changed() {
                    let selected = self.selected.clone();
                    self.settings.set_tray(&selected, tray);
                    self.dirty = true;
                }
            });
        });
        ui.add_space(6.0);

        egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(12).show(ui, |ui| {
            ui.set_min_size(Vec2::new(ui.available_width(), ui.available_height().max(170.0)));
            if rules_table(ui, p, self.settings.device_notifications.entry(self.selected.clone()).or_default()) {
                self.dirty = true;
            }
        });
        if self.settings.device_notifications.get(&self.selected).is_some_and(|r| r.is_empty()) {
            self.settings.device_notifications.remove(&self.selected);
        }
    }

    fn webhook_tab(&mut self, ui: &mut egui::Ui, p: Palette) {
        egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(20).show(ui, |ui| {
            ui.set_min_width(ui.available_width());

            ui.horizontal(|ui| {
                ui.label(RichText::new("Webhook 通知").size(18.0).strong().color(p.text));
                info_icon(ui, p, "残量低下・充電完了の通知を Discord / Slack などにも送ります。");
            });
            ui.add_space(12.0);

            if ui.checkbox(&mut self.settings.webhook_enabled, RichText::new("Webhook に送信する").size(15.0)).changed() {
                self.dirty = true;
            }
            ui.add_space(10.0);

            const LABEL_W: f32 = 48.0;
            ui.horizontal(|ui| {
                ui.add_sized([LABEL_W, CONTROL_H], egui::Label::new(RichText::new("URL").color(p.sub)));
                let edit = egui::TextEdit::singleline(&mut self.settings.webhook_url)
                    .password(!self.show_url)
                    .hint_text("https://discord.com/api/webhooks/...")
                    .align(egui::Align2::LEFT_CENTER);
                let width = (ui.available_width() - 80.0).max(200.0);
                if ui.add_sized([width, CONTROL_H], edit).changed() {
                    self.dirty = true;
                }
                if ui.add_sized([72.0, CONTROL_H], egui::Button::new(if self.show_url { "隠す" } else { "表示" })).clicked() {
                    self.show_url = !self.show_url;
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_sized([LABEL_W, CONTROL_H], egui::Label::new(RichText::new("形式").color(p.sub)));
                egui::ComboBox::from_id_salt("format")
                    .selected_text(format_label(self.settings.webhook_format))
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for f in [WebhookFormat::Discord, WebhookFormat::Slack, WebhookFormat::Json] {
                            if ui.selectable_value(&mut self.settings.webhook_format, f, format_label(f)).changed() {
                                self.dirty = true;
                            }
                        }
                    });
            });

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let can_test = self.webhook_test.is_none() && !self.settings.webhook_url.trim().is_empty();
                if ui.add_enabled(can_test, egui::Button::new("テスト送信").min_size(Vec2::new(110.0, CONTROL_H))).clicked() {
                    let (tx, rx) = channel();
                    let url = self.settings.webhook_url.trim().to_string();
                    let format = self.settings.webhook_format;
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let m = webhook::Message { title: "GHUB Battery".into(), text: "テスト送信です".into(), device: "test".into(), battery: 50, event: "test" };
                        let _ = tx.send(webhook::send(&url, format, &m));
                        ctx.request_repaint();
                    });
                    self.webhook_test = Some(rx);
                    self.webhook_result = None;
                }
                match (&self.webhook_test, &self.webhook_result) {
                    (Some(_), _) => {
                        ui.label(RichText::new("送信中...").color(p.sub));
                    }
                    (None, Some(Ok(code))) => {
                        ui.label(RichText::new(format!("送信できました (HTTP {code})")).color(p.ok));
                    }
                    (None, Some(Err(e))) => {
                        ui.label(RichText::new(format!("失敗: {e}")).color(p.danger));
                    }
                    _ => {}
                }
            });

            ui.add_space(16.0);
            let help = match self.settings.webhook_format {
                WebhookFormat::Discord => "Discord: チャンネル設定 → 連携サービス → ウェブフック で URL を作成できます。",
                WebhookFormat::Slack => "Slack: Incoming Webhooks アプリで URL を作成できます。",
                WebhookFormat::Json => "汎用 JSON: {\"title\", \"message\", \"device\", \"battery\", \"event\"} を POST します (Home Assistant / n8n など)。",
            };
            ui.label(RichText::new(help).color(p.sub).size(12.5));
            ui.label(RichText::new("URL には認証用のトークンが含まれるので、他人に見せないでください。").color(p.sub).size(12.5));
        });
    }
}

impl SettingsApp {
    /// 歯車ボタンで開く設定: アイコン表示 / 起動 / 残量の色
    fn app_tab(&mut self, ui: &mut egui::Ui, p: Palette) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            section(ui, p, "アイコン表示", "タスクトレイにアイコンを表示するデバイスです。デバイスごとに別のアイコンが表示されます。\n「保存」で反映されます。", |ui| {
                let names = self.device_names();
                if names.is_empty() {
                    ui.label(RichText::new("デバイスが見つかりません").color(p.sub));
                }
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(24.0, 8.0);
                    for (name, battery) in &names {
                        let mut show = self.settings.is_tray(name);
                        let text = battery.as_ref().map(|b| b.label().to_string()).unwrap_or_else(|| name.clone());
                        if ui.checkbox(&mut show, text).changed() {
                            self.settings.set_tray(name, show);
                            self.dirty = true;
                        }
                    }
                });
            });

            section(ui, p, "起動", "オンにすると、Windows にサインインしたときに自動で起動します。\n切り替えるとすぐに反映されます (「保存」は不要)。", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Windows 起動時に開始").color(p.text));
                    ui.add_space(8.0);
                    if toggle_switch(ui, &mut self.startup).changed() {
                        startup::set(self.startup);
                        self.startup = startup::enabled();
                    }
                    ui.label(RichText::new(if self.startup { "オン" } else { "オフ" }).color(p.sub));
                });
            });

            self.update_section(ui, p);

            section(
                ui,
                p,
                "残量の色",
                "残量に応じた、タスクトレイのアイコンと画面の色です。上から順に「この % 以下ならこの色」で、最後の段階は 100% までです。\n色の四角を押すと色を選べます。画面にはすぐ反映され、タスクトレイには「保存」で反映されます。",
                |ui| {
                    let colors = &mut self.settings.level_colors;

                    // プレビュー
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("プレビュー").color(p.sub).size(12.5));
                        ui.add_space(8.0);
                        let mut samples: Vec<(String, [u8; 3])> = [5, 15, 30, 45, 60, 85].iter().map(|v| (v.to_string(), colors.for_percent(*v))).collect();
                        samples.push(("充".into(), colors.charging));
                        samples.push(("--".into(), colors.disconnected));
                        for (text, c) in samples {
                            color_tile(ui, &text, c);
                        }
                    });
                    ui.add_space(10.0);

                    let mut remove = None;
                    let count = colors.levels.len();
                    for i in 0..count {
                        let low = if i == 0 { 0 } else { colors.levels[i - 1].max_percent + 1 };
                        let high_limit = if i + 1 < count { colors.levels[i + 1].max_percent - 1 } else { 100 };
                        ui.horizontal(|ui| {
                            ui.add_sized([44.0, CONTROL_H], egui::Label::new(RichText::new(format!("{low}%")).color(p.sub)));
                            ui.label(RichText::new("〜").color(p.sub));
                            if i + 1 < count {
                                let drag = egui::DragValue::new(&mut colors.levels[i].max_percent).range(low..=high_limit.max(low)).suffix(" %").speed(0.3);
                                if ui.add_sized([80.0, CONTROL_H], drag).changed() {
                                    self.dirty = true;
                                }
                            } else {
                                colors.levels[i].max_percent = 100;
                                ui.add_sized([80.0, CONTROL_H], egui::Label::new(RichText::new("100 %").color(p.text)));
                            }
                            if egui::color_picker::color_edit_button_srgb(ui, &mut colors.levels[i].color).changed() {
                                self.dirty = true;
                            }
                            if count > 1 && ui.add(egui::Button::new(RichText::new("削除").color(p.danger)).frame(false)).clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        colors.levels.remove(i);
                        colors.normalize();
                        self.dirty = true;
                    }
                    ui.add_space(4.0);
                    if ui.add_enabled(count < 8, egui::Button::new("＋ 段階を追加")).clicked() {
                        // いちばん広い段階を半分に分ける
                        let mut best = (0usize, 0i32);
                        for (i, l) in colors.levels.iter().enumerate() {
                            let low = if i == 0 { 0 } else { colors.levels[i - 1].max_percent + 1 };
                            if l.max_percent - low > best.1 {
                                best = (i, l.max_percent - low);
                            }
                        }
                        if best.1 >= 2 {
                            let (i, width) = best;
                            let low = if i == 0 { 0 } else { colors.levels[i - 1].max_percent + 1 };
                            let color = colors.levels[i].color;
                            colors.levels.insert(i, LevelColor { max_percent: low + width / 2, color });
                            self.dirty = true;
                        }
                    }

                    ui.add_space(10.0);
                    for (label, color) in [("充電中", &mut colors.charging), ("未接続", &mut colors.disconnected)] {
                        ui.horizontal(|ui| {
                            ui.add_sized([44.0 + 8.0 + 14.0 + 80.0, CONTROL_H], egui::Label::new(RichText::new(label).color(p.text)));
                            if egui::color_picker::color_edit_button_srgb(ui, color).changed() {
                                self.dirty = true;
                            }
                        });
                    }

                    ui.add_space(10.0);
                    if ui.add_enabled(*colors != LevelColors::default(), egui::Button::new("デフォルトに戻す")).clicked() {
                        *colors = LevelColors::default();
                        self.dirty = true;
                    }
                },
            );
        });
    }
}

impl SettingsApp {
    /// アップデート: 自動確認のオン/オフ、今すぐ確認、更新ボタン
    fn update_section(&mut self, ui: &mut egui::Ui, p: Palette) {
        section(
            ui,
            p,
            "アップデート",
            "GitHub のリリースで新しいバージョンを確認します。自動確認はアプリの起動時と 1 日 1 回です。\n「更新」を押すと、新しいバージョンをダウンロードして入れ替え、アプリを再起動します。設定はそのまま引き継がれます。",
            |ui| {
                let busy = self.update_job.is_some();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("現在のバージョン  v{}", updater::CURRENT)).color(p.text));
                    ui.add_space(16.0);
                    if ui.add_enabled(!busy, egui::Button::new("今すぐ確認")).clicked() {
                        let (tx, rx) = channel();
                        let ctx = ui.ctx().clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(UpdateJob::Checked(updater::check()));
                            ctx.request_repaint();
                        });
                        self.update_job = Some(rx);
                        self.update_message = None;
                    }
                    if busy {
                        ui.spinner();
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("更新を自動で確認する").color(p.text));
                    ui.add_space(8.0);
                    if toggle_switch(ui, &mut self.settings.check_updates).changed() {
                        self.dirty = true;
                    }
                    ui.label(RichText::new(if self.settings.check_updates { "オン" } else { "オフ" }).color(p.sub));
                });

                if let Some(release) = self.update_state.available().cloned() {
                    ui.add_space(12.0);
                    egui::Frame::new().fill(p.bg).stroke(Stroke::new(1.0, ACCENT)).corner_radius(6).inner_margin(12).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} が出ています", release.tag)).strong().color(p.text));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let button = egui::Button::new(RichText::new("更新").color(Color32::WHITE).strong()).fill(ACCENT).min_size(Vec2::new(96.0, CONTROL_H));
                                if ui.add_enabled(!busy, button).clicked() {
                                    let (tx, rx) = channel();
                                    let ctx = ui.ctx().clone();
                                    let r = release.clone();
                                    std::thread::spawn(move || {
                                        let _ = tx.send(UpdateJob::Installed(updater::update_and_restart(&r)));
                                        ctx.request_repaint();
                                    });
                                    self.update_job = Some(rx);
                                    self.update_message = Some((true, "ダウンロードしています...".into()));
                                }
                                if ui.add(egui::Button::new("リリースページ").frame(false)).on_hover_text("ブラウザーで GitHub のリリースページを開きます").clicked() {
                                    updater::open_page(Some(&release));
                                }
                            });
                        });
                        let notes = release.notes.trim();
                        if !notes.is_empty() {
                            ui.add_space(4.0);
                            egui::CollapsingHeader::new(RichText::new("更新内容").color(p.sub).size(12.5)).id_salt("release_notes").show(ui, |ui| {
                                egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                                    ui.label(RichText::new(notes).color(p.text).size(12.5));
                                });
                            });
                        }
                    });
                }

                if let Some((ok, msg)) = &self.update_message {
                    ui.add_space(8.0);
                    let failed = !ok;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(msg).color(if failed { p.danger } else { p.sub }).size(12.5));
                        if failed && ui.add(egui::Button::new("リリースページを開く").frame(false)).clicked() {
                            updater::open_page(self.update_state.latest.as_ref());
                        }
                    });
                } else if let Some(r) = self.update_state.latest.as_ref().filter(|r| !r.is_newer()) {
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("最新のバージョンです (最新リリース {})", r.tag)).color(p.sub).size(12.5));
                }
            },
        );
    }
}

/// カード状の区切り (見出し + i アイコン)
fn section(ui: &mut egui::Ui, p: Palette, title: &str, info: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(18).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).size(16.0).strong().color(p.text));
            info_icon(ui, p, info);
        });
        ui.add_space(10.0);
        body(ui);
    });
    ui.add_space(12.0);
}

/// 色見本の小さなタイル (トレイアイコンと同じ見た目)
fn color_tile(ui: &mut egui::Ui, text: &str, c: [u8; 3]) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(7), Color32::from_rgb(c[0], c[1], c[2]));
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(14.0), Color32::WHITE);
}

/// オン/オフのスイッチ
fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = Vec2::new(42.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let visuals = ui.style().interact_selectable(&response, *on);
    let off_bg = ui.visuals().widgets.inactive.bg_fill;
    let bg = if *on { ACCENT } else { off_bg };
    ui.painter().rect(rect, CornerRadius::same(11), bg, visuals.bg_stroke, egui::StrokeKind::Inside);
    let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 歯車のボタン (押すと設定ページ)
fn gear_button(ui: &mut egui::Ui, p: Palette, active: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(CONTROL_H), egui::Sense::click());
    let visuals = ui.style().interact(&response);
    let bg = if active { ACCENT } else { visuals.bg_fill };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    let color = if active { Color32::WHITE } else { p.text };
    let c = rect.center();
    let painter = ui.painter();
    // 歯 8 本 + 外周 + 中央の穴
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = Vec2::new(a.cos(), a.sin());
        painter.line_segment([c + dir * 5.5, c + dir * 9.0], Stroke::new(3.0, color));
    }
    painter.circle_filled(c, 6.5, color);
    painter.circle_filled(c, 2.6, bg);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn format_label(f: WebhookFormat) -> &'static str {
    match f {
        WebhookFormat::Discord => "Discord",
        WebhookFormat::Slack => "Slack",
        WebhookFormat::Json => "汎用 JSON",
    }
}

/// 通知設定の表。変更があれば true
fn rules_table(ui: &mut egui::Ui, p: Palette, rules: &mut Vec<Rule>) -> bool {
    // 列幅: 有効 / 残量 / アイコン / 通知内容 (残り) / 削除
    const W: [f32; 3] = [44.0, 110.0, 100.0];
    const DELETE_W: f32 = 50.0;
    let gap = ui.spacing().item_spacing.x;
    let message_w = (ui.available_width() - W.iter().sum::<f32>() - DELETE_W - gap * 4.0 - 16.0).max(160.0);
    let h = CONTROL_H;
    let centered = egui::Layout::left_to_right(egui::Align::Center).with_main_align(egui::Align::Center).with_main_justify(true);
    let middle_left = egui::Layout::left_to_right(egui::Align::Center);

    let mut changed = false;
    let mut remove = None;

    ui.horizontal(|ui| {
        let header = |t: &str| RichText::new(t).color(p.sub).strong().size(12.5);
        for (text, w) in [("有効", W[0]), ("残量 (%) 以下", W[1]), ("アイコン", W[2])] {
            ui.allocate_ui_with_layout(Vec2::new(w, 20.0), centered, |ui| ui.label(header(text)));
        }
        ui.allocate_ui_with_layout(Vec2::new(message_w, 20.0), middle_left, |ui| {
            ui.add_space(6.0);
            ui.label(header("通知内容"));
        });
    });
    ui.separator();

    egui::ScrollArea::vertical().id_salt("rules").auto_shrink([false, true]).max_height(220.0).show(ui, |ui| {
        if rules.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("通知がありません。「＋ 通知追加」で追加できます。").color(p.sub));
        }

        for (i, rule) in rules.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(Vec2::new(W[0], h), centered, |ui| changed |= ui.checkbox(&mut rule.enabled, "").changed());
                changed |= ui.add_sized([W[1], h], egui::DragValue::new(&mut rule.battery).range(0..=100).suffix(" %").speed(0.5)).changed();
                ui.allocate_ui_with_layout(Vec2::new(W[2], h), middle_left, |ui| {
                    egui::ComboBox::from_id_salt(("icon", i))
                        .selected_text(ICON_NAMES[rule.balloon_tip_icon.clamp(0, 3) as usize])
                        .width(W[2] - 8.0)
                        .show_ui(ui, |ui| {
                            for (v, n) in ICON_NAMES.iter().enumerate() {
                                changed |= ui.selectable_value(&mut rule.balloon_tip_icon, v as i32, *n).changed();
                            }
                        });
                });
                changed |= ui.add_sized([message_w, h], egui::TextEdit::singleline(&mut rule.message).align(egui::Align2::LEFT_CENTER)).changed();
                if ui.add_sized([DELETE_W, h], egui::Button::new(RichText::new("削除").color(p.danger)).frame(false)).clicked() {
                    remove = Some(i);
                }
            });
            ui.add_space(4.0);
        }
    });

    if let Some(i) = remove {
        rules.remove(i);
        changed = true;
    }
    changed
}

#[allow(clippy::too_many_arguments)]
fn device_card(ui: &mut egui::Ui, p: Palette, colors: &LevelColors, name: &str, battery: Option<&Battery>, selected: bool, expanded: bool, is_tray: bool) -> egui::Response {
    let (r, g, b) = level_color(battery, colors);
    let accent = Color32::from_rgb(r, g, b);
    let size = Vec2::new(390.0, 104.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter_at(rect);

    let fill = if response.hovered() && !selected { p.card_hover } else { p.card };
    let stroke = if selected { Stroke::new(2.0, ACCENT) } else { Stroke::new(1.0, p.border) };
    painter.rect(rect, CornerRadius::same(8), fill, stroke, egui::StrokeKind::Inside);

    let pad = 14.0;
    let title = battery.map(|b| b.label()).unwrap_or(name);
    painter.text(rect.min + Vec2::new(pad, pad - 2.0), egui::Align2::LEFT_TOP, title, egui::FontId::proportional(14.0), p.text);

    // 取得元のタグ (右上)
    if let Some(src) = battery.and_then(|b| b.source) {
        let galley = painter.layout_no_wrap(src.label().into(), egui::FontId::proportional(11.0), p.sub);
        let tag = egui::Rect::from_min_size(egui::pos2(rect.max.x - pad - galley.size().x - 12.0, rect.min.y + pad - 3.0), galley.size() + Vec2::new(12.0, 4.0));
        painter.rect(tag, CornerRadius::same(4), p.bg, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
        painter.galley(tag.min + Vec2::new(6.0, 2.0), galley, p.sub);
    }

    let percent = match battery {
        Some(b) if b.connected => format!("{}%", b.percentage),
        _ => "--".into(),
    };
    let big = painter.text(rect.min + Vec2::new(pad, 34.0), egui::Align2::LEFT_TOP, percent, egui::FontId::proportional(28.0), accent);

    let mut notes = Vec::new();
    if is_tray {
        notes.push("アイコン表示中".to_string());
    }
    match battery {
        Some(b) if b.connected => {
            let status = b.status();
            if let Some(rest) = status.split_once("  ·  ") {
                notes.push(rest.1.to_string());
            }
        }
        _ => notes.push("未接続".into()),
    }
    painter.text(egui::pos2(big.max.x + 10.0, big.center().y), egui::Align2::LEFT_CENTER, notes.join("  ·  "), egui::FontId::proportional(12.5), p.sub);

    // 残量バー
    let bar = egui::Rect::from_min_size(egui::pos2(rect.min.x + pad, rect.max.y - pad - 6.0), Vec2::new(rect.width() - pad * 2.0 - 70.0, 6.0));
    painter.rect_filled(bar, CornerRadius::same(3), p.track);
    if let Some(b) = battery.filter(|b| b.connected && b.percentage > 0) {
        let fill = egui::Rect::from_min_size(bar.min, Vec2::new((bar.width() * b.percentage.clamp(0, 100) as f32 / 100.0).max(6.0), 6.0));
        painter.rect_filled(fill, CornerRadius::same(3), accent);
    }

    // 詳細の開閉
    let hint = if expanded { "閉じる ▲" } else { "詳細 ▼" };
    let hint_color = if selected || response.hovered() { ACCENT } else { p.sub };
    painter.text(egui::pos2(rect.max.x - pad, bar.center().y), egui::Align2::RIGHT_CENTER, hint, egui::FontId::proportional(12.0), hint_color);

    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 小さな「i」アイコン。ホバーで説明を出す
fn info_icon(ui: &mut egui::Ui, p: Palette, text: &str) {
    let size = 15.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let color = if response.hovered() { ACCENT } else { p.sub.gamma_multiply(0.8) };
    let painter = ui.painter();
    painter.circle_stroke(rect.center(), size / 2.0 - 1.0, Stroke::new(1.2, color));
    painter.text(rect.center() + Vec2::new(0.0, 0.5), egui::Align2::CENTER_CENTER, "i", egui::FontId::proportional(10.5), color);
    response.on_hover_cursor(egui::CursorIcon::Help).on_hover_ui(|ui| {
        ui.set_max_width(300.0);
        ui.label(RichText::new(text).size(12.5));
    });
}

/// 見出し + i アイコン
fn heading_with_info(ui: &mut egui::Ui, p: Palette, title: &str, info: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(15.0).strong().color(p.text));
        info_icon(ui, p, info);
    });
}

/// カードの下に開く詳細: 減り具合のグラフと電池の健康状態
fn detail_panel(ui: &mut egui::Ui, p: Palette, battery: Option<&Battery>, samples: &[Sample], runs: &[Run]) {
    let now = history::now();
    let h = health::analyze(runs, now);
    let remaining_runs = health::MIN_RUNS - h.runs.min(health::MIN_RUNS);

    egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(16).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.columns(2, |cols| {
            // 左: 減り具合
            let ui = &mut cols[0];
            heading_with_info(ui, p, "バッテリーの減り方", "実線: 実際の減り方 (最後に充電してから、最大 24 時間分)\n点線: このまま同じペースで減った場合の予測\n\n記録が少ないうちは、G HUB の残り時間から予測します。");
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width() - 12.0, 150.0), egui::Sense::hover());
            let chart = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - 18.0));
            draw_trend(ui.painter(), chart, p, battery, samples);

            // 右: 健康状態
            let ui = &mut cols[1];
            heading_with_info(
                ui,
                p,
                "バッテリーの健康状態",
                "推定値です。充電のたびにその前の放電 (何 % を何時間で使ったか) を記録し、記録が増えるほど正確になります。\n\nマウスは使わない間スリープして電池をほとんど使わないため、使い方によって実際の容量とはずれます。",
            );
            ui.add_space(6.0);

            // (項目, 値, 値が出ているか, 説明)
            let mut rows: Vec<(&str, String, bool, String)> = Vec::new();

            let ghub_wh = battery.and_then(|b| Some(b.power_mw? * b.max_life_hours? / 1000.0));
            rows.push(match (h.capacity_mwh, ghub_wh) {
                (Some(c), g) => (
                    "推定容量",
                    format!("{:.2} Wh", c / 1000.0),
                    true,
                    format!("放電量 × 満充電での持ち時間 (実測) から計算した容量です。{}", g.map(|g| format!("\nG HUB の推定: {g:.2} Wh")).unwrap_or_default()),
                ),
                (None, Some(g)) => ("推定容量", format!("{g:.2} Wh"), true, "G HUB の推定値です。充電の記録がたまると、実測から計算した値に切り替わります。".into()),
                (None, None) => ("推定容量", "—".into(), false, "放電量 (G HUB が起動しているときに取得) と充電の記録がそろうと表示します。".into()),
            });
            rows.push(match (h.full_runtime_h, battery.and_then(|b| b.max_life_hours)) {
                (Some(r), _) => ("満充電での持ち", format!("約{r:.0}時間"), true, "実際の放電の記録から計算した、100% から 0% までの時間です。".into()),
                (None, Some(g)) => ("満充電での持ち", format!("約{g:.0}時間"), true, "G HUB の推定値です。充電の記録がたまると、実測の値に切り替わります。".into()),
                _ => ("満充電での持ち", "—".into(), false, "充電の記録がたまると表示します。".into()),
            });
            rows.push(match h.health {
                Some(v) => ("容量", format!("{:.0}%", v * 100.0), true, "記録を始めた頃と比べた、今の容量です。".into()),
                None => ("容量", "記録中".into(), false, format!("記録を始めた頃と比べた容量です。あと {remaining_runs} 回充電すると表示します。")),
            });
            rows.push(("充電サイクル", format!("{:.1} 回", h.cycles), true, "このアプリで記録を始めてからの充電回数です (使った % の合計 ÷ 100)。それ以前の使用分は含みません。".into()));

            let when = |label: &'static str, at: Option<(u64, Basis)>, level: &str| -> (&'static str, String, bool, String) {
                let what = format!("容量が {level} まで減る時期の目安です。");
                match at {
                    Some((t, _)) if t <= now => (label, "到達済みの可能性".into(), true, what),
                    Some((t, basis)) => {
                        let (y, m) = health::year_month(t);
                        let why = match basis {
                            Basis::Trend => "実際の容量の減り方から計算しています。",
                            Basis::Cycles => "今の充電ペースと、一般的なリチウムイオン電池の寿命 (約 400 回で容量 80%、約 600 回で 70%) から計算しています。記録が 6 回・14 日以上たまると、実際の減り方からの計算に切り替わります。",
                        };
                        (label, format!("{y}年{m}月ごろ"), true, format!("{what}\n{why}"))
                    }
                    None => (label, "記録中".into(), false, format!("{what}\n1 日以上記録すると表示します。")),
                }
            };
            rows.push(when("劣化の目安", h.degrade_at, "80%"));
            rows.push(when("買い替えの目安", h.replace_at, "70%"));

            egui::Grid::new("health").num_columns(2).spacing(Vec2::new(18.0, 9.0)).min_row_height(22.0).show(ui, |ui| {
                for (label, value, known, info) in rows {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(label).color(p.sub).size(13.0));
                        info_icon(ui, p, &info);
                    });
                    ui.label(RichText::new(value).color(if known { p.text } else { p.sub }).size(13.5));
                    ui.end_row();
                }
            });
        });
    });
}

fn hours_text(h: f64) -> String {
    if h >= 1.0 { format!("約{h:.0}時間") } else { format!("約{:.0}分", (h * 60.0).max(1.0)) }
}

fn draw_trend(painter: &egui::Painter, area: egui::Rect, p: Palette, battery: Option<&Battery>, samples: &[Sample]) {
    painter.rect_filled(area, CornerRadius::same(4), p.chart_bg);
    for pct in [0.0, 50.0, 100.0] {
        let y = area.bottom() - area.height() * pct / 100.0;
        painter.line_segment([egui::pos2(area.left(), y), egui::pos2(area.right(), y)], Stroke::new(1.0, p.chart_grid));
    }
    let small = egui::FontId::proportional(11.0);
    let label_y = area.bottom() + 3.0;

    let Some(b) = battery.filter(|b| b.connected) else {
        painter.text(area.center(), egui::Align2::CENTER_CENTER, "データなし", small, p.sub);
        return;
    };

    let now = history::now();
    let trend = history::trend(samples, now, b);
    let empty = trend.empty_at(now, b.percentage);

    // 横軸: 実績の始まり 〜 0% になる予測時刻
    let t1 = empty.unwrap_or(now + 3600).max(now + 60);
    let first = trend.actual.first().map(|s| s.t).unwrap_or(now);
    let t0 = if first + 60 >= now { now.saturating_sub(((t1 - now) / 5).max(600)) } else { first };
    let span = (t1 - t0) as f32;
    let at = |t: u64, pct: f64| -> Pos2 {
        egui::pos2(
            area.left() + area.width() * (t.saturating_sub(t0)) as f32 / span,
            area.bottom() - area.height() * (pct.clamp(0.0, 100.0) as f32) / 100.0,
        )
    };

    // 今の位置
    let now_x = at(now, 0.0).x;
    painter.line_segment([egui::pos2(now_x, area.top()), egui::pos2(now_x, area.bottom())], Stroke::new(1.0, p.chart_grid.gamma_multiply(1.6)));

    // 実績 (実線)
    let line = Stroke::new(2.0, p.chart_line);
    let points: Vec<Pos2> = trend.actual.iter().map(|s| at(s.t, s.percent as f64)).collect();
    if points.len() >= 2 {
        painter.add(egui::Shape::line(points, line));
    }

    // 予測 (点線)
    if let Some(end) = empty {
        let from = at(now, b.percentage as f64);
        painter.extend(egui::Shape::dashed_line(&[from, at(end, 0.0)], line, 4.0, 3.0));
    }
    painter.circle_filled(at(now, b.percentage as f64), 3.0, p.chart_line);

    // 下のラベル
    let past_h = (now - t0) as f64 / 3600.0;
    let left = if trend.actual.len() >= 2 { format!("{}前から", hours_text(past_h)) } else { "履歴を記録中".into() };
    painter.text(egui::pos2(area.left(), label_y), egui::Align2::LEFT_TOP, left, small.clone(), p.sub);
    let right = match (b.charging, empty, trend.drain_per_hour) {
        (true, _, _) => "充電中".to_string(),
        (false, Some(end), Some(rate)) => format!("{}後に 0%  (-{rate:.1}%/時)", hours_text((end - now) as f64 / 3600.0)),
        _ => "減り方を計算中".to_string(),
    };
    painter.text(egui::pos2(area.right(), label_y), egui::Align2::RIGHT_TOP, right, small, p.sub);
}

fn setup_style(ctx: &egui::Context, dark: bool) {
    // 日本語表示用に Windows のフォントを読み込む (初回のみ)
    static FONT_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !FONT_LOADED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let mut fonts = egui::FontDefinitions::default();
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        for file in ["YuGothM.ttc", "meiryo.ttc", "msgothic.ttc"] {
            if let Ok(bytes) = std::fs::read(format!(r"{windir}\Fonts\{file}")) {
                let mut data = egui::FontData::from_owned(bytes);
                data.tweak.y_offset_factor = JP_FONT_Y_OFFSET;
                fonts.font_data.insert("jp".into(), std::sync::Arc::new(data));
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts.families.entry(family).or_default().insert(0, "jp".into());
                }
                break;
            }
        }
        ctx.set_fonts(fonts);
    }

    let p = Palette::new(dark);
    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
    ctx.global_style_mut(|style| {
        style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        style.spacing.button_padding = Vec2::new(12.0, 4.0);
        style.spacing.interact_size.y = CONTROL_H;
        style.visuals.panel_fill = p.bg;
        style.visuals.window_fill = p.card;
        // 入力欄はカードと見分けられる色 + 枠線
        style.visuals.extreme_bg_color = if dark { Color32::from_rgb(30, 31, 35) } else { Color32::from_rgb(244, 245, 248) };
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        style.visuals.override_text_color = Some(p.text);
        for w in [&mut style.visuals.widgets.inactive, &mut style.visuals.widgets.hovered, &mut style.visuals.widgets.active] {
            w.corner_radius = CornerRadius::same(6);
        }
    });
}
