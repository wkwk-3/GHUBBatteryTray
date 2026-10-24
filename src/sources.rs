//! 取得元の優先順位: 直接通信 (HID++) > G HUB API > settings.db

use crate::ghub::{self, Battery};
use crate::hidpp::{self, Reader};
use crate::settings;
use std::collections::BTreeMap;

/// 直接通信で見つけた名前 -> 設定のキー (G HUB 時代の名前を引き継ぐための対応表)
fn aliases_path() -> std::path::PathBuf {
    settings::dir().join("aliases.json")
}

/// 直接通信の名前と G HUB の名前の対応が増えた・変わった (常駐が記録をまとめる合図)
pub static ALIASES_CHANGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn load_aliases() -> BTreeMap<String, String> {
    std::fs::read_to_string(aliases_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// 直接通信 (hid) と G HUB (agent) の 2 つが同じデバイスか。
/// 1. デバイス固有の番号 (unitId) が同じ  2. 製品 ID が重なる  3. 名前が同じか、一方がもう一方の先頭と一致
/// (G703 は直接通信の名前が "G703 LIGHTSPEED Wireless Gaming Mouse w/ HERO"、G HUB は "G703 LIGHTSPEED" / "g703hero")
pub fn same_device(hid: &Battery, agent: &Battery) -> bool {
    if let (Some(a), Some(b)) = (&hid.unit_id, &agent.unit_id) {
        return a == b;
    }
    if !hid.pids.is_empty() && hid.pids.iter().any(|p| agent.pids.contains(p)) {
        return true;
    }
    let hid_key = hidpp::normalize(hid.label());
    let names = [hidpp::normalize(agent.label()), agent.device_name.to_ascii_lowercase()];
    names.iter().any(|n| !n.is_empty() && (*n == hid_key || *n == hid.device_name || (n.len() >= 6 && hid_key.starts_with(n.as_str()))))
}

/// 同じデバイスの直接通信の値 (hid) に、G HUB の値 (agent) を足す
pub fn merge(hid: &mut Battery, agent: &Battery) {
    // 電圧から推定している残量は充電中に高く出るので、G HUB の残量・充電状態を使う
    if hid.voltage_estimate && agent.connected && !hid.asleep {
        hid.percentage = agent.percentage;
        hid.charging = agent.charging;
        hid.fully_charged = agent.fully_charged;
    }
    // 直接通信がスリープで古い値しかないときは G HUB の値のほうが新しい
    if hid.asleep && agent.connected {
        let (voltage, unit_id, pids) = (hid.voltage_estimate, hid.unit_id.clone(), hid.pids.clone());
        *hid = agent.clone();
        hid.voltage_estimate = voltage;
        hid.unit_id = hid.unit_id.clone().or(unit_id);
        if hid.pids.is_empty() {
            hid.pids = pids;
        }
    }
    // 名前 (設定のキー)・残り時間・放電量は G HUB のものを使う
    hid.device_name = agent.device_name.clone();
    hid.display_name = agent.label().to_string();
    hid.remaining_hours = agent.remaining_hours;
    hid.power_mw = agent.power_mw;
    hid.max_life_hours = agent.max_life_hours;
}

pub fn collect(reader: &mut Reader) -> Vec<Battery> {
    let mut list = reader.read_all();
    let agent = ghub::fetch_agent();
    let mut aliases = load_aliases();
    let mut aliases_changed = false;

    for b in &mut list {
        let hid_key = b.device_name.clone();
        let matched = agent.iter().flatten().find(|a| same_device(b, a));

        if let Some(a) = matched {
            merge(b, a);
            if aliases.get(&hid_key) != Some(&a.device_name) {
                aliases.insert(hid_key, a.device_name.clone());
                aliases_changed = true;
            }
        } else if let Some(key) = aliases.get(&hid_key) {
            b.device_name = key.clone();
        }
    }

    if aliases_changed {
        ALIASES_CHANGED.store(true, std::sync::atomic::Ordering::Relaxed);
        let saved = std::fs::create_dir_all(settings::dir()).and_then(|_| std::fs::write(aliases_path(), serde_json::to_string_pretty(&aliases).unwrap_or_default()));
        if let Err(e) = saved {
            crate::log::error("E005", format!("aliases.json を保存できませんでした: {e}"));
        }
    }

    // 直接通信で読めなかったデバイスは G HUB API の値を使う
    for a in agent.into_iter().flatten() {
        if !list.iter().any(|b| b.device_name.eq_ignore_ascii_case(&a.device_name)) {
            list.push(a);
        }
    }

    if list.is_empty() {
        list = match ghub::read_settings_db() {
            Ok(list) => {
                crate::log::problem("settings_db", None, ("", ""));
                list
            }
            Err(e) => {
                crate::log::problem("settings_db", Some(("W009", format!("G HUB の settings.db を読めません: {e}"))), ("", ""));
                Vec::new()
            }
        };
    }
    list.sort_by(|a, b| a.label().cmp(b.label()));
    list
}
