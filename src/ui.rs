//! 設定画面。トレイから `--settings` 付きで別プロセスとして起動され、閉じると終了する。

use crate::ghub::{Battery, Source};
use crate::icon::level_color;
use crate::settings::{LevelColor, LevelColors};
use crate::i18n::{self, Language, dec, f, pct, t};
use crate::{log, placeholders, startup, updater};
use crate::settings::{Rule, Settings, Theme, WebhookFormat};
use crate::health::{self, Basis, Run};
use crate::history::{self, Sample};
use crate::{hidpp, sources, webhook};
use eframe::egui::{self, Color32, CornerRadius, Pos2, RichText, Stroke, Vec2};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

/// ボタン・入力欄・プルダウンの高さをそろえる
const CONTROL_H: f32 = 30.0;
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
            .with_title(t().window_title)
            .with_inner_size([860.0, 780.0])
            .with_min_inner_size([660.0, 480.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(t().window_title, options, Box::new(|cc| Ok(Box::new(SettingsApp::new(cc))))) {
        crate::log::error("E009", format!("設定画面を開けませんでした: {e}"));
    }
}

struct Loaded {
    devices: Vec<Battery>,
    history: HashMap<String, Vec<Sample>>,
    runs: HashMap<String, Vec<Run>>,
    stats: HashMap<String, Vec<crate::stats::Hour>>,
}

#[derive(PartialEq)]
enum Tab {
    /// デバイスと通知設定 (最初の画面)
    Devices,
    /// 歯車ボタンで開くアプリの設定 (Webhook もここ)
    App,
}

struct SettingsApp {
    settings: Settings,
    devices: Vec<Battery>,
    /// デバイス名 (小文字) -> 残量の履歴
    history: HashMap<String, Vec<Sample>>,
    /// デバイス名 (小文字) -> 放電区間の記録 (健康状態の推定用)
    runs: HashMap<String, Vec<Run>>,
    /// デバイス名 (小文字) -> 1 時間ごとの長期の統計
    stats: HashMap<String, Vec<crate::stats::Hour>>,
    /// 統計の計算結果 (デバイス名 (小文字) -> (計算した時刻, 結果))。再描画のたびに計算しないよう使い回す
    analyzed: HashMap<String, (Instant, health::Health)>,
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
    /// 置換文字の補完
    completion: Option<Completion>,
    /// 常駐から届く最新の残量
    live: Receiver<crate::live::Live>,
}

/// 通知内容の置換文字の補完 (どの行で、どこから置き換えるか)
#[derive(Clone, Debug, Default)]
struct Completion {
    row: usize,
    /// 置き換えを始める文字位置 ('{' の位置、または Tab を押した位置)
    start: usize,
    selected: usize,
    /// 候補の一覧にマウスが乗っているか (クリックで選ぶ間は閉じない)
    hovered: bool,
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
        // 常駐から最新の残量を受け取る (届いたら再描画)
        let (live_tx, live) = channel();
        let live_ctx = cc.egui_ctx.clone();
        crate::live::receive(move |l| {
            let _ = live_tx.send(l);
            live_ctx.request_repaint();
        });
        let mut app = SettingsApp {
            selected: settings.tray_display_devices.first().cloned().unwrap_or_default(),
            settings,
            devices: Vec::new(),
            history: HashMap::new(),
            runs: HashMap::new(),
            stats: HashMap::new(),
            analyzed: HashMap::new(),
            // `--expand` で詳細を開いた状態で起動
            expanded: std::env::args().any(|a| a == "--expand"),
            loading: None,
            status: String::new(),
            dirty: false,
            // `--tab app` (旧 `--tab webhook` も) で設定ページから開く
            tab: if std::env::args().any(|a| a == "app" || a == "webhook") { Tab::App } else { Tab::Devices },
            show_url: false,
            webhook_test: None,
            webhook_result: None,
            dark,
            dark_checked: Instant::now(),
            startup: startup::enabled(),
            update_state: updater::State::load(),
            update_job: None,
            update_message: None,
            completion: None,
            live,
        };
        app.refresh(&cc.egui_ctx);
        log::info("I028", "設定画面を開きました");
        app
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        self.load(ctx, None);
        self.status = t().loading.into();
    }

    /// 残量と記録 (履歴・統計) を読み込む。devices があれば (常駐から届いた) それを使い、デバイスには問い合わせない
    fn load(&mut self, ctx: &egui::Context, devices: Option<Vec<Battery>>) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let devices = devices.unwrap_or_else(|| sources::collect(&mut hidpp::Reader::new()));
            let history = history::load_all(&history::path());
            let runs = health::load_all(&health::path_beside(&history::path()));
            // 統計はデバイスごとのファイル。表示する可能性のあるデバイスだけ読む
            let mut keys: Vec<String> = devices.iter().map(|b| b.device_name.to_ascii_lowercase()).collect();
            keys.extend(Settings::load().device_notifications.keys().map(|k| k.to_ascii_lowercase()));
            keys.dedup();
            let stats = keys.into_iter().map(|k| (k.clone(), crate::stats::load(&crate::stats::path_for(&k)))).collect();
            let _ = tx.send(Loaded { devices, history, runs, stats });
            ctx.request_repaint();
        });
        self.loading = Some(rx);
    }

    /// 常駐から届いた最新の残量を反映する (届かなければ自分で読み直す)
    fn poll_live(&mut self, ctx: &egui::Context) {
        let mut latest = None;
        let mut stale = false;
        while let Ok(l) = self.live.try_recv() {
            match l {
                crate::live::Live::Update(list) => latest = Some(list),
                crate::live::Live::Stale => stale = true,
            }
        }
        if self.loading.is_some() {
            return;
        }
        if let Some(list) = latest {
            // カードはすぐ更新し、グラフ用の記録は裏で読み直す
            self.devices = list.clone();
            self.load(ctx, Some(list));
        } else if stale {
            self.load(ctx, None);
        }
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
                self.status = t().saved.into();
                log::info("I029", "設定を保存しました");
                notify_tray();
            }
            Err(e) => {
                log::error("E010", format!("設定の保存に失敗しました: {e}"));
                self.status = f(t().save_failed_fmt, &[&e]);
            }
        }
    }

    /// テーマはすぐ反映・すぐ保存 (編集中の他の設定は保存しない)
    fn set_theme(&mut self, ctx: &egui::Context, theme: Theme) {
        self.settings.theme = theme;
        let mut on_disk = Settings::load();
        on_disk.theme = theme;
        if let Err(e) = on_disk.save() {
            log::error("E011", format!("設定の保存に失敗しました: {e}"));
        }
        notify_tray();
        self.dark = theme.is_dark();
        setup_style(ctx, self.dark);
        log::info("I030", format!("テーマ: {}", theme.label()));
    }

    /// 言語はすぐ反映・すぐ保存 (編集中の他の設定は保存しない)
    fn set_language(&mut self, ctx: &egui::Context, language: Language) {
        self.settings.language = language;
        let mut on_disk = Settings::load();
        on_disk.language = language;
        if let Err(e) = on_disk.save() {
            log::error("E012", format!("設定の保存に失敗しました: {e}"));
        }
        i18n::set(language);
        // 表示中の文言も今の言語で作り直す
        self.status = if self.loading.is_some() { t().loading.into() } else { self.source_status() };
        self.update_message = None;
        notify_tray();
        setup_style(ctx, self.dark);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(t().window_title.into()));
        log::info("I031", format!("言語: {:?} ({:?})", language, i18n::current()));
    }

    /// 左下の「取得元: ...」
    fn source_status(&self) -> String {
        let mut kinds: Vec<&str> = self.devices.iter().filter_map(|b| b.source.map(Source::label)).collect();
        kinds.dedup();
        if kinds.is_empty() { t().no_battery_info.into() } else { f(t().source_fmt, &[&kinds.join(" / ")]) }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        self.poll_live(ctx);
        if let Some(rx) = &self.loading {
            if let Ok(Loaded { devices, history, runs, stats }) = rx.try_recv() {
                self.devices = devices;
                self.history = history;
                self.runs = runs;
                self.stats = stats;
                self.analyzed.clear();
                self.loading = None;
                self.status = self.source_status();
                let names = self.device_names();
                if !names.iter().any(|(n, _)| n.eq_ignore_ascii_case(&self.selected)) {
                    self.selected = names.first().map(|(n, _)| n.clone()).unwrap_or_default();
                }
            }
        }
        if let Some(rx) = &self.webhook_test {
            if let Ok(r) = rx.try_recv() {
                match &r {
                    Ok(code) => log::info("I032", format!("Webhook のテスト送信に成功しました (HTTP {code})")),
                    Err(e) => log::error("E013", format!("Webhook のテスト送信に失敗しました: {e}")),
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
                        self.update_message = Some((true, if state.available().is_some() { t().update_available.into() } else { t().up_to_date.into() }));
                        self.update_state = state;
                    }
                    UpdateJob::Checked(Err(e)) => {
                        log::warn("W012", format!("更新の確認に失敗しました: {e}"));
                        self.update_message = Some((false, f(t().check_failed_fmt, &[&e])));
                    }
                    UpdateJob::Installed(Ok(())) => {
                        // 新しいバージョンが起動するので、この画面は閉じる
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    UpdateJob::Installed(Err(e)) => {
                        log::error("E014", format!("更新に失敗しました: {e}"));
                        self.update_message = Some((false, f(t().update_failed_fmt, &[&e])));
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

            // ヘッダー: 左にタイトル (設定ページでは「戻る」)、右にテーマと歯車
            egui::Panel::top("header").frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { bottom: 12, ..Default::default() })).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(CONTROL_H);
                    if self.tab == Tab::App {
                        if ui.add(egui::Button::new(RichText::new(t().back).color(p.text)).frame(false)).on_hover_text(t().back_hover).clicked() {
                            self.tab = Tab::Devices;
                        }
                        ui.add_space(4.0);
                        ui.label(RichText::new(t().settings).size(17.0).strong().color(p.text));
                    } else {
                        ui.label(RichText::new("GHUB Battery").size(17.0).strong().color(p.text));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if gear_button(ui, p, self.tab == Tab::App).on_hover_text(t().settings).clicked() {
                            self.tab = if self.tab == Tab::App { Tab::Devices } else { Tab::App };
                        }
                        let next = self.settings.theme.next();
                        let text = f(t().theme_fmt, &[&self.settings.theme.label()]);
                        if ui.add(egui::Button::new(text)).on_hover_text(f(t().theme_hover_fmt, &[&next.label()])).clicked() {
                            self.set_theme(&ctx, next);
                        }
                    });
                });
            });

            // フッター (下端に固定)
            egui::Panel::bottom("footer").frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { top: 10, ..Default::default() })).show(ui, |ui| {
                if self.tab == Tab::Devices {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t().placeholder_hint).color(p.sub).size(12.5));
                        let list: Vec<String> = placeholders::DEFS.iter().map(|d| format!("{{{}}}  {}  ({})", d.key, d.desc(), f(t().example_fmt, &[&d.sample()]))).collect();
                        info_icon(ui, p, &list.join("\n"));
                    });
                    ui.add_space(8.0);
                }
                ui.horizontal(|ui| {
                    let button_w = i18n::style().footer_button;
                    if self.tab == Tab::Devices {
                        if ui.add_enabled(!self.selected.is_empty(), egui::Button::new(t().add_rule).min_size(Vec2::new(button_w, 34.0))).clicked() {
                            self.settings.device_notifications.entry(self.selected.clone()).or_default().push(Rule::default());
                            self.dirty = true;
                        }
                        if ui.add_enabled(self.loading.is_none(), egui::Button::new(t().refresh).min_size(Vec2::new(button_w, 34.0))).clicked() {
                            self.refresh(&ctx);
                        }
                    }
                    if folder_button(ui, p).on_hover_text(t().open_logs).clicked() {
                        let _ = std::fs::create_dir_all(log::dir());
                        let _ = std::process::Command::new("explorer.exe").arg(log::dir()).spawn();
                    }
                    if discord_button(ui, p).on_hover_text(t().join_discord).clicked() {
                        let _ = std::process::Command::new("explorer.exe").arg(DISCORD_INVITE).spawn();
                    }
                    ui.label(RichText::new(&self.status).color(p.sub));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new(t().close).min_size(Vec2::new(96.0, 34.0))).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        let save = egui::Button::new(RichText::new(t().save).color(Color32::WHITE).strong()).fill(ACCENT).min_size(Vec2::new(96.0, 34.0));
                        if ui.add(save).clicked() {
                            self.save();
                        }
                        if self.dirty {
                            ui.label(RichText::new(t().unsaved).color(p.danger));
                        }
                    });
                });
            });

            egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| match self.tab {
                Tab::Devices => self.devices_tab(ui, p),
                Tab::App => self.app_tab(ui, p),
            });
        });
    }
}

