//! 常駐プロセス。Win32 の隠しウィンドウ + 通知領域アイコンだけで動き、
//! バッテリーの取得は別スレッドで 60 秒ごと (直接通信 > G HUB API > settings.db)。

use crate::ghub::Battery;
use crate::settings::{self, Settings, Theme};
use crate::{hidpp, icon, log, sources, startup, updater, webhook};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

pub const WINDOW_CLASS: PCWSTR = w!("GHUBBatteryTrayWnd");
const WM_APP_UPDATE: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
/// 設定画面が保存したときに送ってくる
pub const WM_APP_RELOAD: u32 = WM_APP + 3;
/// 更新のために終了してほしい (設定画面から)
pub const WM_APP_QUIT: u32 = WM_APP + 4;
/// 新しいバージョンが見つかった (確認スレッドから)
const WM_APP_UPDATE_FOUND: u32 = WM_APP + 5;
/// 通知 (バルーン) がクリックされた
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
const ID_UPDATE: u32 = 6;
/// フルスクリーンが終わったか確かめるタイマー
const TIMER_FULLSCREEN: usize = 1;
const FULLSCREEN_POLL_MS: u32 = 2000;

const ID_NONE: u32 = 999;
const ID_DEVICE: u32 = 1000;
const ID_SETTINGS: u32 = 1;
const ID_REFRESH: u32 = 2;
const ID_STARTUP: u32 = 3;
const ID_EXIT: u32 = 4;
const ID_WEBHOOK: u32 = 5;
const ID_THEME: u32 = 10;
const POLL_INTERVAL: Duration = Duration::from_secs(60);


#[derive(Default)]
struct Shared {
    batteries: Vec<Battery>,
    /// 1 回以上取得を終えたか
    loaded: bool,
}

struct App {
    hwnd: HWND,
    shared: Arc<Mutex<Shared>>,
    refresh: Arc<AtomicBool>,
    settings: Settings,
    settings_time: Option<std::time::SystemTime>,
    /// 表示中のアイコン。i 番目が uID = i + 1
    icons: Vec<HICON>,
    /// 通知済み (デバイス, しきい値)
    notified: HashSet<(String, i32)>,
    /// 満充電を通知済みのデバイス
    full_notified: HashSet<String>,
    taskbar_created: u32,
    /// 最後に出した通知が更新のお知らせか (クリックで設定を開く)
    update_balloon: bool,
    /// フルスクリーン中のためためている通知
    pending: Vec<Pending>,
}

/// フルスクリーン中にためておく通知
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub title: String,
    pub text: String,
    pub kind: i32,
    pub is_update: bool,
}

/// ためていた通知を 1 つの通知にまとめる。(タイトル, 本文, アイコン)
pub fn combine(items: &[Pending]) -> (String, String, i32) {
    if let [one] = items {
        return (one.title.clone(), one.text.clone(), one.kind);
    }
    let lines: Vec<String> = items.iter().map(|p| format!("・{}: {}", p.title, p.text.replace("\r\n", " "))).collect();
    let mut text = lines.join("\n");
    // バルーンの本文は 255 文字まで
    if text.chars().count() > 250 {
        text = text.chars().take(248).collect::<String>() + "…";
    }
    // アイコンは一番重いもの (エラー > 警告 > 情報 > なし)
    let kind = items.iter().map(|p| p.kind).max().unwrap_or(0);
    (crate::i18n::f(crate::i18n::t().pending_fmt, &[&items.len()]), text, kind)
}

static mut APP: Option<App> = None;
/// 長期の統計 (取得スレッドが積み上げ、終了時に書き出す)
static STATS: Mutex<Option<crate::stats::Recorder>> = Mutex::new(None);

#[allow(static_mut_refs)]
fn app() -> &'static mut App {
    unsafe { APP.as_mut().unwrap() }
}

