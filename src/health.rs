//! 電池の健康状態 (推定)。
//! 充電を始めるたびに、その前の放電区間 (何 % を何時間で使ったか) を health.csv に 1 行記録し、
//! そこから「満充電での持ち時間」「推定容量」「劣化・買い替えの目安」を求める。
//! 1 行 = "終了unix秒,デバイス,開始%,終了%,時間,放電量mW(不明なら空)"

use crate::history::Sample;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 記録する放電区間の最低条件
const MIN_DROP: i32 = 10;
const MIN_HOURS: f64 = 1.0;
/// 一般的なリチウムイオン電池: 約 400 回で容量 80%、約 600 回で 70% (目安)
const CYCLES_TO_80: f64 = 400.0;
const CYCLES_TO_70: f64 = 600.0;
/// 健康状態を出すのに必要な放電区間の数
pub const MIN_RUNS: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub end: u64,
    pub start_pct: i32,
    pub end_pct: i32,
    pub hours: f64,
    pub power_mw: Option<f64>,
}

impl Run {
    fn drop(&self) -> f64 {
        (self.start_pct - self.end_pct) as f64
    }

    /// 満充電 (100%→0%) での持ち時間
    pub fn full_runtime_h(&self) -> f64 {
        self.hours * 100.0 / self.drop()
    }

    /// 推定容量 (mWh) = 放電量 × 満充電での持ち時間
    pub fn capacity_mwh(&self) -> Option<f64> {
        self.power_mw.map(|p| p * self.full_runtime_h())
    }
}

pub fn path_beside(history: &Path) -> PathBuf {
    history.with_file_name("health.csv")
}

pub fn append(path: &Path, device: &str, run: &Run) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let power = run.power_mw.map(|p| format!("{p:.2}")).unwrap_or_default();
        let _ = writeln!(f, "{},{},{},{},{:.3},{}", run.end, device, run.start_pct, run.end_pct, run.hours, power);
    }
}

pub fn load_all(path: &Path) -> HashMap<String, Vec<Run>> {
    let mut map: HashMap<String, Vec<Run>> = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else { return map };
    for line in text.lines() {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() != 6 {
            continue;
        }
        let (Ok(end), Ok(start_pct), Ok(end_pct), Ok(hours)) = (f[0].parse(), f[2].parse(), f[3].parse(), f[4].parse()) else { continue };
        map.entry(f[1].to_ascii_lowercase()).or_default().push(Run { end, start_pct, end_pct, hours, power_mw: f[5].parse().ok() });
    }
    for v in map.values_mut() {
        v.sort_by_key(|r| r.end);
    }
    map
}

/// 履歴の最後の放電区間 (直前の充電の後から、最後に放電していた点まで) を取り出す
pub fn last_run(samples: &[Sample], power_mw: Option<f64>) -> Option<Run> {
    let end = samples.iter().rposition(|s| !s.charging)?;
    let start = samples[..end].iter().rposition(|s| s.charging).map(|i| i + 1).unwrap_or(0);
    let (a, b) = (samples[start], samples[end]);
    let hours = (b.t.saturating_sub(a.t)) as f64 / 3600.0;
    (a.percent - b.percent >= MIN_DROP && hours >= MIN_HOURS).then_some(Run { end: b.t, start_pct: a.percent, end_pct: b.percent, hours, power_mw })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Basis {
    /// 実測した容量の減り方から
    Trend,
    /// 一般的な充電回数の寿命から
    Cycles,
}

#[derive(Debug, Default)]
pub struct Health {
    pub runs: usize,
    /// このアプリで記録してからの充電サイクル数 (放電した % の合計 / 100)
    pub cycles: f64,
    /// 最近の満充電での持ち時間 (実測)
    pub full_runtime_h: Option<f64>,
    /// 最近の推定容量 (mWh)
    pub capacity_mwh: Option<f64>,
    /// 記録を始めた頃と比べた容量 (1.0 = 100%)
    pub health: Option<f64>,
    /// 容量 80% になる目安 (劣化が目立ち始める)
    pub degrade_at: Option<(u64, Basis)>,
    /// 容量 70% になる目安 (買い替え時期)
    pub replace_at: Option<(u64, Basis)>,
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(v[v.len() / 2])
}

pub fn analyze(runs: &[Run], now: u64) -> Health {
    let mut h = Health { runs: runs.len(), cycles: runs.iter().map(|r| r.drop() / 100.0).sum::<f64>() + 0.0, ..Default::default() };
    if runs.is_empty() {
        return h;
    }

    let recent = &runs[runs.len().saturating_sub(3)..];
    h.full_runtime_h = median(recent.iter().map(Run::full_runtime_h).collect());
    h.capacity_mwh = median(recent.iter().filter_map(Run::capacity_mwh).collect());

    // 比べる指標: 全区間で放電量が分かれば容量、そうでなければ持ち時間
    let use_capacity = runs.iter().all(|r| r.power_mw.is_some());
    let metric = |r: &Run| if use_capacity { r.capacity_mwh().unwrap_or(0.0) } else { r.full_runtime_h() };

    if runs.len() >= MIN_RUNS {
        let baseline = median(runs[..3].iter().map(metric).collect()).unwrap_or(0.0);
        let current = median(recent.iter().map(metric).collect()).unwrap_or(0.0);
        if baseline > 0.0 {
            h.health = Some((current / baseline).min(1.0));
        }

        // 実測の傾向: 6 区間以上・14 日以上あれば、容量比の回帰直線が 80% / 70% を切る日
        let span_days = (runs[runs.len() - 1].end - runs[0].end) as f64 / 86400.0;
        if runs.len() >= 6 && span_days >= 14.0 && baseline > 0.0 {
            let xs: Vec<f64> = runs.iter().map(|r| (r.end - runs[0].end) as f64 / 86400.0).collect();
            let ys: Vec<f64> = runs.iter().map(|r| metric(r) / baseline).collect();
            let n = xs.len() as f64;
            let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
            let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
            let slope = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum::<f64>() / sxx;
            if slope < 0.0 {
                let at = |level: f64| {
                    let day = mx + (level - my) / slope;
                    let today = now.saturating_sub(runs[0].end) as f64 / 86400.0;
                    (runs[0].end as f64 + day.max(today) * 86400.0) as u64
                };
                h.degrade_at = Some((at(0.8), Basis::Trend));
                h.replace_at = Some((at(0.7), Basis::Trend));
            }
        }
    }

    // 傾向が出ないうちは充電回数のペースから
    if h.degrade_at.is_none() {
        let first_start = runs[0].end.saturating_sub((runs[0].hours * 3600.0) as u64);
        let span_days = (now.saturating_sub(first_start)) as f64 / 86400.0;
        if span_days >= 1.0 && h.cycles > 0.0 {
            let per_day = h.cycles / span_days;
            let at = |target: f64| now + (((target - h.cycles).max(0.0) / per_day) * 86400.0) as u64;
            h.degrade_at = Some((at(CYCLES_TO_80), Basis::Cycles));
            h.replace_at = Some((at(CYCLES_TO_70), Basis::Cycles));
        }
    }
    h
}

/// unix 秒 -> (年, 月) (UTC。月単位の目安なので時差は気にしない)
pub fn year_month(t: u64) -> (i64, u32) {
    let (y, m, _) = ymd(t);
    (y, m)
}

/// unix 秒 -> (年, 月, 日)
pub fn ymd(t: u64) -> (i64, u32, u32) {
    let days = (t / 86400) as i64;
    // Howard Hinnant の civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}
