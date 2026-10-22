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
                crate::log::error(format!("HID の初期化に失敗しました。直接通信は使えません: {e}"));
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
                    crate::log::debug(format!("HID: {} を開けません: {e}", k.name));
                    None
                }
            };
            if reading.is_none() {
                crate::log::debug(format!("HID: {} から応答がありません", k.name));
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
                        power_mw: None,
                        max_life_hours: None,
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
            crate::log::problem("hid_scan", Some(format!("HID デバイスの一覧を取得できません: {e}")), "");
            return;
        }
        crate::log::problem("hid_scan", None, "HID デバイスの一覧を取得できるようになりました");

        let paths: Vec<_> = api
            .device_list()
            .filter(|d| d.usage_page() == 0xFF00 && d.usage() == 0x0002)
            .map(|d| d.path().to_owned())
            .collect();

        for path in paths {
            let dev = match api.open_path(&path) {
                Ok(dev) => dev,
                Err(e) => {
                    crate::log::debug(format!("HID: {} を開けません: {e}", path.to_string_lossy()));
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
                    crate::log::debug(format!("HID: 番号 {index} はバッテリー機能に対応していません"));
                    continue;
                };
                let name = ch.name().unwrap_or_else(|| format!("Logitech device {index}"));
                if self.known.iter().any(|k| k.name == name) {
                    continue;
                }
                let last = previous.iter().find(|k| k.name == name).and_then(|k| k.last.clone());
                self.known.push(Known { path: path.clone(), index, name, feature, last });
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
                crate::log::info(format!("HID 直接通信で見つかったデバイス: {found}"));
                *last = Some(found);
            }
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
