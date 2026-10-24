//! G HUB なしでマウス等と直接通信する (Logitech HID++ 2.0)。
//! 受信機 (LIGHTSPEED / Unifying / Bolt) 経由でも有線/Bluetooth 直結でも、
//! ベンダー定義の HID コレクション (usage page 0xFF00, usage 0x0002 = long report) に
//! 20 バイトの long report を書いて応答を読む。

use crate::ghub::{Battery, Source};
use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};

const VENDOR_LOGITECH: u16 = 0x046D;
const REPORT_LONG: u8 = 0x11;
const SOFTWARE_ID: u8 = 0x0B;
const TIMEOUT: Duration = Duration::from_millis(400);
/// 未使用スロットの探索は短く切り上げる
const PING_TIMEOUT: Duration = Duration::from_millis(120);
/// 見つけたデバイスだけを読む期間。過ぎたら全スロットを探し直す
const RESCAN_INTERVAL: Duration = Duration::from_secs(600);

const FEATURE_DEVICE_INFO: u16 = 0x0003;
const FEATURE_DEVICE_NAME: u16 = 0x0005;
const FEATURE_BATTERY_STATUS: u16 = 0x1000;
const FEATURE_BATTERY_VOLTAGE: u16 = 0x1001;
const FEATURE_UNIFIED_BATTERY: u16 = 0x1004;

struct Channel<'a> {
    dev: &'a HidDevice,
    index: u8,
}

impl Channel<'_> {
    /// HID++ 2.0 リクエスト。応答のパラメータ (16 バイト) を返す。エラー/タイムアウトは None
    fn call(&self, feature_index: u8, function: u8, params: &[u8]) -> Option<[u8; 16]> {
        self.call_with(feature_index, function, params, TIMEOUT)
    }

    fn call_with(&self, feature_index: u8, function: u8, params: &[u8], timeout: Duration) -> Option<[u8; 16]> {
        let mut req = [0u8; 20];
        req[0] = REPORT_LONG;
        req[1] = self.index;
        req[2] = feature_index;
        req[3] = (function << 4) | SOFTWARE_ID;
        req[4..4 + params.len()].copy_from_slice(params);
        self.dev.write(&req).ok()?;

        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 20];
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let n = self.dev.read_timeout(&mut buf, left.as_millis().max(1) as i32).ok()?;
            if n < 7 || buf[1] != self.index {
                continue;
            }
            // HID++ 2.0 エラー (0xFF) / HID++ 1.0 エラー (0x8F)
            if (buf[2] == 0xFF || buf[2] == 0x8F) && buf[3] == feature_index && buf[4] == req[3] {
                return None;
            }
            if buf[2] == feature_index && buf[3] == req[3] {
                let mut out = [0u8; 16];
                out[..n - 4].copy_from_slice(&buf[4..n]);
                return Some(out);
            }
        }
        None
    }

    /// Root (index 0) の getFeature で機能番号を探す
    fn feature(&self, id: u16) -> Option<u8> {
        let r = self.call(0x00, 0, &id.to_be_bytes())?;
        (r[0] != 0).then_some(r[0])
    }

    fn ping(&self) -> bool {
        self.call_with(0x00, 1, &[0, 0, 0x5A], PING_TIMEOUT).is_some_and(|r| r[2] == 0x5A)
    }

    fn name(&self) -> Option<String> {
        let f = self.feature(FEATURE_DEVICE_NAME)?;
        let len = self.call(f, 0, &[])?[0] as usize;
        let mut bytes = Vec::with_capacity(len);
        while bytes.len() < len {
            let chunk = self.call(f, 1, &[bytes.len() as u8])?;
            let take = (len - bytes.len()).min(16);
            bytes.extend_from_slice(&chunk[..take]);
        }
        Some(String::from_utf8_lossy(&bytes).trim_end_matches('\0').trim().to_string())
    }

    /// デバイス情報 (0x0003): (unitId を 10 進数にしたもの, 製品 ID の一覧)
    /// 応答 = [機能の数, unitId (4 バイト), 接続の種類 (2 バイト), 製品 ID (2 バイト × 3), …]
    fn device_info(&self) -> Option<(String, Vec<u16>)> {
        let f = self.feature(FEATURE_DEVICE_INFO)?;
        let r = self.call(f, 0, &[])?;
        let unit = u32::from_be_bytes([r[1], r[2], r[3], r[4]]);
        let pids = (0..3).map(|i| u16::from_be_bytes([r[7 + i * 2], r[8 + i * 2]])).filter(|p| *p != 0).collect();
        (unit != 0).then(|| (unit.to_string(), pids))
    }

    /// 対応しているバッテリー機能を探す
    fn battery_feature(&self) -> Option<(u16, u8)> {
        [FEATURE_UNIFIED_BATTERY, FEATURE_BATTERY_STATUS, FEATURE_BATTERY_VOLTAGE].into_iter().find_map(|id| self.feature(id).map(|f| (id, f)))
    }

    /// (残量%, 充電中, 満充電)
    fn battery(&self, (id, f): (u16, u8)) -> Option<(i32, bool, bool)> {
        match id {
            FEATURE_UNIFIED_BATTERY => {
                // get_status: [残量%, レベル, 充電状態, 外部電源]
                let r = self.call(f, 1, &[])?;
                Some((r[0] as i32, matches!(r[2], 1 | 2 | 3), r[2] == 3))
            }
            FEATURE_BATTERY_STATUS => {
                // getBatteryLevelStatus: [残量%, 次のレベル, 状態]
                let r = self.call(f, 0, &[])?;
                Some((r[0] as i32, matches!(r[2], 1..=4), r[2] == 3))
            }
            _ => {
                // getBatteryInfo: [電圧 mV (2 バイト), フラグ]
                let r = self.call(f, 0, &[])?;
                let mv = u16::from_be_bytes([r[0], r[1]]) as i32;
                Some((voltage_to_percent(mv), r[2] & 0x80 != 0, false))
            }
        }
    }
}