pub fn run() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: WINDOW_CLASS, ..Default::default() };
        RegisterClassW(&wc);

        let title = window_title();
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), WINDOW_CLASS, &title, WINDOW_STYLE(0), 0, 0, 0, 0, None, None, Some(instance.into()), None)
            .expect("CreateWindowExW");

        let shared = Arc::new(Mutex::new(Shared::default()));
        let refresh = Arc::new(AtomicBool::new(false));

        APP = Some(App {
            hwnd,
            shared: shared.clone(),
            refresh: refresh.clone(),
            settings: Settings::load(),
            settings_time: settings::modified(),
            icons: Vec::new(),
            notified: HashSet::new(),
            full_notified: HashSet::new(),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            update_balloon: false,
            pending: Vec::new(),
        });

        apply_menu_theme();
        update_icon();

        log::info("I011", format!("アプリを開始しました (バージョン {})", env!("CARGO_PKG_VERSION")));
        crate::envinfo::log_startup(&app().settings);
        updater::cleanup_old();
        if std::env::args().any(|a| a == "--after-update") {
            notify("GHUB Battery", &crate::i18n::f(crate::i18n::t().updated_fmt, &[&updater::CURRENT]), 1);
        }

        let hwnd_raw = hwnd.0 as isize;
        // デバイスからの通知を待ち受け、届いたらすぐ読み直す
        let wake = refresh.clone();
        spawn_named("listener", move || {
            hidpp::listen(move |why| {
                log::debug("D012", format!("今すぐ読み直します ({why})"));
                wake.store(true, Ordering::Relaxed);
            })
        });
        spawn_named("worker", move || worker(hwnd_raw, shared, refresh));
        spawn_named("update_checker", move || update_checker(hwnd_raw));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        for uid in 1..=app().icons.len() as u32 {
            remove_tray_icon(uid);
        }
        // 統計の今の 1 時間分を書き出してから終わる
        if let Ok(mut stats) = STATS.lock() {
            if let Some(s) = stats.as_mut() {
                s.flush_all(crate::history::now());
            }
        }
        log::info("I012", "アプリを終了しました");
    }
}

/// 前回と比べてデバイスの状態が変わったらログに残す
fn log_changes(prev: &[Battery], list: &[Battery]) {
    let src = |b: &Battery| b.source.map(|s| s.label()).unwrap_or("-");
    for b in list {
        match prev.iter().find(|p| p.device_name == b.device_name) {
            None => log::info("I013", format!("デバイスを検出: {} {}% (取得元: {})", b.label(), b.percentage, src(b))),
            Some(p) => {
                if p.source != b.source {
                    log::info("I014", format!("取得元が変わりました: {} {} → {}", b.label(), src(p), src(b)));
                }
                if p.charging != b.charging && b.connected {
                    log::info("I015", format!("{}: {} ({}%)", b.label(), if b.charging { "充電を開始" } else { "充電を終了" }, b.percentage));
                }
                if p.asleep != b.asleep {
                    log::info("I016", format!("{}: {}", b.label(), if b.asleep { "スリープ中 (前回の値を表示)" } else { "スリープから復帰" }));
                }
                if p.connected != b.connected {
                    log::info("I017", format!("{}: {}", b.label(), if b.connected { "接続" } else { "未接続" }));
                }
            }
        }
    }
    for p in prev.iter().filter(|p| !list.iter().any(|b| b.device_name == p.device_name)) {
        log::info("I018", format!("デバイスが切断されました: {}", p.label()));
    }
    if list.is_empty() && !prev.is_empty() {
        log::warn("W010", "バッテリー情報を取得できませんでした (直接通信・G HUB API・settings.db すべて)");
    }
}

/// 設定フォルダーごとに別のウィンドウ名 (設定画面から自分の常駐だけに知らせるため)
fn window_title() -> HSTRING {
    HSTRING::from(format!("GHUB Battery {}", settings::instance_id()))
}

/// 同じ設定フォルダーの常駐ウィンドウ
pub fn find_window() -> Option<HWND> {
    unsafe { FindWindowW(WINDOW_CLASS, &window_title()).ok().filter(|h| !h.is_invalid()) }
}

