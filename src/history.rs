//! 残量の履歴 (%LOCALAPPDATA%\GHUBBatteryTray\history.csv)。
//! 常駐プロセスが記録し、設定画面がグラフにする。1 行 = "unix秒,デバイス,残量%,充電中(0/1)"

use crate::ghub::Battery;
use crate::settings;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 残量が変わらなくてもこの間隔で 1 行書く
const KEEP_ALIVE_SECS: u64 = 15 * 60;
/// これより古い行は捨てる
const KEEP_SECS: u64 = 7 * 24 * 3600;
/// ファイルがこれを超えたら古い行を整理する
const COMPACT_BYTES: u64 = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub t: u64,
    pub percent: i32,
    pub charging: bool,
}

pub fn path() -> PathBuf {
    settings::dir().join("history.csv")
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 前回書いた値 (デバイスごと)
#[derive(Default)]
pub struct Recorder {
    last: HashMap<String, Sample>,
    /// 放電中に最後に分かった放電量 (充電が始まった時点の記録に使う)
    last_power: HashMap<String, f64>,
}

impl Recorder {
    pub fn record(&mut self, list: &[Battery]) {
        self.record_to(&path(), list, now());
    }

    pub fn record_to(&mut self, path: &Path, list: &[Battery], t: u64) {
        let mut lines = String::new();
        let mut started_charging = Vec::new();
        for b in list.iter().filter(|b| b.connected && !b.asleep) {
            let s = Sample { t, percent: b.percentage, charging: b.charging };
            if !b.charging {
                if let Some(p) = b.power_mw {
                    self.last_power.insert(b.device_name.clone(), p);
                }
            }
            if b.charging && self.last.get(&b.device_name).is_some_and(|p| !p.charging) {
                started_charging.push(b.device_name.clone());
            }
            let changed = self.last.get(&b.device_name).is_none_or(|p| p.percent != s.percent || p.charging != s.charging || t >= p.t + KEEP_ALIVE_SECS);
            if changed {
                lines += &format!("{},{},{},{}\n", t, b.device_name, s.percent, s.charging as u8);
                self.last.insert(b.device_name.clone(), s);
            }
        }
        if lines.is_empty() {
            return;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let written = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(lines.as_bytes()));
        crate::log::problem("history", written.err().map(|e| format!("history.csv に書き込めません: {e}")), "history.csv に書き込めるようになりました");
        if !started_charging.is_empty() {
            let all = load_all(path);
            for device in started_charging {
                let samples = all.get(&device.to_ascii_lowercase()).map(Vec::as_slice).unwrap_or(&[]);
                if let Some(run) = crate::health::last_run(samples, self.last_power.get(&device).copied()) {
                    crate::health::append(&crate::health::path_beside(path), &device, &run);
                    crate::log::info(format!(
                        "放電の記録を追加: {device} {}% → {}% / {:.1} 時間 (満充電時の使用可能時間 約{:.0} 時間)",
                        run.start_pct, run.end_pct, run.hours, run.full_runtime_h()
                    ));
                }
            }
        }
        if std::fs::metadata(path).is_ok_and(|m| m.len() > COMPACT_BYTES) {
            compact(path, t);
        }
    }
}

/// 古い行を消す
fn compact(path: &Path, t: u64) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let kept: String = text
        .lines()
        .filter(|l| l.split(',').next().and_then(|v| v.parse::<u64>().ok()).is_some_and(|lt| lt + KEEP_SECS >= t))
        .map(|l| format!("{l}\n"))
        .collect();
    if let Err(e) = std::fs::write(path, kept) {
        crate::log::warn(format!("history.csv の古い行を消せませんでした: {e}"));
    }
}

/// デバイスごとの履歴 (古い順)
pub fn load_all(path: &Path) -> HashMap<String, Vec<Sample>> {
    let mut map: HashMap<String, Vec<Sample>> = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else { return map };
    for line in text.lines() {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() != 4 {
            continue;
        }
        let (Ok(t), Ok(percent)) = (f[0].parse(), f[2].parse()) else { continue };
        map.entry(f[1].to_ascii_lowercase()).or_default().push(Sample { t, percent, charging: f[3] == "1" });
    }
    for v in map.values_mut() {
        v.sort_by_key(|s| s.t);
    }
    map
}

/// グラフ用: 今の放電区間の実績と、このまま減った場合の予測
#[derive(Debug, PartialEq)]
pub struct Trend {
    /// 実績 (最後に充電していた時点以降、最大 24 時間)
    pub actual: Vec<Sample>,
    /// 1 時間あたりの減少 % (正の値)。分からなければ None
    pub drain_per_hour: Option<f64>,
}

impl Trend {
    /// 0% になる予測時刻
    pub fn empty_at(&self, now: u64, percent: i32) -> Option<u64> {
        let rate = self.drain_per_hour?;
        Some(now + (percent.max(0) as f64 / rate * 3600.0) as u64)
    }
}

const WINDOW_SECS: u64 = 24 * 3600;

/// 実績から減り方を求める。データが足りなければ G HUB の残り時間から求める
pub fn trend(samples: &[Sample], now: u64, current: &Battery) -> Trend {
    // 最後に充電していたところより後 & 直近 24 時間
    let start = samples.iter().rposition(|s| s.charging).map(|i| i + 1).unwrap_or(0);
    let mut actual: Vec<Sample> = samples[start..].iter().filter(|s| s.t + WINDOW_SECS >= now && s.t <= now).copied().collect();
    if current.connected && !current.charging && actual.last().is_none_or(|s| s.t < now) {
        actual.push(Sample { t: now, percent: current.percentage, charging: false });
    }

    let mut drain = None;
    if actual.len() >= 2 && actual.last().unwrap().t >= actual[0].t + 20 * 60 {
        // 最小二乗法で傾きを求める (時間 -> %)
        let n = actual.len() as f64;
        let xs: Vec<f64> = actual.iter().map(|s| (s.t - actual[0].t) as f64 / 3600.0).collect();
        let ys: Vec<f64> = actual.iter().map(|s| s.percent as f64).collect();
        let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
        let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
        let sxy: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
        if sxx > 0.0 {
            let slope = sxy / sxx;
            if slope < -0.05 {
                drain = Some(-slope);
            }
        }
    }
    if drain.is_none() && !current.charging {
        if let Some(h) = current.remaining_hours.filter(|h| *h > 0.0) {
            drain = Some(current.percentage as f64 / h);
        }
    }
    if current.charging {
        drain = None;
    }
    Trend { actual, drain_per_hour: drain }
}
