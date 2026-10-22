//! 取得元の優先順位: 直接通信 (HID++) > G HUB API > settings.db

use crate::ghub::{self, Battery};
use crate::hidpp::{self, Reader};
use crate::settings;
use std::collections::BTreeMap;

/// 直接通信で見つけた名前 -> 設定のキー (G HUB 時代の名前を引き継ぐための対応表)
fn aliases_path() -> std::path::PathBuf {
    settings::dir().join("aliases.json")
}

fn load_aliases() -> BTreeMap<String, String> {
    std::fs::read_to_string(aliases_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn collect(reader: &mut Reader) -> Vec<Battery> {
    let mut list = reader.read_all();
    let agent = ghub::fetch_agent();
    let mut aliases = load_aliases();
    let mut aliases_changed = false;

    for b in &mut list {
        let hid_key = b.device_name.clone();
        let matched = agent.iter().flatten().find(|a| hidpp::normalize(a.label()) == hid_key || a.device_name == hid_key);

        if let Some(a) = matched {
            // G HUB が動いていれば同じデバイスの名前・残り時間を借りる
            if b.asleep && a.connected {
                // 直接通信がスリープで古い値しかないときは G HUB の値のほうが新しい
                *b = a.clone();
            }
            b.device_name = a.device_name.clone();
            b.display_name = a.label().to_string();
            b.remaining_hours = a.remaining_hours;
            b.power_mw = a.power_mw;
            b.max_life_hours = a.max_life_hours;
            if aliases.get(&hid_key) != Some(&a.device_name) {
                aliases.insert(hid_key, a.device_name.clone());
                aliases_changed = true;
            }
        } else if let Some(key) = aliases.get(&hid_key) {
            b.device_name = key.clone();
        }
    }

    if aliases_changed {
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