/// リチウムイオン電池の電圧から残量を推定 (Solaar と同じ近似表)
fn voltage_to_percent(mv: i32) -> i32 {
    const TABLE: [(i32, i32); 13] = [
        (4186, 100), (4067, 90), (3989, 80), (3922, 70), (3859, 60), (3811, 50), (3778, 40),
        (3751, 30), (3717, 20), (3671, 10), (3646, 5), (3579, 2), (3500, 0),
    ];
    if mv >= TABLE[0].0 {
        return 100;
    }
    for w in TABLE.windows(2) {
        let ((v1, p1), (v2, p2)) = (w[0], w[1]);
        if mv >= v2 {
            return p2 + (p1 - p2) * (mv - v2) / (v1 - v2);
        }
    }
    0
}

/// 設定キー用に名前を正規化 ("PRO 2 LIGHTSPEED" -> "pro2lightspeed")
pub fn normalize(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// 見つけたデバイス (次回からはこれだけ読む)
struct Known {
    path: std::ffi::CString,
    index: u8,
    name: String,
    feature: (u16, u8),
    /// デバイス情報 (unitId, 製品 ID)。G HUB との突き合わせ用
    info: Option<(String, Vec<u16>)>,
    /// 最後に読めた値と時刻 (スリープ中はこれを表示する)
    last: Option<(Battery, Instant)>,
}

/// スリープ中に前回の値を出し続ける上限
const KEEP_ASLEEP: Duration = Duration::from_secs(6 * 3600);

pub struct Reader {
    api: Option<HidApi>,
    known: Vec<Known>,
    last_scan: Option<Instant>,
}

impl Reader {
    pub fn new() -> Self {
        #[allow(deprecated)]
        let api = match HidApi::new_without_enumerate() {
            Ok(api) => Some(api),
            Err(e) => {
                crate::log::error("E003", format!("HID の初期化に失敗しました。直接通信は使えません: {e}"));
                None
            }
        };
        Reader { api, known: Vec::new(), last_scan: None }
    }

    /// 接続中の Logitech デバイスのバッテリーを読む
    pub fn read_all(&mut self) -> Vec<Battery> {
        let rescan = self.known.is_empty() || self.known.iter().any(|k| k.last.is_none()) || self.last_scan.is_none_or(|t| t.elapsed() > RESCAN_INTERVAL);
        if rescan {
            self.scan();
        }

        let Some(api) = self.api.as_ref() else { return Vec::new() };
        let mut list = Vec::new();
        let mut lost = false;
        for k in &mut self.known {
            let reading = match api.open_path(&k.path) {
                Ok(dev) => Channel { dev: &dev, index: k.index }.battery(k.feature),
                Err(e) => {
                    crate::log::debug("D002", format!("HID: {} を開けません: {e}", k.name));
                    None
                }
            };
            if reading.is_none() {
                crate::log::debug("D003", format!("HID: {} から応答がありません", k.name));
            }
            match reading {
                Some((percentage, charging, full)) => {
                    let b = Battery {
                        device_name: normalize(&k.name),
                        display_name: k.name.clone(),
                        percentage: percentage.clamp(0, 100),
                        charging,
                        fully_charged: full,
                        connected: true,
                        remaining_hours: None,
                        source: Some(Source::Hid),
                        asleep: false,
                        unit_id: k.info.as_ref().map(|i| i.0.clone()),
                        pids: k.info.as_ref().map(|i| i.1.clone()).unwrap_or_default(),
                        voltage_estimate: k.feature.0 == FEATURE_BATTERY_VOLTAGE,
                        ..Default::default()
                    };
                    k.last = Some((b.clone(), Instant::now()));
                    list.push(b);
                }
                None => {
                    lost = true;
                    if let Some((b, at)) = &k.last {
                        if at.elapsed() < KEEP_ASLEEP {
                            list.push(Battery { asleep: true, charging: false, ..b.clone() });
                        }
                    }
                }
            }
        }
        // 電源オフ・抜き差しなどで読めなくなったら次回は探し直す
        if lost {
            self.last_scan = None;
        }
        list
    }

    /// 全受信機・全スロットを探す
    fn scan(&mut self) {
        self.last_scan = Some(Instant::now());
        // 前回の値は持ち越す (再スキャンで見つからなくてもスリープ表示に使う)
        let previous: Vec<Known> = std::mem::take(&mut self.known);
        let Some(api) = self.api.as_mut() else { return };
        if let Err(e) = api.reset_devices().and_then(|_| api.add_devices(VENDOR_LOGITECH, 0)) {
            crate::log::problem("hid_scan", Some(("W002", format!("HID デバイスの一覧を取得できません: {e}"))), ("", ""));
            return;
        }
        crate::log::problem("hid_scan", None, ("I004", "HID デバイスの一覧を取得できるようになりました"));

        let paths: Vec<_> = api
            .device_list()
            .filter(|d| d.usage_page() == 0xFF00 && d.usage() == 0x0002)
            .map(|d| d.path().to_owned())
            .collect();

        for path in paths {
            let dev = match api.open_path(&path) {
                Ok(dev) => dev,
                Err(e) => {
                    crate::log::debug("D004", format!("HID: {} を開けません: {e}", path.to_string_lossy()));
                    continue;
                }
            };

            // 直結デバイスは 0xFF、受信機ならペアリング番号 1〜6
            let direct = Channel { dev: &dev, index: 0xFF };
            let indices: Vec<u8> = if direct.ping() && direct.feature(FEATURE_DEVICE_NAME).is_some() { vec![0xFF] } else { (1..=6).collect() };

            for index in indices {
                let ch = Channel { dev: &dev, index };
                if !ch.ping() {
                    continue;
                }
                let Some(feature) = ch.battery_feature() else {
                    crate::log::debug("D005", format!("HID: 番号 {index} はバッテリー機能に対応していません"));
                    continue;
                };
                let name = ch.name().unwrap_or_else(|| format!("Logitech device {index}"));
                if self.known.iter().any(|k| k.name == name) {
                    continue;
                }
                let last = previous.iter().find(|k| k.name == name).and_then(|k| k.last.clone());
                let info = ch.device_info();
                self.known.push(Known { path: path.clone(), index, name, feature, info, last });
            }
        }

        // 見つかった顔ぶれが変わったときだけ書く
        static LAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        let found = if self.known.is_empty() {
            "なし".to_string()
        } else {
            self.known.iter().map(|k| format!("{} (番号 {}, 機能 0x{:04X})", k.name, k.index, k.feature.0)).collect::<Vec<_>>().join(", ")
        };
        if let Ok(mut last) = LAST.lock() {
            if last.as_deref() != Some(found.as_str()) {
                crate::log::info("I005", format!("HID 直接通信で見つかったデバイス: {found}"));
                *last = Some(found);
            }
        }
    }
}

// ───────────── 通知の待ち受け ─────────────
//
// 受信機・直結デバイスのベンダー定義コレクションを開いたままにして、デバイスから届く通知を待つ。
// - 統合バッテリー (0x1004) / バッテリー状態 (0x1000) の変化通知 (残量・充電の開始と終了)
// - 受信機の接続通知 (HID++ 1.0 の 0x41: 接続・切断・スリープからの復帰)
// - しばらく通信がなかったデバイスからの通信 (スリープからの復帰とみなす)
// を受けたら on_change を呼ぶ (取得スレッドに「今すぐ読み直す」を知らせる)。
// 受信機を抜いたときは読み込みエラーで気付いて閉じ、挿し直されるまで数秒ごとに探し直す。
// Windows では同じ HID を複数開くと、届いたデータはそれぞれに配られるので、取得スレッドの読み取りとは干渉しない。

/// 通知の種類
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    /// 残量・充電状態の変化 (デバイス番号, 残量%, 充電中, 満充電)
    Battery(u8, i32, bool, bool),
    /// 接続・切断 (受信機の 0x41 通知)。(デバイス番号, 接続中)
    Link(u8, bool),
}

