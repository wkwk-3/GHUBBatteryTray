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
    (format!("待っていた通知 ({} 件)", items.len()), text, kind)
}

static mut APP: Option<App> = None;

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

        log::info(format!("アプリを開始しました (バージョン {})", env!("CARGO_PKG_VERSION")));
        updater::cleanup_old();
        if std::env::args().any(|a| a == "--after-update") {
            notify("GHUB Battery", &format!("v{} に更新しました", updater::CURRENT), 1);
        }

        let hwnd_raw = hwnd.0 as isize;
        std::thread::spawn(move || worker(hwnd_raw, shared, refresh));
        std::thread::spawn(move || update_checker(hwnd_raw));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        for uid in 1..=app().icons.len() as u32 {
            remove_tray_icon(uid);
        }
        log::info("アプリを終了しました");
    }
}

/// 前回と比べてデバイスの状態が変わったらログに残す
fn log_changes(prev: &[Battery], list: &[Battery]) {
    let src = |b: &Battery| b.source.map(|s| s.label()).unwrap_or("-");
    for b in list {
        match prev.iter().find(|p| p.device_name == b.device_name) {
            None => log::info(format!("デバイスを検出: {} {}% (取得元: {})", b.label(), b.percentage, src(b))),
            Some(p) => {
                if p.source != b.source {
                    log::info(format!("取得元が変わりました: {} {} → {}", b.label(), src(p), src(b)));
                }
                if p.charging != b.charging && b.connected {
                    log::info(format!("{}: {} ({}%)", b.label(), if b.charging { "充電を開始" } else { "充電を終了" }, b.percentage));
                }
                if p.asleep != b.asleep {
                    log::info(format!("{}: {}", b.label(), if b.asleep { "スリープ中 (前回の値を表示)" } else { "スリープから復帰" }));
                }
                if p.connected != b.connected {
                    log::info(format!("{}: {}", b.label(), if b.connected { "接続" } else { "未接続" }));
                }
            }
        }
    }
    for p in prev.iter().filter(|p| !list.iter().any(|b| b.device_name == p.device_name)) {
        log::info(format!("デバイスが見つからなくなりました: {}", p.label()));
    }
    if list.is_empty() && !prev.is_empty() {
        log::warn("バッテリー情報を取得できませんでした (直接通信・G HUB API・settings.db すべて)");
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
                    Err(e) => log::warn(format!("更新の確認に失敗しました: {e}")),
                }
            }
        }
        first = false;
        std::thread::sleep(Duration::from_secs(30 * 60));
    }
}