/// 常駐に終了を頼む (更新時)
pub fn request_quit() {
    if let Some(hwnd) = find_window() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

/// 更新の確認: 起動時と 1 日 1 回 (設定でオフなら何もしない)
fn update_checker(hwnd: isize) {
    // 更新直後は古い設定画面が .old を掴んでいることがあるので、少し待って消し直す
    for _ in 0..6 {
        std::thread::sleep(Duration::from_secs(5));
        updater::cleanup_old();
    }
    let mut first = true;
    loop {
        if Settings::load().check_updates {
            let state = updater::State::load();
            if first || crate::history::now() >= state.last_check + updater::CHECK_INTERVAL_SECS {
                match updater::check() {
                    Ok(mut state) => {
                        if let Some(tag) = state.available().map(|r| r.tag.clone()) {
                            if state.notified_tag != tag {
                                state.notified_tag = tag;
                                state.save();
                                unsafe {
                                    let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_UPDATE_FOUND, WPARAM(0), LPARAM(0));
                                }
                            }
                        }
                    }
                    Err(e) => log::warn("W011", format!("更新の確認に失敗しました: {e}")),
                }
            }
        }
        first = false;
        std::thread::sleep(Duration::from_secs(30 * 60));
    }
}

/// 名前付きのスレッド。作れなければログに残す
fn spawn_named(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        log::error("E016", format!("スレッド {name} を開始できませんでした: {e}"));
    }
}

/// 充電中の残量の上がり方から、電池に入る電力と満充電までの時間を求める
#[derive(Default)]
struct ChargeTracker {
    /// デバイスごとの、充電中に残量が変わった時刻 (unix 秒) と残量
    ticks: std::collections::HashMap<String, Vec<(f64, i32)>>,
    /// デバイスごとの推定容量 (mWh) と、求めた時刻。統計から求めるので 1 時間は使い回す
    capacity: std::collections::HashMap<String, (std::time::Instant, Option<f64>)>,
}

impl ChargeTracker {
    fn apply(&mut self, list: &mut [Battery]) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        for b in list.iter_mut() {
            let key = b.device_name.to_ascii_lowercase();
            if !(b.connected && b.charging && !b.asleep && !b.fully_charged) {
                self.ticks.remove(&key);
                continue;
            }
            let ticks = self.ticks.entry(key.clone()).or_default();
            if ticks.last().map(|x| x.1) != Some(b.percentage) {
                ticks.push((now, b.percentage));
                // 直近の 10 段で測る (満充電に近づくと遅くなるため)
                if ticks.len() > 11 {
                    ticks.remove(0);
                }
            }
            // 最初の点は充電を見つけた時点 (1% の途中) なので、2 回目以降の変化の間で測る
            if ticks.len() < 3 {
                continue;
            }
            let ((t0, p0), (t1, p1)) = (ticks[1], ticks[ticks.len() - 1]);
            if p1 <= p0 || t1 <= t0 {
                continue;
            }
            let rate = (p1 - p0) as f64 / (t1 - t0) * 3600.0;
            b.charge_rate = Some(rate);
            b.charge_power_mw = self.capacity_mwh(&key, b).map(|c| c * rate / 100.0);
        }
    }

    /// 推定容量: 長期の統計 (放電量と減った残量から) があればそれ、なければ G HUB の放電量 × 満充電での持ち時間
    fn capacity_mwh(&mut self, key: &str, b: &Battery) -> Option<f64> {
        let fresh = self.capacity.get(key).filter(|(at, _)| at.elapsed() < Duration::from_secs(3600)).map(|(_, c)| *c);
        let from_stats = fresh.unwrap_or_else(|| {
            let hours = crate::stats::load(&crate::stats::path_for(key));
            let c = crate::stats::analyze(&hours, &[], crate::history::now(), log::utc_offset_secs()).capacity_mwh;
            self.capacity.insert(key.to_string(), (std::time::Instant::now(), c));
            c
        });
        from_stats.or_else(|| Some(b.power_mw? * b.max_life_hours?))
    }
}