/// 受け取ったデータを通知として読む。battery = デバイス番号ごとの (バッテリー機能の ID, 機能番号)
pub fn parse_notice(buf: &[u8], battery: &[(u8, u16, u8)]) -> Option<Notice> {
    if buf.len() < 7 {
        return None;
    }
    let index = buf[1];
    // HID++ 1.0: 受信機のデバイス接続通知 (短い報告 0x10 / 長い報告 0x11)。params[0] の bit6 = 切断
    if buf[2] == 0x41 && (buf[0] == 0x10 || buf[0] == REPORT_LONG) {
        return Some(Notice::Link(index, buf[4] & 0x40 == 0));
    }
    // HID++ 2.0 のブロードキャスト: function 0 / software id 0
    if buf[0] != REPORT_LONG || buf[3] != 0x00 {
        return None;
    }
    let (_, id, _) = battery.iter().find(|(i, _, f)| *i == index && *f == buf[2])?;
    match *id {
        FEATURE_UNIFIED_BATTERY => Some(Notice::Battery(index, buf[4] as i32, matches!(buf[6], 1 | 2 | 3), buf[6] == 3)),
        FEATURE_BATTERY_STATUS => Some(Notice::Battery(index, buf[4] as i32, matches!(buf[6], 1..=4), buf[6] == 3)),
        _ => None,
    }
}

