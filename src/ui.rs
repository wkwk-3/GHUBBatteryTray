//! 設定画面。トレイから `--settings` 付きで別プロセスとして起動され、閉じると終了する。

use crate::ghub::{Battery, Source};
use crate::icon::level_color;
use crate::settings::{Rule, Settings, Theme, WebhookFormat};
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
            .with_inner_size([840.0, 640.0])
            .with_min_inner_size([660.0, 480.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("GHUB Battery 設定", options, Box::new(|cc| Ok(Box::new(SettingsApp::new(cc))))) {
        crate::settings::log(&format!("[ERROR] 設定画面を開けませんでした: {e}"));
    }
}

#[derive(PartialEq)]
enum Tab {
    Devices,
    Webhook,
}

struct SettingsApp {
    settings: Settings,
    devices: Vec<Battery>,
    /// デバイス名 (小文字) -> 残量の履歴
    history: HashMap<String, Vec<Sample>>,
    loading: Option<Receiver<(Vec<Battery>, HashMap<String, Vec<Sample>>)>>,
    selected: String,
    status: String,
    dirty: bool,
    tab: Tab,
    show_url: bool,
    webhook_test: Option<Receiver<Result<u16, String>>>,
    webhook_result: Option<Result<u16, String>>,
    dark: bool,
    dark_checked: Instant,
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
            loading: None,
            status: String::new(),
            dirty: false,
            // `--tab webhook` で Webhook タブから開く
            tab: if std::env::args().any(|a| a == "webhook") { Tab::Webhook } else { Tab::Devices },
            show_url: false,
            webhook_test: None,
            webhook_result: None,
            dark,
            dark_checked: Instant::now(),
        };
        app.refresh(&cc.egui_ctx);
        app
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let devices = sources::collect(&mut hidpp::Reader::new());
            let _ = tx.send((devices, history::load_all(&history::path())));
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
                notify_tray();
            }
            Err(e) => self.status = format!("保存に失敗しました: {e}"),
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
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.loading {
            if let Ok((devices, history)) = rx.try_recv() {
                self.devices = devices;
                self.history = history;
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
                self.webhook_result = Some(r);
                self.webhook_test = None;
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
    unsafe {
        if let Ok(hwnd) = FindWindowW(crate::tray::WINDOW_CLASS, None) {
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
                    ui.label(RichText::new("通知内容で使える置換:  {name} デバイス名   {battery} しきい値   {rn} 改行").color(p.sub).size(12.5));
                    ui.add_space(8.0);
                }
                ui.horizontal(|ui| {
                    if self.tab == Tab::Devices {
                        if ui.add_enabled(!self.selected.is_empty(), egui::Button::new("＋ ルール追加").min_size(Vec2::new(110.0, 34.0))).clicked() {
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
            });
        });
    }
}

impl SettingsApp {
    fn devices_tab(&mut self, ui: &mut egui::Ui, p: Palette) {
        let names = self.device_names();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(12.0, 12.0);
            for (name, battery) in &names {
                let is_tray = self.settings.is_tray(name);
                let samples = self.history.get(&name.to_ascii_lowercase()).map(Vec::as_slice).unwrap_or(&[]);
                if device_card(ui, p, name, battery.as_ref(), samples, name.eq_ignore_ascii_case(&self.selected), is_tray).clicked() {
                    self.selected = name.clone();
                }
            }
            if names.is_empty() && self.loading.is_none() {
                ui.label(RichText::new("バッテリー対応デバイスが見つかりません。受信機が挿さっているか確認してください。").color(p.sub));
            }
        });

        ui.add_space(14.0);
        if self.selected.is_empty() {
            return;
        }

        let selected_label = names.iter().find(|(n, _)| n == &self.selected).map(label).unwrap_or(&self.selected).to_string();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("通知ルール  —  {selected_label}")).size(18.0).strong().color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.checkbox(&mut self.settings.notify_full_charge, "充電完了を通知").changed() {
                    self.dirty = true;
                }
                let mut tray = self.settings.is_tray(&self.selected);
                if ui.checkbox(&mut tray, "このデバイスをトレイに表示").on_hover_text("デバイスごとに別のアイコンで表示します").changed() {
                    let selected = self.selected.clone();
                    self.settings.set_tray(&selected, tray);
                    self.dirty = true;
                }
            });
        });
        ui.add_space(6.0);

        egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(12).show(ui, |ui| {
            ui.set_min_size(Vec2::new(ui.available_width(), ui.available_height().max(120.0)));
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

            ui.label(RichText::new("Webhook 通知").size(18.0).strong().color(p.text));
            ui.label(RichText::new("残量低下・充電完了の通知を Discord / Slack などにも送ります。").color(p.sub));
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

fn format_label(f: WebhookFormat) -> &'static str {
    match f {
        WebhookFormat::Discord => "Discord",
        WebhookFormat::Slack => "Slack",
        WebhookFormat::Json => "汎用 JSON",
    }
}

/// 通知ルール表。変更があれば true
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

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if rules.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("ルールがありません。「＋ ルール追加」で追加できます。").color(p.sub));
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

fn device_card(ui: &mut egui::Ui, p: Palette, name: &str, battery: Option<&Battery>, samples: &[Sample], selected: bool, is_tray: bool) -> egui::Response {
    let (r, g, b) = level_color(battery);
    let accent = Color32::from_rgb(r, g, b);
    let size = Vec2::new(340.0, 182.0);
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
        notes.push("トレイ表示中".to_string());
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
    let bar = egui::Rect::from_min_size(egui::pos2(rect.min.x + pad, rect.min.y + 80.0), Vec2::new(rect.width() - pad * 2.0, 6.0));
    painter.rect_filled(bar, CornerRadius::same(3), p.track);
    if let Some(b) = battery.filter(|b| b.connected && b.percentage > 0) {
        let fill = egui::Rect::from_min_size(bar.min, Vec2::new((bar.width() * b.percentage.clamp(0, 100) as f32 / 100.0).max(6.0), 6.0));
        painter.rect_filled(fill, CornerRadius::same(3), accent);
    }

    // バーの下: 減り具合 (実線) と このまま減った場合の予測 (点線)
    let chart = egui::Rect::from_min_max(egui::pos2(bar.min.x, bar.max.y + 10.0), egui::pos2(bar.max.x, rect.max.y - pad - 18.0));
    draw_trend(&painter, chart, p, battery, samples);

    response.on_hover_cursor(egui::CursorIcon::PointingHand)
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