impl SettingsApp {
    fn devices_tab(&mut self, ui: &mut egui::Ui, p: Palette) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.devices_tab_body(ui, p));
    }

    /// 統計の計算結果。読み込み直したときと、1 分たったときだけ計算し直す
    fn health_of(&mut self, key: &str) -> health::Health {
        if let Some((_, h)) = self.analyzed.get(key).filter(|(at, _)| at.elapsed().as_secs() < 60) {
            return h.clone();
        }
        let runs = self.runs.get(key).map(Vec::as_slice).unwrap_or(&[]);
        let hours = self.stats.get(key).map(Vec::as_slice).unwrap_or(&[]);
        let h = crate::stats::analyze(hours, runs, history::now(), log::utc_offset_secs());
        self.analyzed.insert(key.to_string(), (Instant::now(), h.clone()));
        h
    }

    fn devices_tab_body(&mut self, ui: &mut egui::Ui, p: Palette) {
        let names = self.device_names();
        let selected_health = self.health_of(&self.selected.to_ascii_lowercase());
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
                ui.label(RichText::new(t().no_devices_long).color(p.sub));
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
            detail_panel(ui, p, battery.as_ref(), samples, &selected_health);
        });
        state.store(ui.ctx());

        ui.add_space(14.0);
        if self.selected.is_empty() {
            return;
        }

        let selected_label = names.iter().find(|(n, _)| n == &self.selected).map(label).unwrap_or(&self.selected).to_string();
        ui.horizontal(|ui| {
            ui.label(RichText::new(f(t().rules_title_fmt, &[&selected_label])).size(18.0).strong().color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.checkbox(&mut self.settings.notify_full_charge, t().notify_full_charge).changed() {
                    self.dirty = true;
                }
                let mut tray = self.settings.is_tray(&self.selected);
                if ui.checkbox(&mut tray, t().show_icon).on_hover_text(t().show_icon_hover).changed() {
                    let selected = self.selected.clone();
                    self.settings.set_tray(&selected, tray);
                    self.dirty = true;
                }
            });
        });
        ui.add_space(6.0);

        // 候補のサンプルは、選んでいるデバイスの今の値で見せる
        let key = self.selected.to_ascii_lowercase();
        let sample_ctx = names.iter().find(|(n, _)| n.eq_ignore_ascii_case(&self.selected)).and_then(|(_, b)| b.as_ref()).map(|b| {
            let samples = self.history.get(&key).map(Vec::as_slice).unwrap_or(&[]);
            let runs = self.runs.get(&key).map(Vec::as_slice).unwrap_or(&[]);
            let has_stats = self.stats.get(&key).is_some_and(|v| !v.is_empty());
            placeholders::Context::build_with(b, None, samples, runs, &selected_health, has_stats, history::now())
        });

        egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(12).show(ui, |ui| {
            ui.set_min_size(Vec2::new(ui.available_width(), ui.available_height().max(170.0)));
            let rules = self.settings.device_notifications.entry(self.selected.clone()).or_default();
            if rules_table(ui, p, rules, &mut self.completion, sample_ctx.as_ref()) {
                self.dirty = true;
            }
        });
        if self.settings.device_notifications.get(&self.selected).is_some_and(|r| r.is_empty()) {
            self.settings.device_notifications.remove(&self.selected);
        }
    }

    /// 設定ページの「Webhook 通知」
    fn webhook_section(&mut self, ui: &mut egui::Ui, p: Palette) {
        section(
            ui,
            p,
            t().webhook_title,
            t().webhook_info,
            |ui| {
                if ui.checkbox(&mut self.settings.webhook_enabled, t().webhook_enable).changed() {
                    self.dirty = true;
                }
                ui.add_space(10.0);

                let label_w = i18n::style().field_label;
                ui.horizontal(|ui| {
                    ui.add_sized([label_w, CONTROL_H], egui::Label::new(RichText::new("URL").color(p.sub)));
                    let edit = egui::TextEdit::singleline(&mut self.settings.webhook_url)
                        .password(!self.show_url)
                        .hint_text("https://discord.com/api/webhooks/...")
                        .align(egui::Align2::LEFT_CENTER);
                    let width = (ui.available_width() - 80.0).max(200.0);
                    if ui.add_sized([width, CONTROL_H], edit).changed() {
                        self.dirty = true;
                    }
                    if ui.add_sized([72.0, CONTROL_H], egui::Button::new(if self.show_url { t().hide } else { t().show })).clicked() {
                        self.show_url = !self.show_url;
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_sized([label_w, CONTROL_H], egui::Label::new(RichText::new(t().format).color(p.sub)));
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
                    let help = match self.settings.webhook_format {
                        WebhookFormat::Discord => t().help_discord,
                        WebhookFormat::Slack => t().help_slack,
                        WebhookFormat::Json => t().help_json,
                    };
                    info_icon(ui, p, help);
                });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let can_test = self.webhook_test.is_none() && !self.settings.webhook_url.trim().is_empty();
                    if ui.add_enabled(can_test, egui::Button::new(t().test_send).min_size(Vec2::new(i18n::style().footer_button, CONTROL_H))).clicked() {
                        let (tx, rx) = channel();
                        let url = self.settings.webhook_url.trim().to_string();
                        let format = self.settings.webhook_format;
                        let ctx = ui.ctx().clone();
                        std::thread::spawn(move || {
                            let m = webhook::Message { title: "GHUB Battery".into(), text: t().test_message.into(), device: "test".into(), battery: 50, event: "test" };
                            let _ = tx.send(webhook::send(&url, format, &m));
                            ctx.request_repaint();
                        });
                        self.webhook_test = Some(rx);
                        self.webhook_result = None;
                    }
                    match (&self.webhook_test, &self.webhook_result) {
                        (Some(_), _) => {
                            ui.label(RichText::new(t().sending).color(p.sub));
                        }
                        (None, Some(Ok(code))) => {
                            ui.label(RichText::new(f(t().sent_fmt, &[code])).color(p.ok));
                        }
                        (None, Some(Err(e))) => {
                            ui.label(RichText::new(f(t().failed_fmt, &[e])).color(p.danger));
                        }
                        _ => {}
                    }
                });
                ui.add_space(8.0);
                ui.label(RichText::new(t().url_warning).color(p.sub).size(12.0));
            },
        );
    }
}

