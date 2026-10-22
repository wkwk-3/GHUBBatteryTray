//! 常駐から設定画面へ、最新の残量をすぐ送る (WM_COPYDATA)。
//!
//! 設定画面は受け取り用の見えないウィンドウ (メッセージ専用) を作って待つ。
//! 常駐は残量を取得するたびに、そのウィンドウがあれば一覧 (JSON) を送る。無ければ何もしない。
//! 常駐が動いていない (届かない) 間は、設定画面に「自分で読み直す」を 60 秒ごとに知らせる。

use crate::ghub::Battery;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

const CLASS: PCWSTR = w!("GHUBBatteryLiveWnd");
/// WM_COPYDATA の種類 ("GBLV")
const TAG: usize = 0x4742_4C56;
/// 常駐から届かないとき、自分で読み直す間隔
const STALE_SECS: u64 = 60;
const TIMER_STALE: usize = 1;

/// 設定画面が受け取るもの
pub enum Live {
    /// 常駐が取得した最新の一覧
    Update(Vec<Battery>),
    /// 常駐から届かない。自分で読み直す
    Stale,
}

/// 設定フォルダーごとに別のウィンドウ名 (テスト用の別フォルダーの常駐とは混ざらない)
fn title() -> HSTRING {
    HSTRING::from(format!("GHUB Battery live {}", crate::settings::instance_id()))
}

/// 常駐側: 開いている設定画面に一覧を送る (1 秒以内に受け取れなければあきらめる)
pub fn send(list: &[Battery]) {
    let Ok(hwnd) = (unsafe { FindWindowExW(Some(HWND_MESSAGE), None, CLASS, &title()) }) else { return };
    if hwnd.is_invalid() {
        return;
    }
    let Ok(json) = serde_json::to_vec(list) else { return };
    let data = COPYDATASTRUCT { dwData: TAG, cbData: json.len() as u32, lpData: json.as_ptr() as *mut _ };
    let ok = unsafe { SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&data as *const _ as isize), SMTO_ABORTIFHUNG | SMTO_BLOCK, 1000, None) };
    if ok.0 == 0 {
        crate::log::debug("D015", "設定画面に最新の残量を送れませんでした");
    }
}

static SINK: OnceLock<Box<dyn Fn(Live) + Send + Sync>> = OnceLock::new();
/// 最後に常駐から受け取った時刻 (unix 秒)
static LAST: AtomicU64 = AtomicU64::new(0);

/// 設定画面側: 受け取り用のウィンドウを別スレッドで作って待つ
pub fn receive(on: impl Fn(Live) + Send + Sync + 'static) {
    if SINK.set(Box::new(on)).is_err() {
        return;
    }
    LAST.store(crate::history::now(), Ordering::Relaxed);
    let spawned = std::thread::Builder::new().name("live".into()).spawn(|| unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: CLASS, ..Default::default() };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, &title(), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance.into()), None) else {
            crate::log::warn("W018", "設定画面: 最新の残量を受け取るウィンドウを作れません。60 秒ごとに読み直します");
            return;
        };
        SetTimer(Some(hwnd), TIMER_STALE, 10_000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    });
    if let Err(e) = spawned {
        crate::log::error("E017", format!("スレッド live を開始できませんでした: {e}"));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => {
            let data = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
            if data.dwData != TAG || data.lpData.is_null() {
                return LRESULT(0);
            }
            let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize) };
            match serde_json::from_slice::<Vec<Battery>>(bytes) {
                Ok(list) => {
                    LAST.store(crate::history::now(), Ordering::Relaxed);
                    if let Some(sink) = SINK.get() {
                        sink(Live::Update(list));
                    }
                    LRESULT(1)
                }
                Err(e) => {
                    crate::log::debug("D016", format!("設定画面: 受け取った残量を読めません: {e}"));
                    LRESULT(0)
                }
            }
        }
        WM_TIMER => {
            // 常駐から 60 秒届いていなければ、自分で読み直す
            let now = crate::history::now();
            if now >= LAST.load(Ordering::Relaxed) + STALE_SECS {
                LAST.store(now, Ordering::Relaxed);
                if let Some(sink) = SINK.get() {
                    sink(Live::Stale);
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