/// 取得スレッド
fn worker(hwnd: isize, shared: Arc<Mutex<Shared>>, refresh: Arc<AtomicBool>) {
    let mut reader = hidpp::Reader::new();
    let mut charge = ChargeTracker::default();
    let mut recorder = crate::history::Recorder::default();
    let mut prev: Vec<Battery> = Vec::new();
    loop {
        let started = std::time::Instant::now();
        let mut list = sources::collect(&mut reader);
        charge.apply(&mut list);
        // 直接通信と G HUB で別の名前に分かれていた記録を、G HUB 側の名前にまとめる (起動時と対応が増えたとき)
        if sources::ALIASES_CHANGED.swap(false, Ordering::Relaxed) {
            let mut merged = false;
            for (old, new) in sources::load_aliases().into_iter().filter(|(o, n)| !o.eq_ignore_ascii_case(n)) {
                if let Ok(mut stats) = STATS.lock() {
                    if let Some(s) = stats.as_mut() {
                        s.forget(&old);
                    }
                }
                merged |= crate::migrate::run(&old, &new).settings;
            }
            if merged {
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_RELOAD, WPARAM(0), LPARAM(0));
                }
            }
        }
        if log::debug_enabled() {
            let summary: Vec<String> = list.iter().map(|b| format!("{} {}%{}{} [{}]", b.label(), b.percentage, if b.charging { " 充電中" } else { "" }, if b.asleep { " スリープ" } else { "" }, b.source.map(|s| s.label()).unwrap_or("?"))).collect();
            log::debug("D006", format!("取得結果: {} ({} ms)", if summary.is_empty() { "なし".into() } else { summary.join(", ") }, started.elapsed().as_millis()));
        }
        recorder.record(&list);
        if let Ok(mut stats) = STATS.lock() {
            stats.get_or_insert_with(crate::stats::Recorder::default).observe(&list, crate::history::now());
        }
        log_changes(&prev, &list);
        prev = list.clone();
        if let Ok(mut s) = shared.lock() {
            s.batteries = list;
            s.loaded = true;
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_UPDATE, WPARAM(0), LPARAM(0));
        }
        // 設定画面が開いていれば、最新の残量をすぐ送る
        crate::live::send(&prev);

        // 次の取得まで待つ (「残量を今すぐ再取得」ならすぐ)
        let mut waited = Duration::ZERO;
        while waited < POLL_INTERVAL && !refresh.swap(false, Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
            waited += Duration::from_millis(500);
        }
    }
}

fn tray_data(uid: u32) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: app().hwnd,
        uID: uid,
        ..Default::default()
    }
}

fn add_tray_icon(uid: u32) {
    let mut nid = tray_data(uid);
    nid.uFlags = NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_APP_TRAY;
    copy_wstr(&mut nid.szTip, "GHUB Battery");
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            log::error("E007", format!("トレイアイコンを登録できませんでした (番号 {uid}): {}", windows::core::Error::from_thread()));
        }
    }
}

fn remove_tray_icon(uid: u32) {
    let nid = tray_data(uid);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// アイコンごとに表示するデバイス。未選択なら案内用のアイコンを 1 つ出す ("")
pub fn icon_slots(settings: &Settings) -> Vec<String> {
    if settings.tray_display_devices.is_empty() { vec![String::new()] } else { settings.tray_display_devices.clone() }
}

fn copy_wstr(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for c in s.encode_utf16().take(dst.len() - 1) {
        dst[n] = c;
        n += 1;
    }
    dst[n] = 0;
}

fn icon_size() -> i32 {
    unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }
}

fn batteries() -> Vec<Battery> {
    app().shared.lock().map(|s| s.batteries.clone()).unwrap_or_default()
}

fn loaded() -> bool {
    app().shared.lock().map(|s| s.loaded).unwrap_or(false)
}

fn reload_settings_if_changed() {
    let time = settings::modified();
    if time != app().settings_time {
        app().settings = Settings::load();
        app().settings_time = time;
        log::info("I019", "設定を読み込み直しました");
    }
}

/// 選択中のデバイスの数だけアイコンを出し、それぞれを最新の残量で描き直す
fn update_icon() {
    let list = batteries();
    let slots = icon_slots(&app().settings);
    let a = app();

    while a.icons.len() < slots.len() {
        add_tray_icon(a.icons.len() as u32 + 1);
        a.icons.push(HICON::default());
    }
    while a.icons.len() > slots.len() {
        remove_tray_icon(a.icons.len() as u32);
        icon::destroy(a.icons.pop().unwrap_or_default());
    }

    for (i, tray) in slots.iter().enumerate() {
        let battery = list.iter().find(|b| !tray.is_empty() && b.device_name.eq_ignore_ascii_case(tray));
        let t = crate::i18n::t();
        let tip = match battery {
            Some(b) => format!("{}\n{}", b.label(), b.status()),
            None if !loaded() => format!("GHUB Battery\n{}", t.loading),
            None if tray.is_empty() && list.is_empty() => format!("GHUB Battery\n{}", t.tip_no_info),
            None if tray.is_empty() => format!("GHUB Battery\n{}", t.tip_pick),
            None => format!("{tray}\n{}", t.no_devices),
        };

        let new_icon = icon::create(battery, icon_size(), &a.settings.level_colors);
        let mut nid = tray_data(i as u32 + 1);
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = new_icon;
        copy_wstr(&mut nid.szTip, &tip);
        let ok = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() };
        log::problem("tray_modify", (!ok).then(|| ("W015", format!("トレイアイコンを更新できません (番号 {}): {}", i + 1, windows::core::Error::from_thread()))), ("I043", "トレイアイコンを更新できるようになりました"));
        icon::destroy(std::mem::replace(&mut a.icons[i], new_icon));
    }
}