impl SettingsApp {
    /// 歯車ボタンで開く設定: アイコン表示 / 通知 / Webhook / 起動 / アップデート / 残量の色
    fn app_tab(&mut self, ui: &mut egui::Ui, p: Palette) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let ctx = ui.ctx().clone();
            let mut picked = None;
            section(ui, p, t().language_title, t().language_info, |ui| {
                let current = self.settings.language;
                let shown = if current == Language::System { format!("{} ({})", current.label(), i18n::current().label()) } else { current.label().to_string() };
                let combo = egui::ComboBox::from_id_salt("language").selected_text(shown).width(260.0).height(400.0).show_ui(ui, |ui| {
                    for l in i18n::CHOICES {
                        if ui.selectable_label(l == current, l.label()).clicked() && l != current {
                            picked = Some(l);
                        }
                    }
                });
                // 一覧には各言語の文字で名前が並ぶので、カーソルが乗った・開いたときに予備のフォントを読み込む
                if combo.response.hovered() || combo.inner.is_some() {
                    apply_fonts(ui.ctx(), true);
                }
            });
            if let Some(l) = picked {
                self.set_language(&ctx, l);
            }

            section(ui, p, t().show_icon, t().icon_section_info, |ui| {
                let names = self.device_names();
                if names.is_empty() {
                    ui.label(RichText::new(t().no_devices).color(p.sub));
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

            section(
                ui,
                p,
                t().notify_title,
                t().notify_info,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t().defer_fullscreen).color(p.text));
                        ui.add_space(8.0);
                        if toggle_switch(ui, &mut self.settings.defer_in_fullscreen).changed() {
                            self.dirty = true;
                        }
                        ui.label(RichText::new(if self.settings.defer_in_fullscreen { t().on } else { t().off }).color(p.sub));
                    });
                },
            );

            self.webhook_section(ui, p);

            section(ui, p, t().startup_title, t().startup_info, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t().start_with_windows).color(p.text));
                    ui.add_space(8.0);
                    if toggle_switch(ui, &mut self.startup).changed() {
                        startup::set(self.startup);
                        self.startup = startup::enabled();
                    }
                    ui.label(RichText::new(if self.startup { t().on } else { t().off }).color(p.sub));
                });
            });

            self.update_section(ui, p);

            section(
                ui,
                p,
                t().colors_title,
                t().colors_info,
                |ui| {
                    let colors = &mut self.settings.level_colors;

                    // プレビュー
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t().preview).color(p.sub).size(12.5));
                        ui.add_space(8.0);
                        let mut samples: Vec<(String, [u8; 3])> = [5, 15, 30, 45, 60, 85].iter().map(|v| (v.to_string(), colors.for_percent(*v))).collect();
                        samples.push((t().charging_short.into(), colors.charging));
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
                            ui.add_sized([44.0, CONTROL_H], egui::Label::new(RichText::new(pct(low)).color(p.sub)));
                            ui.label(RichText::new("〜").color(p.sub));
                            if i + 1 < count {
                                let drag = egui::DragValue::new(&mut colors.levels[i].max_percent).range(low..=high_limit.max(low)).suffix(pct("")).speed(0.3);
                                if ui.add_sized([80.0, CONTROL_H], drag).changed() {
                                    self.dirty = true;
                                }
                            } else {
                                colors.levels[i].max_percent = 100;
                                ui.add_sized([80.0, CONTROL_H], egui::Label::new(RichText::new(pct(100)).color(p.text)));
                            }
                            if egui::color_picker::color_edit_button_srgb(ui, &mut colors.levels[i].color).changed() {
                                self.dirty = true;
                            }
                            if count > 1 && ui.add(egui::Button::new(RichText::new(t().delete).color(p.danger)).frame(false)).clicked() {
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
                    if ui.add_enabled(count < 8, egui::Button::new(t().add_level)).clicked() {
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
                    for (label, color) in [(t().charging, &mut colors.charging), (t().disconnected, &mut colors.disconnected)] {
                        ui.horizontal(|ui| {
                            ui.add_sized([44.0 + 8.0 + 14.0 + 80.0, CONTROL_H], egui::Label::new(RichText::new(label).color(p.text)));
                            if egui::color_picker::color_edit_button_srgb(ui, color).changed() {
                                self.dirty = true;
                            }
                        });
                    }

                    ui.add_space(10.0);
                    if ui.add_enabled(*colors != LevelColors::default(), egui::Button::new(t().reset_default)).clicked() {
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
            t().update_title,
            t().update_info,
            |ui| {
                let busy = self.update_job.is_some();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(f(t().current_version_fmt, &[&updater::CURRENT])).color(p.text));
                    ui.add_space(16.0);
                    if ui.add_enabled(!busy, egui::Button::new(t().check_now)).clicked() {
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
                    ui.label(RichText::new(t().auto_check).color(p.text));
                    ui.add_space(8.0);
                    if toggle_switch(ui, &mut self.settings.check_updates).changed() {
                        self.dirty = true;
                    }
                    ui.label(RichText::new(if self.settings.check_updates { t().on } else { t().off }).color(p.sub));
                });

                if let Some(release) = self.update_state.available().cloned() {
                    ui.add_space(12.0);
                    egui::Frame::new().fill(p.bg).stroke(Stroke::new(1.0, ACCENT)).corner_radius(6).inner_margin(12).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(f(t().new_version_fmt, &[&release.tag])).strong().color(p.text));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let button = egui::Button::new(RichText::new(t().update).color(Color32::WHITE).strong()).fill(ACCENT).min_size(Vec2::new(96.0, CONTROL_H));
                                if ui.add_enabled(!busy, button).clicked() {
                                    let (tx, rx) = channel();
                                    let ctx = ui.ctx().clone();
                                    let r = release.clone();
                                    std::thread::spawn(move || {
                                        let _ = tx.send(UpdateJob::Installed(updater::update_and_restart(&r)));
                                        ctx.request_repaint();
                                    });
                                    self.update_job = Some(rx);
                                    self.update_message = Some((true, t().downloading.into()));
                                }
                                if ui.add(egui::Button::new(t().release_page).frame(false)).on_hover_text(t().release_page_hover).clicked() {
                                    updater::open_page(Some(&release));
                                }
                            });
                        });
                        let notes = release.notes.trim();
                        if !notes.is_empty() {
                            ui.add_space(4.0);
                            egui::CollapsingHeader::new(RichText::new(t().release_notes).color(p.sub).size(12.5)).id_salt("release_notes").show(ui, |ui| {
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
                        if failed && ui.add(egui::Button::new(t().open_release_page).frame(false)).clicked() {
                            updater::open_page(self.update_state.latest.as_ref());
                        }
                    });
                } else if let Some(r) = self.update_state.latest.as_ref().filter(|r| !r.is_newer()) {
                    ui.add_space(8.0);
                    ui.label(RichText::new(f(t().up_to_date_fmt, &[&r.tag])).color(p.sub).size(12.5));
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

const DISCORD_INVITE: &str = "https://discord.gg/V7vdTBB8m8";

/// フッターの小さなアイコンボタンの土台
fn icon_button(ui: &mut egui::Ui) -> (egui::Rect, egui::Response) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(30.0), egui::Sense::click());
    let visuals = ui.style().interact(&response);
    ui.painter().rect_filled(rect, CornerRadius::same(6), visuals.bg_fill);
    (rect, response.on_hover_cursor(egui::CursorIcon::PointingHand))
}

