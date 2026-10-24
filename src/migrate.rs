//! 同じデバイスが別の名前で記録されていたとき (直接通信の名前と G HUB の名前に分かれていた) に、
//! 統計・履歴・放電の記録・設定を G HUB 側の名前にまとめる。
//!
//! 分かれていた間は両方の名前で同じ時間を記録しているので、そのまま足すと二重に数える。
//! - 統計: 同じ時は、観測した時間が長いほうのレコードを残す。片方にしかない時はそのまま入れる
//! - 履歴: G HUB 側の記録がない時間帯 (前後 15 分) の行だけ、名前を書き換えて残す
//! - 放電の記録: G HUB 側に終わりが 2 時間以内の記録があれば重複として捨てる
//! - 設定: 通知ルールは G HUB 側に無いものだけ足し、トレイ表示は G HUB 側の名前に置き換える
//! 古い統計ファイルは「.bin.merged」に名前を変えて残す。

use crate::health::Run;
use crate::settings::Settings;
use crate::stats::{self, Hour};
use std::collections::BTreeMap;
use std::path::Path;

/// まとめた件数
#[derive(Debug, Default, PartialEq)]
pub struct Merged {
    pub stats_hours: usize,
    pub history_lines: usize,
    pub runs: usize,
    pub settings: bool,
}

impl Merged {
    pub fn any(&self) -> bool {
        self.stats_hours + self.history_lines + self.runs > 0 || self.settings
    }
}

/// 統計ファイル old を new にまとめる。まとめた (old にしかなかった) 時の数
pub fn merge_stats(old: &Path, new: &Path) -> std::io::Result<usize> {
    if !old.exists() {
        return Ok(0);
    }
    let mut by_hour: BTreeMap<u32, Hour> = stats::load(new).into_iter().map(|h| (h.hour, h)).collect();
    let mut added = 0;
    for h in stats::load(old) {
        let observed = |x: &Hour| x.use_secs as u32 + x.charge_secs as u32;
        match by_hour.get(&h.hour) {
            Some(cur) if observed(cur) >= observed(&h) => {}
            Some(_) => {
                by_hour.insert(h.hour, h);
            }
            None => {
                by_hour.insert(h.hour, h);
                added += 1;
            }
        }
    }
    let all: Vec<Hour> = by_hour.into_values().collect();
    stats::save_all(new, &all)?;
    std::fs::rename(old, old.with_extension("bin.merged"))?;
    Ok(added)
}

/// 書き込み途中で落ちても壊れないよう、一時ファイルに書いてから置き換える
fn replace(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// history.csv ("unix秒,デバイス,残量,充電中") の old の行を new にまとめる。残した行の数
pub fn merge_history(path: &Path, old: &str, new: &str) -> std::io::Result<usize> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(0) };
    let device = |l: &str| l.split(',').nth(1).map(str::to_ascii_lowercase);
    let time = |l: &str| l.split(',').next().and_then(|t| t.parse::<u64>().ok());
    if !text.lines().any(|l| device(l).as_deref() == Some(old)) {
        return Ok(0);
    }
    let mut new_times: Vec<u64> = text.lines().filter(|l| device(l).as_deref() == Some(new)).filter_map(time).collect();
    new_times.sort_unstable();
    let near_new = |t: u64| {
        let i = new_times.partition_point(|&x| x < t);
        [i.checked_sub(1), Some(i)].into_iter().flatten().filter_map(|i| new_times.get(i)).any(|&x| x.abs_diff(t) <= 15 * 60)
    };
    let mut kept = 0;
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if device(line).as_deref() == Some(old) {
            let Some(t) = time(line) else { continue };
            if near_new(t) {
                continue;
            }
            let f: Vec<&str> = line.split(',').collect();
            out += &format!("{},{},{}\n", f[0], new, f[2..].join(","));
            kept += 1;
        } else {
            out += line;
            out.push('\n');
        }
    }
    replace(path, &out)?;
    Ok(kept)
}

/// health.csv の old の放電記録を new にまとめる。残した記録の数
pub fn merge_health(path: &Path, old: &str, new: &str) -> std::io::Result<usize> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(0) };
    let mut all = crate::health::load_all(path);
    let Some(old_runs) = all.remove(old) else { return Ok(0) };
    let new_runs: Vec<Run> = all.remove(new).unwrap_or_default();
    let kept: Vec<&Run> = old_runs.iter().filter(|r| !new_runs.iter().any(|n| n.end.abs_diff(r.end) <= 2 * 3600)).collect();
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if line.split(',').nth(1).map(str::to_ascii_lowercase).as_deref() != Some(old) {
            out += line;
            out.push('\n');
        }
    }
    for r in &kept {
        let power = r.power_mw.map(|p| format!("{p:.2}")).unwrap_or_default();
        out += &format!("{},{},{},{},{:.3},{}\n", r.end, new, r.start_pct, r.end_pct, r.hours, power);
    }
    replace(path, &out)?;
    Ok(kept.len())
}

/// 設定の old を new にまとめる。変えたら true
pub fn merge_settings(s: &mut Settings, old: &str, new: &str) -> bool {
    let mut changed = false;
    let old_key = s.device_notifications.keys().find(|k| k.eq_ignore_ascii_case(old)).cloned();
    if let Some(key) = old_key {
        let old_rules = s.device_notifications.remove(&key).unwrap_or_default();
        let new_key = s.device_notifications.keys().find(|k| k.eq_ignore_ascii_case(new)).cloned().unwrap_or_else(|| new.to_string());
        let rules = s.device_notifications.entry(new_key).or_default();
        for r in old_rules {
            if !rules.iter().any(|x| x.battery == r.battery && x.message == r.message) {
                rules.push(r);
            }
        }
        changed = true;
    }
    if s.is_tray(old) {
        s.set_tray(old, false);
        s.set_tray(new, true);
        changed = true;
    }
    changed
}

/// old の記録・設定を new にまとめる (常駐の取得スレッドから呼ぶ)
pub fn run(old: &str, new: &str) -> Merged {
    let old = old.to_ascii_lowercase();
    let new = new.to_ascii_lowercase();
    let mut m = Merged::default();
    let fail = |what: &str, e: std::io::Error| crate::log::warn("W019", format!("{what}をまとめられませんでした ({old} → {new}): {e}"));
    match merge_stats(&stats::path_for(&old), &stats::path_for(&new)) {
        Ok(n) => m.stats_hours = n,
        Err(e) => fail("統計", e),
    }
    let history = crate::history::path();
    match merge_history(&history, &old, &new) {
        Ok(n) => m.history_lines = n,
        Err(e) => fail("履歴", e),
    }
    match merge_health(&crate::health::path_beside(&history), &old, &new) {
        Ok(n) => m.runs = n,
        Err(e) => fail("放電の記録", e),
    }
    let mut s = Settings::load();
    if merge_settings(&mut s, &old, &new) {
        match s.save() {
            Ok(()) => m.settings = true,
            Err(e) => fail("設定", e),
        }
    }
    if m.any() {
        crate::log::info("I046", format!(
            "別の名前の記録をまとめました: {old} → {new} (統計 {} 時間分, 履歴 {} 行, 放電の記録 {} 件, 設定 {})",
            m.stats_hours, m.history_lines, m.runs, if m.settings { "変更あり" } else { "変更なし" }
        ));
    }
    m
}