/// 取得スレッド
fn worker(hwnd: isize, shared: Arc<Mutex<Shared>>, refresh: Arc<AtomicBool>) {
    let mut reader = hidpp::Reader::new();
    let mut recorder = crate::history::Recorder::default();
    let mut prev: Vec<Battery> = Vec::new();
    loop {
        let list = sources::collect(&mut reader);
        recorder.record(&list);
        log_changes(&prev, &list);
        prev = list.clone();
        if let Ok(mut s) = shared.lock() {
            s.batteries = list;
            s.loaded = true;
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_UPDATE, WPARAM(0), LPARAM(0));
        }

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
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
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
        log::info("設定を読み込み直しました");
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
        let tip = match battery {
            Some(b) => format!("{}\n{}", b.label(), b.status()),
            None if !loaded() => "GHUB Battery\n取得中...".into(),
            None if tray.is_empty() && list.is_empty() => "GHUB Battery\nバッテリー情報を取得できません".into(),
            None if tray.is_empty() => "GHUB Battery\n右クリックで表示するデバイスを選択".into(),
            None => format!("{tray}\nデバイスが見つかりません"),
        };

        let new_icon = icon::create(battery, icon_size(), &a.settings.level_colors);
        let mut nid = tray_data(i as u32 + 1);
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = new_icon;
        copy_wstr(&mut nid.szTip, &tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
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
        log::info(format!("フルスクリーン中のため通知を保留: {title} / {}", text.replace("\r\n", " ")));
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
    log::info(format!("フルスクリーンが終わったので、保留していた通知を表示します ({} 件)", items.len()));
    show_balloon(&title, &text, kind);
    app().update_balloon = items.iter().any(|p| p.is_update);
}

fn show_balloon(title: &str, text: &str, kind: i32) {
    log::info(format!("通知: {title} / {}", text.replace("\r\n", " ").replace('\n', " ")));
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
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// 置換文字の値を作る (履歴・放電の記録を読む)
fn placeholder_context(b: &Battery, threshold: i32) -> crate::placeholders::Context {
    let key = b.device_name.to_ascii_lowercase();
    let history_path = crate::history::path();
    let samples = crate::history::load_all(&history_path).remove(&key).unwrap_or_default();
    let runs = crate::health::load_all(&crate::health::path_beside(&history_path)).remove(&key).unwrap_or_default();
    crate::placeholders::Context::build(b, Some(threshold), &samples, &runs, crate::history::now())
}

fn check_notifications() {
    let list = batteries();
    let a = app();

    for b in list.iter().filter(|b| b.connected) {
        // 満充電通知
        if b.charging && (b.fully_charged || b.percentage >= 100) {
            if a.settings.notify_full_charge && a.full_notified.insert(b.device_name.clone()) {
                notify(b.label(), "充電が完了しました", 1);
                webhook::send_if_enabled(&a.settings, webhook::Message { title: b.label().into(), text: "充電が完了しました".into(), device: b.device_name.clone(), battery: b.percentage, event: "full_charge" });
            }
        } else if !b.charging {
            a.full_notified.remove(&b.device_name);
        }

        let Some(rules) = a.settings.rules(&b.device_name) else { continue };
        let mut rules = rules.clone();
        rules.sort_by_key(|r| -r.battery);

        for rule in rules.iter().filter(|r| r.enabled) {
            let key = (b.device_name.clone(), rule.battery);
            // 充電中は残量低下を通知しない
            if !b.charging && b.percentage <= rule.battery {
                if a.notified.insert(key) {
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

        add(MF_STRING | MF_DISABLED | MF_GRAYED, 0, "トレイに表示するデバイス (複数選択可)");
        for (i, b) in list.iter().enumerate() {
            let checked = if app().settings.is_tray(&b.device_name) { MF_CHECKED } else { MF_UNCHECKED };
            let src = b.source.map(|s| format!("  [{}]", s.label())).unwrap_or_default();
            add(MF_STRING | checked, ID_DEVICE + i as u32, &format!("{}\t{}{}", b.label(), b.status(), src));
        }
        if list.is_empty() {
            add(MF_STRING | MF_DISABLED | MF_GRAYED, 0, "    デバイスが見つかりません");
        }
        add(MF_STRING | if app().settings.tray_display_devices.is_empty() { MF_CHECKED } else { MF_UNCHECKED }, ID_NONE, "表示しない");
        add(MF_SEPARATOR, 0, "");
        add(MF_STRING | MF_DEFAULT, ID_SETTINGS, "設定を開く...");
        if let Some(r) = updater::State::load().available() {
            add(MF_STRING, ID_UPDATE, &format!("更新があります ({})...", r.tag));
        }
        add(MF_STRING, ID_REFRESH, "残量を今すぐ再取得");
        add(MF_STRING | if startup::enabled() { MF_CHECKED } else { MF_UNCHECKED }, ID_STARTUP, "Windows 起動時に開始");
        let no_url = app().settings.webhook_url.trim().is_empty();
        let webhook_flags = if no_url { MF_DISABLED | MF_GRAYED } else { MENU_ITEM_FLAGS(0) };
        let webhook_check = if app().settings.webhook_enabled { MF_CHECKED } else { MF_UNCHECKED };
        add(MF_STRING | webhook_flags | webhook_check, ID_WEBHOOK, if no_url { "Webhook 通知 (設定画面で URL を入力)" } else { "Webhook 通知" });

        let theme_menu = CreatePopupMenu().unwrap_or_default();
        for (i, t) in [Theme::System, Theme::Light, Theme::Dark].iter().enumerate() {
            let flags = if *t == app().settings.theme { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(theme_menu, MF_STRING | flags, (ID_THEME + i as u32) as usize, &HSTRING::from(t.label()));
        }
        let _ = AppendMenuW(menu, MF_POPUP, theme_menu.0 as usize, &HSTRING::from(format!("テーマ: {}", app().settings.theme.label())));

        add(MF_SEPARATOR, 0, "");
        add(MF_STRING, ID_EXIT, "終了");

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
                log::info("メニューから終了が選ばれました");
                PostQuitMessage(0)
            }
            ID_WEBHOOK => {
                update_settings(|s| s.webhook_enabled = !s.webhook_enabled);
                log::info(format!("Webhook 通知: {}", if app().settings.webhook_enabled { "オン" } else { "オフ" }));
            }
            id if (ID_THEME..ID_THEME + 3).contains(&id) => {
                update_settings(|s| s.theme = [Theme::System, Theme::Light, Theme::Dark][(id - ID_THEME) as usize]);
                log::info(format!("テーマ: {}", app().settings.theme.label()));
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
    log::info(format!("アイコン表示: {name} を{}", if app().settings.is_tray(name) { "表示" } else { "非表示" }));
    update_icon();
}

/// 最新の設定ファイルに変更を加えて保存する
fn update_settings(change: impl FnOnce(&mut Settings)) {
    let a = app();
    a.settings = Settings::load();
    change(&mut a.settings);
    let _ = a.settings.save();
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
                log::info("更新のため終了します");
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_APP_UPDATE_FOUND => {
                if let Some(r) = updater::State::load().available() {
                    notify_ex("GHUB Battery の更新があります", &format!("{} が出ています。クリックすると設定から更新できます", r.tag), 1, true);
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