/// フォルダーの形
fn folder_button(ui: &mut egui::Ui, p: Palette) -> egui::Response {
    let (rect, response) = icon_button(ui);
    let color = if response.hovered() { ACCENT } else { p.text };
    let c = rect.center();
    let painter = ui.painter();
    // つまみ + 本体
    painter.rect_filled(egui::Rect::from_min_size(c + Vec2::new(-8.0, -6.5), Vec2::new(7.0, 4.0)), CornerRadius::same(1), color);
    painter.rect_filled(egui::Rect::from_min_size(c + Vec2::new(-8.0, -4.0), Vec2::new(16.0, 11.0)), CornerRadius::same(2), color);
    response
}

/// Discord のロゴ (assets/discord.svg を 64x64 のアルファ値にしたもの。作り直しは assets/svg_mask.ps1)
const DISCORD_MASK: &[u8; 64 * 64] = include_bytes!("../assets/discord_mask.bin");

fn discord_button(ui: &mut egui::Ui, _p: Palette) -> egui::Response {
    let (rect, response) = icon_button(ui);
    // テクスチャは初回だけ作って覚えておく (data の中で load_texture を呼ぶとロックが二重になって固まる)
    let id = egui::Id::NULL.with("discord_logo");
    let texture = match ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        Some(t) => t,
        None => {
            let pixels = DISCORD_MASK.iter().map(|&a| Color32::from_white_alpha(a)).collect();
            let t = ui.ctx().load_texture("discord_logo", egui::ColorImage::new([64, 64], pixels), egui::TextureOptions::LINEAR);
            ui.ctx().data_mut(|d| d.insert_temp(id, t.clone()));
            t
        }
    };
    // ブランド色 (ホバーで少し明るく)
    let color = if response.hovered() { Color32::from_rgb(0x79, 0x84, 0xF5) } else { Color32::from_rgb(0x58, 0x65, 0xF2) };
    let logo = egui::Rect::from_center_size(rect.center(), Vec2::splat(18.0));
    ui.painter().image(texture.id(), logo, egui::Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), color);
    response
}