fn notify(title: &str, text: &str, kind: i32) {
    notify_ex(title, text, kind, false);
}

/// フルスクリーン中 (設定がオンのとき) はためておき、終わったらまとめて出す
fn notify_ex(title: &str, text: &str, kind: i32, is_update: bool) {
    let a = app();
    if a.settings.defer_in_fullscreen && crate::fullscreen::active() {
        log::info("I020", format!("フルスクリーン中のため通知を保留: {title} / {}", text.replace("\r\n", " ")));
        a.pending.push(Pending { title: title.into(), text: text.into(), kind, is_update });
        unsafe {
            SetTimer(Some(a.hwnd), TIMER_FULLSCREEN, FULLSCREEN_POLL_MS, None);
        }
        return;
    }
    show_balloon(title, text, kind);
    app().update_balloon = is_update;
}

/// フルスクリーンが終わっていたら、ためていた通知をまとめて出す
fn flush_pending() {
    let a = app();
    if a.pending.is_empty() {
        unsafe {
            let _ = KillTimer(Some(a.hwnd), TIMER_FULLSCREEN);
        }
        return;
    }
    if a.settings.defer_in_fullscreen && crate::fullscreen::active() {
        return;
    }
    unsafe {
        let _ = KillTimer(Some(a.hwnd), TIMER_FULLSCREEN);
    }
    let items = std::mem::take(&mut a.pending);
    let (title, text, kind) = combine(&items);
    log::info("I021", format!("フルスクリーン終了。保留中の通知を表示 ({} 件)", items.len()));
    show_balloon(&title, &text, kind);
    app().update_balloon = items.iter().any(|p| p.is_update);
}

fn show_balloon(title: &str, text: &str, kind: i32) {
    log::info("I022", format!("通知: {title} / {}", text.replace("\r\n", " ").replace('\n', " ")));
    let mut nid = tray_data(1);
    nid.uFlags = NIF_INFO;
    copy_wstr(&mut nid.szInfoTitle, title);
    copy_wstr(&mut nid.szInfo, text);
    nid.dwInfoFlags = match kind {
        1 => NIIF_INFO,
        2 => NIIF_WARNING,
        3 => NIIF_ERROR,
        _ => NIIF_NONE,
    };
    if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() } {
        log::warn("W016", format!("通知を表示できませんでした: {}", windows::core::Error::from_thread()));
    }
}

/// 置換文字の値を作る (履歴・放電の記録を読む)
fn placeholder_context(b: &Battery, threshold: i32) -> crate::placeholders::Context {
    let key = b.device_name.to_ascii_lowercase();
    let history_path = crate::history::path();
    let samples = crate::history::load_all(&history_path).remove(&key).unwrap_or_default();
    let runs = crate::health::load_all(&crate::health::path_beside(&history_path)).remove(&key).unwrap_or_default();
    let hours = crate::stats::load(&crate::stats::path_for(&key));
    crate::placeholders::Context::build(b, Some(threshold), &samples, &runs, &hours, crate::history::now())
}