/// これより長く通信がなかったデバイスから届いたら、スリープからの復帰とみなす
const WAKE_GAP: Duration = Duration::from_secs(120);
/// 受信機が見つからないときに探し直す間隔
const RETRY: Duration = Duration::from_secs(3);
/// 眠っていたデバイスが起きたときなどに探し直す最短の間隔
const MIN_REDISCOVER: Duration = Duration::from_secs(30);
/// 新しい受信機・眠っていたデバイスのために探し直す間隔
const REDISCOVER: Duration = Duration::from_secs(300);

struct Opened {
    dev: HidDevice,
    /// (デバイス番号, バッテリー機能の ID, 機能番号)
    battery: Vec<(u8, u16, u8)>,
    long: bool,
}

/// 開けるコレクションをすべて開き、デバイスごとのバッテリー機能の番号を調べる
fn discover(api: &mut HidApi) -> Vec<Opened> {
    if let Err(e) = api.reset_devices().and_then(|_| api.add_devices(VENDOR_LOGITECH, 0)) {
        crate::log::problem("listen_scan", Some(("W017", format!("通知の待ち受け: HID デバイスの一覧を取得できません: {e}"))), ("", ""));
        return Vec::new();
    }
    crate::log::problem("listen_scan", None, ("I044", "通知の待ち受け: HID デバイスの一覧を取得できるようになりました"));
    let paths: Vec<_> = api.device_list().filter(|d| d.usage_page() == 0xFF00 && matches!(d.usage(), 1 | 2)).map(|d| (d.path().to_owned(), d.usage() == 2)).collect();
    let mut opened = Vec::new();
    for (path, long) in paths {
        let dev = match api.open_path(&path) {
            Ok(dev) => dev,
            Err(e) => {
                crate::log::debug("D013", format!("通知の待ち受け: {} を開けません: {e}", path.to_string_lossy()));
                continue;
            }
        };
        let mut battery = Vec::new();
        if long {
            let direct = Channel { dev: &dev, index: 0xFF };
            let indices: Vec<u8> = if direct.ping() && direct.feature(FEATURE_DEVICE_NAME).is_some() { vec![0xFF] } else { (1..=6).collect() };
            for index in indices {
                let ch = Channel { dev: &dev, index };
                if ch.ping() {
                    if let Some((id, f)) = ch.battery_feature() {
                        battery.push((index, id, f));
                    }
                }
            }
        }
        opened.push(Opened { dev, battery, long });
    }
    opened
}