fn format_label(f: WebhookFormat) -> &'static str {
    match f {
        WebhookFormat::Discord => "Discord",
        WebhookFormat::Slack => "Slack",
        WebhookFormat::Json => t().json_label,
    }
}

/// 通知設定の表。変更があれば true
fn rules_table(ui: &mut egui::Ui, p: Palette, rules: &mut Vec<Rule>, completion: &mut Option<Completion>, sample_ctx: Option<&placeholders::Context>) -> bool {
    // 列幅: 有効 / 残量 / アイコン / 通知内容 (残り) / 削除
    let cols = i18n::style().rule_cols;
    #[allow(non_snake_case)]
    let (W, DELETE_W) = ([cols[0], cols[1], cols[2]], cols[3]);
    let gap = ui.spacing().item_spacing.x;
    let message_w = (ui.available_width() - W.iter().sum::<f32>() - DELETE_W - gap * 4.0 - 16.0).max(160.0);
    let h = CONTROL_H;
    let centered = egui::Layout::left_to_right(egui::Align::Center).with_main_align(egui::Align::Center).with_main_justify(true);
    let middle_left = egui::Layout::left_to_right(egui::Align::Center);

    let mut changed = false;
    let mut remove = None;

    ui.horizontal(|ui| {
        let header = |t: &str| RichText::new(t).color(p.sub).strong().size(12.5);
        for (text, w) in [(t().col_enabled, W[0]), (t().col_threshold, W[1]), (t().col_icon, W[2])] {
            ui.allocate_ui_with_layout(Vec2::new(w, 20.0), centered, |ui| ui.label(header(text)));
        }
        ui.allocate_ui_with_layout(Vec2::new(message_w, 20.0), middle_left, |ui| {
            ui.add_space(6.0);
            ui.label(header(t().col_message));
        });
    });
    ui.separator();

    egui::ScrollArea::vertical().id_salt("rules").auto_shrink([false, true]).max_height(220.0).show(ui, |ui| {
        if rules.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new(t().no_rules).color(p.sub));
        }

        for (i, rule) in rules.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(Vec2::new(W[0], h), centered, |ui| changed |= ui.checkbox(&mut rule.enabled, "").changed());
                changed |= ui.add_sized([W[1], h], egui::DragValue::new(&mut rule.battery).range(0..=100).suffix(pct("")).speed(0.5)).changed();
                ui.allocate_ui_with_layout(Vec2::new(W[2], h), middle_left, |ui| {
                    egui::ComboBox::from_id_salt(("icon", i))
                        .selected_text(t().icon_names[rule.balloon_tip_icon.clamp(0, 3) as usize])
                        .width(W[2] - 8.0)
                        .show_ui(ui, |ui| {
                            for (v, n) in t().icon_names.iter().enumerate() {
                                changed |= ui.selectable_value(&mut rule.balloon_tip_icon, v as i32, *n).changed();
                            }
                        });
                });
                changed |= message_field(ui, p, i, rule, message_w, completion, sample_ctx);
                if ui.add_sized([DELETE_W, h], egui::Button::new(RichText::new(t().delete).color(p.danger)).frame(false)).clicked() {
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

/// 文字位置 (文字数) で切り出す
fn char_slice(chars: &[char], from: usize, to: usize) -> String {
    chars[from.min(chars.len())..to.min(chars.len())].iter().collect()
}

/// 通知内容の入力欄。{ を入力するか Tab で置換文字の候補を出す。変更があれば true
fn message_field(ui: &mut egui::Ui, p: Palette, row: usize, rule: &mut Rule, width: f32, completion: &mut Option<Completion>, sample_ctx: Option<&placeholders::Context>) -> bool {
    let id = ui.make_persistent_id(("message", row));
    let focused = ui.ctx().memory(|m| m.has_focus(id));
    let open_here = completion.as_ref().is_some_and(|c| c.row == row);

    // 入力欄にフォーカスがあるときは Tab を補完に使う (次の欄へ移らない)
    let (mut tab, mut back, mut down, mut up, mut enter, mut esc) = (false, false, false, false, false, false);
    if focused {
        ui.ctx().input_mut(|i| {
            back = i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
            tab = i.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
            if open_here {
                down = i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
                up = i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
                enter = i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                esc = i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            }
        });
    }

    let before = rule.message.clone();
    let mut out = egui::TextEdit::singleline(&mut rule.message).id(id).lock_focus(true).desired_width(width).min_size(Vec2::new(width, CONTROL_H)).align(egui::Align2::LEFT_CENTER).show(ui);
    let mut changed = out.response.changed();
    // 置き換えたプレビュー (候補の一覧を開いている間は、一覧に重なるので出さない)
    if let Some(ctx) = sample_ctx.filter(|_| !open_here) {
        out.response.response =out.response.response.clone().on_hover_text(f(t().preview_fmt, &[&placeholders::expand(&rule.message, &placeholders::Context { threshold: Some(rule.battery), ..ctx.clone() })]));
    }
    let chars: Vec<char> = rule.message.chars().collect();
    let cursor = out.cursor_range.map(|r| r.primary.index.0).unwrap_or(chars.len()).min(chars.len());

    // { を入力したら開く
    if focused && rule.message != before && cursor > 0 && chars[cursor - 1] == '{' && !open_here {
        *completion = Some(Completion { row, start: cursor - 1, ..Default::default() });
    }
    // Tab: 開いていなければ開き、開いていれば次の候補へ (Shift+Tab で前へ)
    let mut moved = 0i32;
    if tab || back {
        if completion.as_ref().is_some_and(|c| c.row == row) {
            moved = if back { -1 } else { 1 };
        } else {
            // カーソルの前に閉じていない { があれば、そこから置き換える
            let brace = chars[..cursor].iter().rposition(|&c| c == '{' || c == '}').filter(|&i| chars[i] == '{');
            *completion = Some(Completion { row, start: brace.unwrap_or(cursor), ..Default::default() });
        }
    }
    moved += i32::from(down) - i32::from(up);

    let Some(comp) = completion.as_mut().filter(|c| c.row == row) else { return changed };

    // 入力中の文字から候補を絞る ('{' の後ろ〜カーソル)
    let has_brace = chars.get(comp.start) == Some(&'{');
    let query = if cursor >= comp.start { char_slice(&chars, comp.start + usize::from(has_brace), cursor) } else { String::new() };
    let close = esc || cursor < comp.start || query.contains(['}', ' ', '{']) || (!focused && !comp.hovered);
    if close {
        *completion = None;
        return changed;
    }
    let list = placeholders::candidates(&query);
    let n = list.len() as i32;
    comp.selected = (comp.selected as i32 + moved).rem_euclid(n.max(1)) as usize;

    // 候補の一覧 (入力欄のすぐ下)
    let mut clicked = None;
    let area = egui::Area::new(id.with("completion")).order(egui::Order::Foreground).fixed_pos(out.response.rect.left_bottom() + Vec2::new(0.0, 4.0)).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_width(i18n::style().completion_w);
            ui.label(RichText::new(t().completion_help).color(p.sub).size(11.0));
            ui.add_space(2.0);
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for (i, d) in list.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), egui::Sense::click());
                    let selected = i == comp.selected;
                    if selected || resp.hovered() {
                        ui.painter().rect_filled(rect, CornerRadius::same(4), if selected { ACCENT.gamma_multiply(0.25) } else { p.card_hover });
                    }
                    let y = rect.center().y;
                    ui.painter().text(egui::pos2(rect.left() + 8.0, y), egui::Align2::LEFT_CENTER, format!("{{{}}}", d.key), egui::FontId::monospace(13.0), p.text);
                    ui.painter().text(egui::pos2(rect.left() + i18n::style().completion_desc_x, y), egui::Align2::LEFT_CENTER, d.desc(), egui::FontId::proportional(12.5), p.text);
                    let sample = match d.key {
                        "battery" => rule.battery.to_string(),
                        "rn" => d.sample(),
                        _ => sample_ctx.and_then(|c| c.value(d.key)).filter(|v| v != t().unknown).unwrap_or_else(|| d.sample()),
                    };
                    ui.painter().text(egui::pos2(rect.right() - 8.0, y), egui::Align2::RIGHT_CENTER, f(t().example_fmt, &[&sample]), egui::FontId::proportional(12.0), p.sub);
                    if selected && moved != 0 {
                        ui.scroll_to_rect(rect, None);
                    }
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        clicked = Some(i);
                    }
                }
            });
        });
    });
    comp.hovered = area.response.hovered() || area.response.contains_pointer();

    // 決定: 入力中の部分を {key} に置き換え、カーソルをその後ろへ
    if let Some(i) = clicked.or(enter.then_some(comp.selected)) {
        if let Some(d) = list.get(i) {
            let insert = format!("{{{}}}", d.key);
            let mut end = cursor;
            if has_brace && chars.get(end) == Some(&'}') {
                end += 1;
            }
            rule.message = char_slice(&chars, 0, comp.start) + &insert + &char_slice(&chars, end, chars.len());
            let pos = comp.start + insert.chars().count();
            out.state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(pos))));
            out.state.store(ui.ctx(), id);
            ui.ctx().memory_mut(|m| m.request_focus(id));
            changed = true;
        }
        *completion = None;
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
        Some(b) if b.connected => pct(b.percentage),
        _ => "--".into(),
    };
    let big = painter.text(rect.min + Vec2::new(pad, 34.0), egui::Align2::LEFT_TOP, percent, egui::FontId::proportional(28.0), accent);

    let mut notes = Vec::new();
    if is_tray {
        notes.push(t().icon_shown.to_string());
    }
    match battery {
        Some(b) if b.connected => {
            let status = b.status();
            if let Some(rest) = status.split_once(" / ") {
                notes.push(rest.1.to_string());
            }
        }
        _ => notes.push(t().disconnected.into()),
    }
    painter.text(egui::pos2(big.max.x + 10.0, big.center().y), egui::Align2::LEFT_CENTER, notes.join(" / "), egui::FontId::proportional(12.5), p.sub);

    // 残量バー
    let bar = egui::Rect::from_min_size(egui::pos2(rect.min.x + pad, rect.max.y - pad - 6.0), Vec2::new(rect.width() - pad * 2.0 - 70.0, 6.0));
    painter.rect_filled(bar, CornerRadius::same(3), p.track);
    if let Some(b) = battery.filter(|b| b.connected && b.percentage > 0) {
        let fill = egui::Rect::from_min_size(bar.min, Vec2::new((bar.width() * b.percentage.clamp(0, 100) as f32 / 100.0).max(6.0), 6.0));
        painter.rect_filled(fill, CornerRadius::same(3), accent);
    }

    // 詳細の開閉
    let hint = if expanded { t().collapse } else { t().expand };
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
fn detail_panel(ui: &mut egui::Ui, p: Palette, battery: Option<&Battery>, samples: &[Sample], h: &health::Health) {
    let now = history::now();
    let offset = log::utc_offset_secs();
    let pending = h.need_days.map(|d| d as usize).unwrap_or_else(|| health::MIN_RUNS - h.runs.min(health::MIN_RUNS));

    egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(16).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.columns(2, |cols| {
            // 左: 減り具合
            let ui = &mut cols[0];
            heading_with_info(ui, p, t().trend_title, t().trend_info);
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width() - 12.0, 150.0), egui::Sense::hover());
            let chart = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - 18.0));
            draw_trend(ui.painter(), chart, p, battery, samples, h, offset);

            // 右: 健康状態
            let ui = &mut cols[1];
            heading_with_info(
                ui,
                p,
                t().health_title,
                t().health_info,
            );
            ui.add_space(6.0);

            // (項目, 値, 値が出ているか, 説明)
            let mut rows: Vec<(&str, String, bool, String)> = Vec::new();

            let ghub_wh = battery.and_then(|b| Some(b.power_mw? * b.max_life_hours? / 1000.0));
            let tx = t();
            let about_h = |h: f64| f(tx.about_fmt, &[&placeholders::duration_text((h * 3600.0) as u64)]);
            rows.push(match (h.capacity_mwh, ghub_wh) {
                (Some(c), g) => (
                    tx.est_capacity,
                    format!("{} Wh", dec(c / 1000.0, 2)),
                    true,
                    f(tx.cap_measured_fmt, &[&g.map(|g| f(tx.cap_ghub_fmt, &[&dec(g, 2)])).unwrap_or_default()]),
                ),
                (None, Some(g)) => (tx.est_capacity, format!("{} Wh", dec(g, 2)), true, tx.ghub_estimate_info.into()),
                (None, None) => (tx.est_capacity, tx.unknown.into(), false, tx.cap_unknown_info.into()),
            });
            rows.push(match (h.full_runtime_h, battery.and_then(|b| b.max_life_hours)) {
                (Some(r), _) => (tx.full_runtime, about_h(r), true, tx.runtime_info.into()),
                (None, Some(g)) => (tx.full_runtime, about_h(g), true, tx.ghub_estimate_info.into()),
                _ => (tx.full_runtime, tx.unknown.into(), false, tx.runtime_unknown_info.into()),
            });
            rows.push(match h.health {
                Some(v) => (tx.capacity, pct(format!("{:.0}", v * 100.0)), true, tx.capacity_info.into()),
                None => (tx.capacity, tx.recording.into(), false, f(tx.capacity_pending_fmt, &[&pending])),
            });
            rows.push(match h.daily_drain {
                Some(d) => (tx.per_charge, f(tx.about_fmt, &[&f(tx.days_fmt, &[&dec(100.0 / d, 1)])]), true, f(tx.per_charge_info_fmt, &[&pct(format!("-{}", dec(d, 1)))])),
                None => (tx.per_charge, tx.recording.into(), false, tx.need_a_day.into()),
            });
            rows.push(match h.charge_time_h {
                Some(c) => (tx.charge_time, about_h(c), true, tx.charge_time_info.into()),
                None => (tx.charge_time, tx.recording.into(), false, tx.charge_time_info.into()),
            });
            rows.push((tx.cycles_label, f(tx.cycles_fmt, &[&dec(h.cycles, 1)]), true, tx.cycles_info.into()));

            let when = |label: &'static str, at: Option<(u64, Basis)>, level: &str| -> (&'static str, String, bool, String) {
                let what = f(tx.reach_fmt, &[&level]);
                match at {
                    Some((at, _)) if at <= now => (label, tx.reached.into(), true, what),
                    Some((at, basis)) => {
                        let (y, m) = health::year_month(at);
                        let why = match basis {
                            Basis::Trend => tx.basis_trend,
                            Basis::Cycles => tx.basis_cycles,
                        };
                        (label, placeholders::month_year_text(y, m), true, format!("{what}\n{why}"))
                    }
                    None => (label, tx.recording.into(), false, format!("{what}\n{}", tx.need_a_day)),
                }
            };
            rows.push(when(tx.degrade, h.degrade_at, "80%"));
            rows.push(when(tx.replace, h.replace_at, "70%"));

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

        ui.add_space(14.0);
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            heading_with_info(ui, p, t().profile_title, t().profile_info);
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width() - 12.0, 120.0), egui::Sense::hover());
            draw_profile(ui.painter(), rect, p, h.profile.as_ref(), local_hour_now(now, offset));

            let ui = &mut cols[1];
            heading_with_info(ui, p, t().daily_title, t().daily_info);
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width() - 12.0, 120.0), egui::Sense::hover());
            draw_daily(ui.painter(), rect, p, &h.daily, offset);
        });
    });
}