fn check_notifications() {
    let list = batteries();
    let a = app();

    for b in list.iter().filter(|b| b.connected) {
        // 満充電通知
        if b.charging && (b.fully_charged || (b.percentage >= 100 && !b.voltage_estimate)) {
            if a.settings.notify_full_charge && a.full_notified.insert(b.device_name.clone()) {
                let done = crate::i18n::t().full_charge_done;
                notify(b.label(), done, 1);
                webhook::send_if_enabled(&a.settings, webhook::Message { title: b.label().into(), text: done.into(), device: b.device_name.clone(), battery: b.percentage, event: "full_charge" });
            }
        } else if !b.charging {
            a.full_notified.remove(&b.device_name);
        }

        let Some(rules) = a.settings.rules(&b.device_name) else {
            log::debug("D007", format!("通知ルールなし: {} ({})", b.label(), b.device_name));
            continue;
        };
        let mut rules = rules.clone();
        rules.sort_by_key(|r| -r.battery);

        for rule in rules.iter().filter(|r| r.enabled) {
            let key = (b.device_name.clone(), rule.battery);
            // 充電中は残量低下を通知しない
            if !b.charging && b.percentage <= rule.battery {
                if !a.notified.insert(key) {
                    log::debug("D008", format!("通知済みのため送りません: {} {}% (しきい値 {}%)", b.label(), b.percentage, rule.battery));
                } else {
                    let text = crate::placeholders::expand(&rule.message, &placeholder_context(b, rule.battery));
                    notify(b.label(), &text, rule.balloon_tip_icon);
                    webhook::send_if_enabled(&a.settings, webhook::Message { title: b.label().into(), text, device: b.device_name.clone(), battery: b.percentage, event: "low_battery" });
                }
            } else if b.percentage > rule.battery {
                a.notified.remove(&key);
            }
        }
    }
}

fn open_settings() {
    open_settings_tab(None);
}

/// 設定画面は別プロセス。閉じればメモリはすべて解放される
fn open_settings_tab(tab: Option<&str>) {
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--settings");
        if let Some(tab) = tab {
            cmd.args(["--tab", tab]);
        }
        let _ = cmd.spawn();
    }
}

fn show_menu() {
    reload_settings_if_changed();
    let list = batteries();

    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        let add = |flags: MENU_ITEM_FLAGS, id: u32, text: &str| {
            let w = HSTRING::from(text);
            let _ = AppendMenuW(menu, flags, id as usize, &w);
        };

        let t = crate::i18n::t();
        add(MF_STRING | MF_DISABLED | MF_GRAYED, 0, t.menu_devices);
        for (i, b) in list.iter().enumerate() {
            let checked = if app().settings.is_tray(&b.device_name) { MF_CHECKED } else { MF_UNCHECKED };
            let src = b.source.map(|s| format!("  [{}]", s.label())).unwrap_or_default();
            add(MF_STRING | checked, ID_DEVICE + i as u32, &format!("{}\t{}{}", b.label(), b.status(), src));
        }
        if list.is_empty() {
            add(MF_STRING | MF_DISABLED | MF_GRAYED, 0, &format!("    {}", t.no_devices));
        }
        add(MF_STRING | if app().settings.tray_display_devices.is_empty() { MF_CHECKED } else { MF_UNCHECKED }, ID_NONE, t.menu_hide);
        add(MF_SEPARATOR, 0, "");
        add(MF_STRING | MF_DEFAULT, ID_SETTINGS, t.menu_settings);
        if let Some(r) = updater::State::load().available() {
            add(MF_STRING, ID_UPDATE, &crate::i18n::f(t.menu_update_fmt, &[&r.tag]));
        }
        add(MF_STRING, ID_REFRESH, t.menu_refresh);
        add(MF_STRING | if startup::enabled() { MF_CHECKED } else { MF_UNCHECKED }, ID_STARTUP, t.start_with_windows);
        let no_url = app().settings.webhook_url.trim().is_empty();
        let webhook_flags = if no_url { MF_DISABLED | MF_GRAYED } else { MENU_ITEM_FLAGS(0) };
        let webhook_check = if app().settings.webhook_enabled { MF_CHECKED } else { MF_UNCHECKED };
        add(MF_STRING | webhook_flags | webhook_check, ID_WEBHOOK, if no_url { t.menu_webhook_no_url } else { t.menu_webhook });

        let theme_menu = CreatePopupMenu().unwrap_or_default();
        for (i, t) in [Theme::System, Theme::Light, Theme::Dark].iter().enumerate() {
            let flags = if *t == app().settings.theme { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(theme_menu, MF_STRING | flags, (ID_THEME + i as u32) as usize, &HSTRING::from(t.label()));
        }
        let _ = AppendMenuW(menu, MF_POPUP, theme_menu.0 as usize, &HSTRING::from(crate::i18n::f(t.theme_fmt, &[&app().settings.theme.label()])));

        add(MF_SEPARATOR, 0, "");
        add(MF_STRING, ID_EXIT, t.menu_exit);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(app().hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, None, app().hwnd, None).0 as u32;
        let _ = DestroyMenu(menu);

        match cmd {
            0 => {}
            ID_SETTINGS => open_settings(),
            ID_UPDATE => open_settings_tab(Some("app")),
            ID_REFRESH => app().refresh.store(true, Ordering::Relaxed),
            ID_STARTUP => startup::set(!startup::enabled()),
            ID_EXIT => {
                log::info("I023", "メニューから終了が選ばれました");
                PostQuitMessage(0)
            }
            ID_WEBHOOK => {
                update_settings(|s| s.webhook_enabled = !s.webhook_enabled);
                log::info("I024", format!("Webhook 通知: {}", if app().settings.webhook_enabled { "オン" } else { "オフ" }));
            }
            id if (ID_THEME..ID_THEME + 3).contains(&id) => {
                update_settings(|s| s.theme = [Theme::System, Theme::Light, Theme::Dark][(id - ID_THEME) as usize]);
                log::info("I025", format!("テーマ: {}", app().settings.theme.label()));
                apply_menu_theme();
            }
            ID_NONE => {
                update_settings(|s| s.tray_display_devices.clear());
                update_icon();
            }
            id if id >= ID_DEVICE => {
                if let Some(b) = list.get((id - ID_DEVICE) as usize) {
                    toggle_tray_device(&b.device_name);
                }
            }
            _ => {}
        }
    }
}

