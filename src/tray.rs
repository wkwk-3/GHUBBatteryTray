//! 常駐プロセス。Win32 の隠しウィンドウ + 通知領域アイコンだけで動き、
//! バッテリーの取得は別スレッドで 60 秒ごと (直接通信 > G HUB API > settings.db)。

use crate::ghub::Battery;
use crate::settings::{self, Settings, Theme};
use crate::{hidpp, icon, sources, webhook};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

pub const WINDOW_CLASS: PCWSTR = w!("GHUBBatteryTrayWnd");
const WM_APP_UPDATE: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
/// 設定画面が保存したときに送ってくる
pub const WM_APP_RELOAD: u32 = WM_APP + 3;

const ID_NONE: u32 = 999;
const ID_DEVICE: u32 = 1000;
const ID_SETTINGS: u32 = 1;
const ID_REFRESH: u32 = 2;
const ID_STARTUP: u32 = 3;
const ID_EXIT: u32 = 4;
const ID_WEBHOOK: u32 = 5;
const ID_THEME: u32 = 10;
const POLL_INTERVAL: Duration = Duration::from_secs(60);

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("GHUBBatteryTray");

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

        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), WINDOW_CLASS, w!("GHUB Battery"), WINDOW_STYLE(0), 0, 0, 0, 0, None, None, Some(instance.into()), None)
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
        });

        apply_menu_theme();
        update_icon();

        let hwnd_raw = hwnd.0 as isize;
        std::thread::spawn(move || worker(hwnd_raw, shared, refresh));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        for uid in 1..=app().icons.len() as u32 {
            remove_tray_icon(uid);
        }
    }
}

/// 取得スレッド
fn worker(hwnd: isize, shared: Arc<Mutex<Shared>>, refresh: Arc<AtomicBool>) {
    let mut reader = hidpp::Reader::new();
    let mut recorder = crate::history::Recorder::default();
    loop {
        let list = sources::collect(&mut reader);
        recorder.record(&list);
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

        let new_icon = icon::create(battery, icon_size());
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
    settings::log(&format!("[INFO] 通知: {title} / {text}"));
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
                    let text = rule.message.replace("{name}", b.label()).replace("{battery}", &rule.battery.to_string()).replace("{rn}", "\r\n");
                    notify(b.label(), &text, rule.balloon_tip_icon);
                    webhook::send_if_enabled(&a.settings, webhook::Message { title: b.label().into(), text, device: b.device_name.clone(), battery: b.percentage, event: "low_battery" });
                }
            } else if b.percentage > rule.battery {
                a.notified.remove(&key);
            }
        }
    }
}

fn startup_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

fn set_startup(enable: bool) {
    unsafe {
        if enable {
            let exe = std::env::current_exe().unwrap_or_default();
            let value: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ.0, Some(value.as_ptr() as _), (value.len() * 2) as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

fn open_settings() {
    // 設定画面は別プロセス。閉じればメモリはすべて解放される
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--settings").spawn();
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
        add(MF_STRING, ID_REFRESH, "残量を今すぐ再取得");
        add(MF_STRING | if startup_enabled() { MF_CHECKED } else { MF_UNCHECKED }, ID_STARTUP, "Windows 起動時に開始");
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
            ID_REFRESH => app().refresh.store(true, Ordering::Relaxed),
            ID_STARTUP => set_startup(!startup_enabled()),
            ID_EXIT => PostQuitMessage(0),
            ID_WEBHOOK => update_settings(|s| s.webhook_enabled = !s.webhook_enabled),
            id if (ID_THEME..ID_THEME + 3).contains(&id) => {
                update_settings(|s| s.theme = [Theme::System, Theme::Light, Theme::Dark][(id - ID_THEME) as usize]);
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
                    _ => {}
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