fn hours_text(h: f64) -> String {
    f(t().about_fmt, &[&placeholders::duration_text((h * 3600.0) as u64)])
}

fn draw_trend(painter: &egui::Painter, area: egui::Rect, p: Palette, battery: Option<&Battery>, samples: &[Sample], h: &health::Health, offset: i64) {
    painter.rect_filled(area, CornerRadius::same(4), p.chart_bg);
    for pct in [0.0, 50.0, 100.0] {
        let y = area.bottom() - area.height() * pct / 100.0;
        painter.line_segment([egui::pos2(area.left(), y), egui::pos2(area.right(), y)], Stroke::new(1.0, p.chart_grid));
    }
    let small = egui::FontId::proportional(11.0);
    let label_y = area.bottom() + 3.0;

    let Some(b) = battery.filter(|b| b.connected) else {
        painter.text(area.center(), egui::Align2::CENTER_CENTER, t().no_data, small, p.sub);
        return;
    };

    let now = history::now();
    let trend = history::trend(samples, now, b);
    // 予測: 使い方の統計があれば時間帯に沿った曲線、なければ今の減り方の直線
    let curve = if b.charging { None } else { crate::stats::forecast(h, now, b.percentage as f64, trend.drain_per_hour, offset) };
    let empty = curve.as_deref().and_then(crate::stats::empty_at).or_else(|| trend.empty_at(now, b.percentage));

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
    match (&curve, empty) {
        (Some(c), _) if c.len() >= 2 => {
            let pts: Vec<Pos2> = c.iter().filter(|(t, _)| *t <= t1).map(|(t, v)| at(*t, *v)).collect();
            painter.extend(egui::Shape::dashed_line(&pts, line, 4.0, 3.0));
        }
        (_, Some(end)) => {
            let from = at(now, b.percentage as f64);
            painter.extend(egui::Shape::dashed_line(&[from, at(end, 0.0)], line, 4.0, 3.0));
        }
        _ => {}
    }
    painter.circle_filled(at(now, b.percentage as f64), 3.0, p.chart_line);

    // 下のラベル
    let past_h = (now - t0) as f64 / 3600.0;
    let left = if trend.actual.len() >= 2 { f(t().since_fmt, &[&hours_text(past_h)]) } else { t().recording_history.into() };
    painter.text(egui::pos2(area.left(), label_y), egui::Align2::LEFT_TOP, left, small.clone(), p.sub);
    let right = match (b.charging, empty, trend.drain_per_hour) {
        (true, _, _) => t().charging.to_string(),
        (false, Some(end), Some(rate)) => f(t().empty_in_fmt, &[&hours_text((end - now) as f64 / 3600.0), &dec(rate, 1), &t().per_hour]),
        _ => t().calculating.to_string(),
    };
    painter.text(egui::pos2(area.right(), label_y), egui::Align2::RIGHT_TOP, right, small, p.sub);
}