/// メニューで選んだデバイスのアイコンを出す / 消す
fn toggle_tray_device(name: &str) {
    update_settings(|s| {
        let show = !s.is_tray(name);
        s.set_tray(name, show);
    });
    log::info("I026", format!("アイコン表示: {name} を{}", if app().settings.is_tray(name) { "表示" } else { "非表示" }));
    update_icon();
}

/// 最新の設定ファイルに変更を加えて保存する
fn update_settings(change: impl FnOnce(&mut Settings)) {
    let a = app();
    a.settings = Settings::load();
    change(&mut a.settings);
    if let Err(e) = a.settings.save() {
        log::error("E008", format!("設定の保存に失敗しました: {e}"));
    }
    a.settings_time = settings::modified();
}

/// 右クリックメニューをライト/ダークに合わせる (uxtheme の非公開 API。失敗しても既定の見た目のまま)
fn apply_menu_theme() {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    unsafe {
        let Ok(uxtheme) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        // 135: SetPreferredAppMode (1=システムに従う 2=ダーク 3=ライト), 136: FlushMenuThemes
        if let Some(set_mode) = GetProcAddress(uxtheme, PCSTR(135 as *const u8)) {
            let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
            set_mode(match app().settings.theme {
                Theme::System => 1,
                Theme::Dark => 2,
                Theme::Light => 3,
            });
        }
        if let Some(flush) = GetProcAddress(uxtheme, PCSTR(136 as *const u8)) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        #[allow(static_mut_refs)]
        if APP.is_none() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        match msg {
            WM_APP_UPDATE => {
                reload_settings_if_changed();
                update_icon();
                check_notifications();
                LRESULT(0)
            }
            WM_APP_RELOAD => {
                reload_settings_if_changed();
                apply_menu_theme();
                update_icon();
                LRESULT(0)
            }
            WM_APP_TRAY => {
                match lparam.0 as u32 {
                    WM_LBUTTONUP => open_settings(),
                    WM_RBUTTONUP => show_menu(),
                    NIN_BALLOONUSERCLICK if app().update_balloon => open_settings_tab(Some("app")),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_FULLSCREEN => {
                flush_pending();
                LRESULT(0)
            }
            WM_APP_QUIT => {
                log::info("I027", "更新のため終了します");
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_APP_UPDATE_FOUND => {
                if let Some(r) = updater::State::load().available() {
                    let t = crate::i18n::t();
                    notify_ex(t.update_balloon_title, &crate::i18n::f(t.update_balloon_fmt, &[&r.tag]), 1, true);
                }
                LRESULT(0)
            }
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
                update_icon();
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            m if m == app().taskbar_created => {
                // エクスプローラー再起動時にアイコンを登録し直す
                for icon in app().icons.drain(..) {
                    icon::destroy(icon);
                }
                update_icon();
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