/// 通知を待ち続ける (戻らない)。on_change は理由 (ログ用) を受け取る
pub fn listen(on_change: impl Fn(&str)) -> ! {
    #[allow(deprecated)]
    let mut api = loop {
        match HidApi::new_without_enumerate() {
            Ok(api) => break api,
            Err(e) => {
                crate::log::problem("listen_init", Some(("W014", format!("通知の待ち受けを開始できません: {e}"))), ("", ""));
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    };
    let mut opened: Vec<Opened> = Vec::new();
    let mut discovered_at: Option<Instant> = None;
    let mut last_seen: std::collections::HashMap<(usize, u8), Instant> = std::collections::HashMap::new();
    loop {
        let due = match discovered_at {
            None => true,
            Some(t) if opened.is_empty() => t.elapsed() >= RETRY,
            Some(t) => t.elapsed() >= REDISCOVER,
        };
        if due {
            let had = opened.iter().filter(|o| o.long).map(|o| o.battery.len()).sum::<usize>();
            opened = discover(&mut api);
            discovered_at = Some(Instant::now());
            last_seen.clear();
            let found = opened.iter().filter(|o| o.long).map(|o| o.battery.len()).sum::<usize>();
            if found != had {
                crate::log::info("I040", format!("通知の待ち受け: {} 個の接続口、{found} 台のデバイス", opened.len()));
                if found > had {
                    on_change("デバイスが見つかった");
                }
            }
        }
        if opened.is_empty() {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

        let wait = (200 / opened.len().max(1)).max(20) as i32;
        let mut broken = false;
        let mut rediscover = false;
        for (n, o) in opened.iter().enumerate() {
            let mut buf = [0u8; 20];
            match o.dev.read_timeout(&mut buf, wait) {
                Ok(0) => {}
                Ok(len) => {
                    let buf = &buf[..len];
                    let index = buf.get(1).copied().unwrap_or(0);
                    // 取得スレッドへの応答 (software id 付き) は無視する
                    if buf.len() >= 4 && buf[0] == REPORT_LONG && buf[3] & 0x0F == SOFTWARE_ID {
                        continue;
                    }
                    let gap = last_seen.insert((n, index), Instant::now()).map(|t| t.elapsed());
                    match parse_notice(buf, &o.battery) {
                        Some(Notice::Battery(i, pct, charging, full)) => {
                            crate::log::debug("D009", format!("通知: 番号 {i} {pct}%{}{}", if charging { " 充電中" } else { "" }, if full { " 満充電" } else { "" }));
                            on_change("残量・充電状態の通知");
                        }
                        Some(Notice::Link(i, up)) => {
                            crate::log::debug("D010", format!("通知: 番号 {i} {}", if up { "接続" } else { "切断" }));
                            if up && !o.battery.iter().any(|b| b.0 == i) {
                                rediscover = true;
                            }
                            on_change("接続状態の通知");
                        }
                        None => {
                            if o.long && index != 0xFF && !o.battery.iter().any(|b| b.0 == index) && (1..=6).contains(&index) {
                                // 調べたときに眠っていたデバイスが起きた
                                rediscover = true;
                                on_change("デバイスの復帰");
                            } else if gap.is_none_or(|g| g >= WAKE_GAP) && o.battery.iter().any(|b| b.0 == index) {
                                crate::log::debug("D011", format!("通知: 番号 {index} からの通信 (スリープからの復帰)"));
                                on_change("スリープからの復帰");
                            }
                        }
                    }
                }
                Err(e) => {
                    broken = true;
                    crate::log::debug("D014", format!("通知の待ち受け: 読み込みエラー: {e}"));
                }
            }
        }
        if broken {
            // 受信機を抜いた・ドライバーが入れ替わったなど。閉じて探し直す
            crate::log::info("I041", "受信機との通信が切れました。探し直します");
            opened.clear();
            discovered_at = Some(Instant::now());
            on_change("受信機の取り外し");
        } else if rediscover && discovered_at.is_none_or(|t| t.elapsed() >= MIN_REDISCOVER) {
            // 調べ直しは 1 秒ほどかかるので、続けて起きても間を空ける
            discovered_at = None;
        }
    }
}

#[cfg(test)]
mod probe {
    use super::*;

    /// どんな情報が取れるか調べる (実機用)
    #[test]
    #[ignore = "実機調査"]
    fn dump_device_info() {
        let mut api = HidApi::new().unwrap();
        api.reset_devices().unwrap();
        api.add_devices(VENDOR_LOGITECH, 0).unwrap();
        let paths: Vec<_> = api.device_list().filter(|d| d.usage_page() == 0xFF00 && d.usage() == 0x0002).map(|d| d.path().to_owned()).collect();
        for path in paths {
            let dev = api.open_path(&path).unwrap();
            for index in [0xFFu8, 1, 2, 3, 4, 5, 6] {
                let ch = Channel { dev: &dev, index };
                if !ch.ping() {
                    continue;
                }
                println!("== path={path:?} index={index:#04x} name={:?}", ch.name());

                // 対応機能の一覧 (0x0001 FeatureSet)
                if let Some(fs) = ch.feature(0x0001) {
                    let count = ch.call(fs, 0, &[]).map(|r| r[0]).unwrap_or(0);
                    let mut ids = Vec::new();
                    for i in 1..=count {
                        if let Some(r) = ch.call(fs, 1, &[i]) {
                            ids.push(format!("{:04X}", u16::from_be_bytes([r[0], r[1]])));
                        }
                    }
                    println!("features: {}", ids.join(" "));
                }

                // 0x0003 DeviceInformation: [entity数, unitId(4), transport(2), modelId(6), extModel, capabilities]
                if let Some(f) = ch.feature(0x0003) {
                    if let Some(r) = ch.call(f, 0, &[]) {
                        let unit = u32::from_be_bytes([r[1], r[2], r[3], r[4]]);
                        println!("deviceInfo raw={:02X?}", &r[..16]);
                        println!("unitId=0x{unit:08X} ({unit})  modelId={:02X?}  capabilities=0x{:02X}", &r[7..13], r[14]);
                    }
                    // getDeviceSerialNumber (fn2)
                    match ch.call(f, 2, &[]) {
                        Some(r) => println!("serial={:?}", String::from_utf8_lossy(&r[..12])),
                        None => println!("serial: 非対応"),
                    }
                }

                // 0x1004 UnifiedBattery: get_capabilities (fn0) / get_status (fn1)
                if let Some(f) = ch.feature(0x1004) {
                    println!("unifiedBattery capabilities raw={:02X?}", ch.call(f, 0, &[]).map(|r| r[..8].to_vec()));
                    println!("unifiedBattery status raw={:02X?}", ch.call(f, 1, &[]).map(|r| r[..8].to_vec()));
                }
                // 電圧 / 電流系
                for (id, label) in [(0x1001u16, "BatteryVoltage"), (0x1F20, "ADC measurement"), (0x1000, "BatteryStatus")] {
                    match ch.feature(id) {
                        Some(f) => println!("{label}({id:04X}) fn0 raw={:02X?}", ch.call(f, 0, &[]).map(|r| r[..8].to_vec())),
                        None => println!("{label}({id:04X}): 非対応"),
                    }
                }
            }
        }
    }
}