fn local_hour_now(now: u64, offset: i64) -> usize {
    ((now as i64 + offset).rem_euclid(86_400) / 3600) as usize
}

/// 時間帯ごとの減り方 (24 本の棒。今の時間帯を強調)
fn draw_profile(painter: &egui::Painter, area: egui::Rect, p: Palette, profile: Option<&[f64; 24]>, now_hour: usize) {
    let chart = egui::Rect::from_min_max(area.min, egui::pos2(area.max.x, area.max.y - 16.0));
    painter.rect_filled(chart, CornerRadius::same(4), p.chart_bg);
    let small = egui::FontId::proportional(10.5);
    let Some(profile) = profile else {
        painter.text(chart.center(), egui::Align2::CENTER_CENTER, t().recording, small, p.sub);
        return;
    };
    let max = profile.iter().cloned().fold(0.0, f64::max).max(0.1);
    let w = chart.width() / 24.0;
    for (i, v) in profile.iter().enumerate() {
        let hgt = (chart.height() - 14.0) * (*v / max) as f32;
        let x = chart.left() + w * i as f32;
        let bar = egui::Rect::from_min_max(egui::pos2(x + 1.5, chart.bottom() - hgt), egui::pos2(x + w - 1.5, chart.bottom()));
        let color = if i == now_hour { p.chart_line } else { p.chart_line.gamma_multiply(0.45) };
        painter.rect_filled(bar, CornerRadius::same(2), color);
    }
    // 最大値 (左上) と時刻 (下)
    painter.text(chart.left_top() + Vec2::new(4.0, 2.0), egui::Align2::LEFT_TOP, format!("{}{}", dec(max, 1), t().per_hour), small.clone(), p.sub);
    for hr in [0, 6, 12, 18] {
        painter.text(egui::pos2(chart.left() + w * hr as f32, chart.bottom() + 2.0), egui::Align2::LEFT_TOP, format!("{hr}:00"), small.clone(), p.sub);
    }
}

/// 1 日ごとの消費 (直近 30 日の棒)
fn draw_daily(painter: &egui::Painter, area: egui::Rect, p: Palette, daily: &[(u64, f64)], offset: i64) {
    let chart = egui::Rect::from_min_max(area.min, egui::pos2(area.max.x, area.max.y - 16.0));
    painter.rect_filled(chart, CornerRadius::same(4), p.chart_bg);
    let small = egui::FontId::proportional(10.5);
    if daily.iter().all(|d| d.1 <= 0.0) {
        painter.text(chart.center(), egui::Align2::CENTER_CENTER, t().recording, small, p.sub);
        return;
    }
    let max = daily.iter().map(|d| d.1).fold(0.0, f64::max).max(1.0);
    let n = crate::stats::RECENT_DAYS as f32;
    let w = chart.width() / n;
    // 右端が今日。足りない日は左を空ける
    let skip = n as usize - daily.len().min(n as usize);
    for (i, (_, v)) in daily.iter().enumerate() {
        let hgt = (chart.height() - 14.0) * (*v / max) as f32;
        let x = chart.left() + w * (skip + i) as f32;
        let bar = egui::Rect::from_min_max(egui::pos2(x + 1.0, chart.bottom() - hgt), egui::pos2(x + w - 1.0, chart.bottom()));
        let color = if i + 1 == daily.len() { p.chart_line } else { p.chart_line.gamma_multiply(0.45) };
        painter.rect_filled(bar, CornerRadius::same(2), color);
    }
    painter.text(chart.left_top() + Vec2::new(4.0, 2.0), egui::Align2::LEFT_TOP, pct(format!("{:.0}", max)), small.clone(), p.sub);
    // 最初と最後の日付
    let date = |day: u64| {
        let (_, m, d) = health::ymd((day as i64 + offset).max(0) as u64);
        f(t().date_md_fmt, &[&m, &d])
    };
    if let (Some(first), Some(last)) = (daily.first(), daily.last()) {
        painter.text(egui::pos2(chart.left() + w * skip as f32, chart.bottom() + 2.0), egui::Align2::LEFT_TOP, date(first.0), small.clone(), p.sub);
        painter.text(egui::pos2(chart.right(), chart.bottom() + 2.0), egui::Align2::RIGHT_TOP, date(last.0), small, p.sub);
    }
}

fn setup_style(ctx: &egui::Context, dark: bool) {
    // 表示言語に合わせて Windows のフォントを読み込む (言語が変わったときだけ)
    apply_fonts(ctx, false);

    let p = Palette::new(dark);
    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
    ctx.global_style_mut(|style| {
        style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(i18n::style().body));
        style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(i18n::style().body));
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

/// 予備フォントの字面を枠の上下中央に合わせる量
fn fallback_offset(file: &str) -> f32 {
    match file {
        "YuGothM.ttc" => 0.32,
        "malgun.ttf" | "msyh.ttc" => 0.1,
        _ => 0.0,
    }
}

/// 言語ごとのフォント: 先頭に見つかったものを優先し、2 つ目以降は他の文字 (日本語のデバイス名など) の予備にする
/// 読み込んだフォントの組み合わせ (言語, 予備のフォントを含むか)
static FONT_SET: std::sync::Mutex<Option<(Language, bool)>> = std::sync::Mutex::new(None);

/// 今の言語のフォントを入れる。with_extra なら予備のフォント (韓国語・中国語) も足す。一度足したら外さない
fn apply_fonts(ctx: &egui::Context, with_extra: bool) {
    let lang = i18n::current();
    let Ok(mut loaded) = FONT_SET.lock() else { return };
    let extra = with_extra || loaded.is_some_and(|(_, e)| e);
    if *loaded != Some((lang, extra)) {
        *loaded = Some((lang, extra));
        ctx.set_fonts(load_fonts(i18n::style(), extra));
    }
}

fn load_fonts(style: &i18n::Style, extra: bool) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let mut first = true;
    let mut order = 0;
    let files = style.fonts.iter().chain(i18n::EXTRA_FONTS.iter().filter(|f| extra && !style.fonts.contains(f)));
    for file in files {
        let Ok(bytes) = std::fs::read(format!(r"{windir}\Fonts\{file}")) else { continue };
        let mut data = egui::FontData::from_owned(bytes);
        // 字面の上下位置はフォントごとに違う。主フォントは言語の設定値、予備はフォントごとの値
        data.tweak.y_offset_factor = if first { style.y_offset } else { fallback_offset(file) };
        let name = file.to_string();
        fonts.font_data.insert(name.clone(), std::sync::Arc::new(data));
        fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(order, name.clone());
        // 等幅は内蔵の等幅フォントを優先し、足りない文字だけ補う
        fonts.families.entry(egui::FontFamily::Monospace).or_default().push(name.clone());
        // 一番目はそのまま優先。予備は既定のフォント (egui 内蔵) より前に並べる
        order += 1;
        first = false;
    }
    fonts
}
