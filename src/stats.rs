//! 長期の統計 (%LOCALAPPDATA%\GHUBBatteryTray\stats\<デバイス>.bin)。
//!
//! 保存形式: 8 バイトのヘッダー ("GHBS" + 版 u16 + 予備 u16) の後に、1 時間 1 レコード (20 バイト固定長、リトルエンディアン)。
//! - 書き込みは「追記」か「最後のレコードの上書き」だけ。1 回の書き込みは 20 バイト、5 分に 1 回まで
//! - 1 年で 8,760 レコード = 約 171 KB / デバイス。読み込みは一括で数 ms
//! - 残量は 1/1000 % 単位で積み上げるので、整数 % の読み取りでも長期の合計はずれない
//! - 途中で落ちて半端なレコードが残っても、読むときに捨てる
//!
//! 記録する量 (1 時間ごと):
//! - 使用中 (接続・放電中で、15 分以内の間隔で読めた区間) の秒数と、その間に減った残量
//! - 読めなかった間 (スリープ・PC 停止など) に減った残量
//! - 充電中の秒数と増えた残量
//! - 放電量 (mW) が分かっていた秒数・平均・その間に減った残量 (容量の計算用)

use crate::ghub::Battery;
use crate::health::{Basis, Health, Run};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"GHBS";
const VERSION: u16 = 1;
const HEADER: u64 = 8;
pub const RECORD: usize = 20;
/// これより間が空いたら「読めなかった間」として扱う
const MAX_STEP_SECS: u64 = 15 * 60;
/// 今の 1 時間分を書き出す間隔
const FLUSH_SECS: u64 = 5 * 60;
const DAY: u64 = 86_400;
/// 最近の値に使う期間
pub const RECENT_DAYS: u64 = 30;
/// 容量の推移を見る区切り
const WINDOW_DAYS: u64 = 14;
/// 値を出すのに必要な、減った残量の合計 (%)
const MIN_DRAIN: f64 = 10.0;
/// 一般的なリチウムイオン電池: 約 400 回で容量 80%、約 600 回で 70% (目安)
const CYCLES_TO_80: f64 = 400.0;
const CYCLES_TO_70: f64 = 600.0;

/// 1 時間分の統計 (ファイルの 1 レコード)
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hour {
    /// unix 時 (UTC の unix 秒 / 3600)
    pub hour: u32,
    /// 使用中の秒数
    pub use_secs: u16,
    /// 使用中に減った残量 (1/1000 %)
    pub drain_m: u16,
    /// 読めなかった間に減った残量 (1/1000 %)
    pub gap_m: u16,
    /// 充電中の秒数
    pub charge_secs: u16,
    /// 充電で増えた残量 (1/1000 %)
    pub gain_m: u16,
    /// 放電量の平均 (1/100 mW)
    pub power_cw: u16,
    /// 放電量が分かっていた秒数
    pub power_secs: u16,
    /// 放電量が分かっていた間に減った残量 (1/1000 %)
    pub power_drain_m: u16,
}

impl Hour {
    fn encode(&self) -> [u8; RECORD] {
        let mut b = [0u8; RECORD];
        b[0..4].copy_from_slice(&self.hour.to_le_bytes());
        for (i, v) in [self.use_secs, self.drain_m, self.gap_m, self.charge_secs, self.gain_m, self.power_cw, self.power_secs, self.power_drain_m].iter().enumerate() {
            b[4 + i * 2..6 + i * 2].copy_from_slice(&v.to_le_bytes());
        }
        b
    }

    fn decode(b: &[u8]) -> Hour {
        let u = |i: usize| u16::from_le_bytes([b[4 + i * 2], b[5 + i * 2]]);
        Hour {
            hour: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            use_secs: u(0),
            drain_m: u(1),
            gap_m: u(2),
            charge_secs: u(3),
            gain_m: u(4),
            power_cw: u(5),
            power_secs: u(6),
            power_drain_m: u(7),
        }
    }

    fn drain(&self) -> f64 {
        self.drain_m as f64 / 1000.0
    }

    fn gap(&self) -> f64 {
        self.gap_m as f64 / 1000.0
    }
}

pub fn dir() -> PathBuf {
    crate::settings::dir().join("stats")
}

/// デバイスごとのファイル (デバイス名の英数字以外は _ にする)
pub fn path_in(dir: &Path, device: &str) -> PathBuf {
    let key: String = device.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    dir.join(format!("{key}.bin"))
}

pub fn path_for(device: &str) -> PathBuf {
    path_in(&dir(), device)
}

/// 全レコード (古い順)。ファイルが無い・壊れていれば空
pub fn load(path: &Path) -> Vec<Hour> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    if bytes.len() < HEADER as usize || &bytes[0..4] != MAGIC || u16::from_le_bytes([bytes[4], bytes[5]]) != VERSION {
        return Vec::new();
    }
    bytes[HEADER as usize..].chunks_exact(RECORD).map(Hour::decode).collect()
}

/// 1 時間分を書く。同じ時のレコードが最後にあれば上書き、なければ追記
fn store(path: &Path, h: &Hour) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    let mut len = f.metadata()?.len();
    let mut head = [0u8; 8];
    let valid = len >= HEADER && f.read_exact(&mut head).is_ok() && &head[0..4] == MAGIC;
    if !valid {
        // 新規 (または読めないファイル) は作り直す
        f.set_len(0)?;
        f.seek(SeekFrom::Start(0))?;
        let mut header = [0u8; HEADER as usize];
        header[0..4].copy_from_slice(MAGIC);
        header[4..6].copy_from_slice(&VERSION.to_le_bytes());
        f.write_all(&header)?;
        len = HEADER;
    }
    // 途中で落ちたときの半端なレコードは捨てる
    let whole = len - (len - HEADER) % RECORD as u64;
    if whole != len {
        f.set_len(whole)?;
    }
    if whole >= HEADER + RECORD as u64 {
        let mut last = [0u8; 4];
        f.seek(SeekFrom::Start(whole - RECORD as u64))?;
        f.read_exact(&mut last)?;
        let last_hour = u32::from_le_bytes(last);
        if last_hour == h.hour {
            f.seek(SeekFrom::Start(whole - RECORD as u64))?;
            return f.write_all(&h.encode());
        }
        if last_hour > h.hour {
            // 時計が戻ったときなど。古い時を後ろに足すと順番が崩れるので書かない
            return Ok(());
        }
    }
    f.seek(SeekFrom::Start(whole))?;
    f.write_all(&h.encode())
}

/// 書き出す前の 1 時間分 (端数を f64 で持つ)
#[derive(Clone, Debug, Default)]
struct Acc {
    hour: u32,
    use_secs: f64,
    drain: f64,
    gap: f64,
    charge_secs: f64,
    gain: f64,
    power_secs: f64,
    /// 放電量 × 秒 (mW·s)
    power_energy: f64,
    power_drain: f64,
}

impl Acc {
    fn new(hour: u32) -> Acc {
        Acc { hour, ..Default::default() }
    }

    fn record(&self) -> Hour {
        let u16s = |v: f64| v.round().clamp(0.0, u16::MAX as f64) as u16;
        let milli = |v: f64| u16s(v * 1000.0);
        Hour {
            hour: self.hour,
            use_secs: u16s(self.use_secs),
            drain_m: milli(self.drain),
            gap_m: milli(self.gap),
            charge_secs: u16s(self.charge_secs),
            gain_m: milli(self.gain),
            power_cw: if self.power_secs > 0.0 { u16s(self.power_energy / self.power_secs * 100.0) } else { 0 },
            power_secs: u16s(self.power_secs),
            power_drain_m: milli(self.power_drain),
        }
    }

    fn is_empty(&self) -> bool {
        self.use_secs == 0.0 && self.gap == 0.0 && self.charge_secs == 0.0
    }
}

#[derive(Clone, Copy, Debug)]
struct Obs {
    t: u64,
    percent: i32,
    charging: bool,
}

/// 常駐プロセスが毎回の読み取りを渡して、1 時間ごとの統計を積み上げる
pub struct Recorder {
    dir: PathBuf,
    last: HashMap<String, Obs>,
    open: HashMap<String, Acc>,
    flushed_at: HashMap<String, u64>,
    /// 統計ファイルが無かったデバイスは、最初に history.csv (直近 7 日) から作る
    seeded: HashSet<String>,
}

impl Default for Recorder {
    fn default() -> Self {
        Recorder::new(dir())
    }
}

impl Recorder {
    pub fn new(dir: PathBuf) -> Recorder {
        Recorder { dir, last: HashMap::new(), open: HashMap::new(), flushed_at: HashMap::new(), seeded: HashSet::new() }
    }

    pub fn observe(&mut self, list: &[Battery], t: u64) {
        for b in list.iter().filter(|b| b.connected && !b.asleep) {
            let key = b.device_name.to_ascii_lowercase();
            if self.seeded.insert(key.clone()) {
                if !path_in(&self.dir, &key).exists() {
                    self.seed(&key, &crate::history::path());
                }
                log_summary(&path_in(&self.dir, &key), &key, t);
            }
            self.step(&key, Obs { t, percent: b.percentage, charging: b.charging }, b.power_mw.filter(|_| !b.charging));
            if t >= self.flushed_at.get(&key).copied().unwrap_or(0) + FLUSH_SECS {
                self.flush(&key, t);
            }
        }
    }

    /// 履歴 (CSV) から統計を作る (初回だけ)
    pub fn seed(&mut self, key: &str, history: &Path) {
        let samples = crate::history::load_all(history).remove(key).unwrap_or_default();
        if samples.is_empty() {
            return;
        }
        for s in &samples {
            self.step(key, Obs { t: s.t, percent: s.percent, charging: s.charging }, None);
        }
        let last_t = samples.last().map(|s| s.t).unwrap_or(0);
        self.flush(key, last_t);
        crate::log::info("I038", format!("統計を履歴から作成しました: {key} ({} 件)", samples.len()));
    }

    /// 前回の読み取りから今回までを、時ごとに振り分けて足す
    fn step(&mut self, key: &str, cur: Obs, power: Option<f64>) {
        let prev = self.last.insert(key.to_string(), cur);
        let Some(prev) = prev.filter(|p| cur.t > p.t) else { return };
        let dt = cur.t - prev.t;
        if dt <= MAX_STEP_SECS && prev.charging == cur.charging {
            let change = if cur.charging { (cur.percent - prev.percent).max(0) } else { (prev.percent - cur.percent).max(0) } as f64;
            let mut a = prev.t;
            while a < cur.t {
                let b = ((a / 3600 + 1) * 3600).min(cur.t);
                let secs = (b - a) as f64;
                let part = change * secs / dt as f64;
                let acc = self.acc(key, (a / 3600) as u32);
                if let Some(acc) = acc {
                    if cur.charging {
                        acc.charge_secs += secs;
                        acc.gain += part;
                    } else {
                        acc.use_secs += secs;
                        acc.drain += part;
                        if let Some(p) = power {
                            acc.power_secs += secs;
                            acc.power_energy += p * secs;
                            acc.power_drain += part;
                        }
                    }
                }
                a = b;
            }
        } else if !prev.charging && !cur.charging && prev.percent > cur.percent {
            // 読めなかった間に減った分は、また読めた時の時に入れる
            if let Some(acc) = self.acc(key, (cur.t / 3600) as u32) {
                acc.gap += (prev.percent - cur.percent) as f64;
            }
        }
    }

    /// その時の積み上げ先。時が進んだら前の時を書き出す
    fn acc(&mut self, key: &str, hour: u32) -> Option<&mut Acc> {
        let path = path_in(&self.dir, key);
        let current = self.open.entry(key.to_string()).or_insert_with(|| Acc::new(hour));
        if hour > current.hour {
            if !current.is_empty() {
                write(&path, &current.record());
            }
            *current = Acc::new(hour);
        }
        (hour == current.hour).then_some(current)
    }

    /// 今の時の途中経過を書き出す (同じ時のレコードを上書き)
    pub fn flush(&mut self, key: &str, t: u64) {
        self.flushed_at.insert(key.to_string(), t);
        if let Some(acc) = self.open.get(key).filter(|a| !a.is_empty()) {
            write(&path_in(&self.dir, key), &acc.record());
        }
    }

    pub fn flush_all(&mut self, t: u64) {
        let keys: Vec<String> = self.open.keys().cloned().collect();
        for k in keys {
            self.flush(&k, t);
        }
    }
}

/// 統計の概要をログに書く (問い合わせ時に、どれだけ記録があって何を推定しているか分かるように)
fn log_summary(path: &Path, key: &str, now: u64) {
    let hours = load(path);
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let Some(first) = hours.first() else {
        crate::log::info("I045", format!("統計: {key} 記録なし"));
        return;
    };
    let days = now.saturating_sub(first.hour as u64 * 3600) as f64 / DAY as f64;
    let h = analyze(&hours, &[], now, crate::log::utc_offset_secs());
    let opt = |v: Option<f64>, unit: &str| v.map(|v| format!("{v:.2}{unit}")).unwrap_or_else(|| "-".into());
    crate::log::info("I042", format!(
        "統計: {key} {} 件 ({days:.1} 日分, {bytes} バイト) / 推定容量 {} / 使用時間 {} / 容量比 {} / 1 日の消費 {} / 充電サイクル {:.1} / 劣化の目安 {} ({:?})",
        hours.len(),
        opt(h.capacity_mwh, " mWh"),
        opt(h.full_runtime_h, " 時間"),
        opt(h.health.map(|v| v * 100.0), "%"),
        opt(h.daily_drain, "%"),
        h.cycles,
        h.degrade_at.map(|(t, _)| {
            let (y, m, d) = crate::health::ymd(t);
            format!("{y}-{m:02}-{d:02}")
        }).unwrap_or_else(|| "-".into()),
        h.degrade_at.map(|(_, b)| b),
    ));
}

fn write(path: &Path, h: &Hour) {
    let result = store(path, h);
    crate::log::problem("stats", result.err().map(|e| ("W013", format!("統計ファイルに書き込めません ({}): {e}", path.display()))), ("I039", "統計ファイルに書き込めるようになりました"));
}

// ───────────── 集計 ─────────────

/// 時刻 (unix 秒) の現地の「時」(0〜23)
fn local_hour(t: u64, offset: i64) -> usize {
    ((t as i64 + offset).rem_euclid(DAY as i64) / 3600) as usize
}

/// 現地の日の始まり (unix 秒)
fn local_day(t: u64, offset: i64) -> u64 {
    let lt = t as i64 + offset;
    (lt - lt.rem_euclid(DAY as i64) - offset) as u64
}

#[derive(Default)]
struct Sum {
    use_h: f64,
    drain: f64,
    gap: f64,
    charge_h: f64,
    gain: f64,
    /// mWh
    energy: f64,
    power_drain: f64,
}

fn sum<'a>(hours: impl Iterator<Item = &'a Hour>) -> Sum {
    let mut s = Sum::default();
    for h in hours {
        s.use_h += h.use_secs as f64 / 3600.0;
        s.drain += h.drain();
        s.gap += h.gap();
        s.charge_h += h.charge_secs as f64 / 3600.0;
        s.gain += h.gain_m as f64 / 1000.0;
        s.energy += h.power_cw as f64 / 100.0 * h.power_secs as f64 / 3600.0;
        s.power_drain += h.power_drain_m as f64 / 1000.0;
    }
    s
}

impl Sum {
    /// 使用中の 1 時間あたりの減り (%)
    fn active_rate(&self) -> Option<f64> {
        (self.drain >= MIN_DRAIN && self.use_h > 0.5).then(|| self.drain / self.use_h)
    }

    /// 推定容量 (mWh) = 使ったエネルギー ÷ 減った割合
    fn capacity(&self) -> Option<f64> {
        (self.power_drain >= MIN_DRAIN && self.energy > 0.0).then(|| self.energy / self.power_drain * 100.0)
    }
}

/// 統計から健康状態・使い方を求める。runs は統計を取り始める前の記録 (充電サイクルの合計に足す)
pub fn analyze(hours: &[Hour], runs: &[Run], now: u64, offset: i64) -> Health {
    let Some(first) = hours.first() else { return crate::health::analyze(runs, now) };
    let first_t = first.hour as u64 * 3600;
    let mut h = Health::default();

    // 充電サイクル: 全期間で減った残量の合計 ÷ 100 (統計より前の記録も足す)
    let before: f64 = runs.iter().filter(|r| r.end < first_t).map(|r| (r.start_pct - r.end_pct) as f64).sum();
    let all = sum(hours.iter());
    h.cycles = (all.drain + all.gap + before) / 100.0;

    // 最近 30 日
    let from = now.saturating_sub(RECENT_DAYS * DAY);
    let recent_hours: Vec<&Hour> = hours.iter().filter(|x| x.hour as u64 * 3600 >= from).collect();
    let recent = sum(recent_hours.iter().copied());
    h.active_rate = recent.active_rate();
    h.full_runtime_h = h.active_rate.map(|r| 100.0 / r);
    h.capacity_mwh = recent.capacity();
    h.charge_time_h = (recent.gain >= MIN_DRAIN && recent.charge_h > 0.0).then(|| 100.0 / (recent.gain / recent.charge_h));

    // 時間帯ごとの減り方 (使っていない時間も含めた、その時間帯の 1 時間あたりの平均)
    let span_days = (now.saturating_sub(first_t.max(from)) as f64 / DAY as f64).max(0.0);
    if span_days >= 1.0 {
        if let Some(active) = h.active_rate {
            let mut use_s = [0.0f64; 24];
            let mut drain_s = [0.0f64; 24];
            for x in &recent_hours {
                let slot = local_hour(x.hour as u64 * 3600, offset);
                use_s[slot] += x.use_secs as f64 / 3600.0;
                drain_s[slot] += x.drain();
            }
            let idle_h = (span_days * 24.0 - recent.use_h).max(1.0);
            let idle_rate = recent.gap / idle_h;
            let mut profile = [0.0f64; 24];
            for s in 0..24 {
                let rate = if use_s[s] >= 0.5 && drain_s[s] > 0.0 { drain_s[s] / use_s[s] } else { active };
                let in_use = (use_s[s] / span_days).min(1.0);
                profile[s] = rate * in_use + idle_rate * (1.0 - in_use);
            }
            let daily: f64 = profile.iter().sum();
            if daily > 0.0 {
                h.profile = Some(profile);
                h.daily_drain = Some(daily);
            }
        }
    }

    // 1 日ごとの消費 (直近 30 日)
    let today = local_day(now, offset);
    for d in (0..RECENT_DAYS).rev() {
        let start = today.saturating_sub(d * DAY);
        if start + DAY <= first_t {
            continue;
        }
        let total: f64 = hours.iter().filter(|x| (start..start + DAY).contains(&(x.hour as u64 * 3600))).map(|x| x.drain() + x.gap()).sum();
        h.daily.push((start, total));
    }

    // 容量の推移: 14 日ごとの区切りで、容量 (放電量が分かれば) か使用時間を比べる
    let mut windows: Vec<(f64, Option<f64>, Option<f64>, f64)> = Vec::new();
    let mut start = first_t;
    while start < now {
        let end = start + WINDOW_DAYS * DAY;
        let s = sum(hours.iter().filter(|x| (start..end).contains(&(x.hour as u64 * 3600))));
        let mid_days = ((start + end.min(now)) / 2).saturating_sub(first_t) as f64 / DAY as f64;
        windows.push((mid_days, s.capacity(), s.active_rate().map(|r| 100.0 / r), s.drain));
        start = end;
    }
    let with_runtime: Vec<_> = windows.iter().filter(|w| w.2.is_some()).collect();
    let use_capacity = !with_runtime.is_empty() && with_runtime.iter().all(|w| w.1.is_some());
    let points: Vec<(f64, f64, f64)> = windows.iter().filter_map(|w| if use_capacity { w.1 } else { w.2 }.map(|v| (w.0, v, w.3))).collect();
    let total_days = now.saturating_sub(first_t) as f64 / DAY as f64;
    if points.len() >= 2 {
        let baseline = points[0].1;
        let current = points[points.len() - 1].1;
        if baseline > 0.0 {
            h.health = Some((current / baseline).min(1.0));
        }
        // 3 区切り以上あれば、重み付き (減った量) の回帰直線が 80% / 70% を切る日
        if points.len() >= 3 && baseline > 0.0 {
            let w: f64 = points.iter().map(|p| p.2).sum();
            let mx = points.iter().map(|p| p.0 * p.2).sum::<f64>() / w;
            let my = points.iter().map(|p| p.1 / baseline * p.2).sum::<f64>() / w;
            let sxx: f64 = points.iter().map(|p| p.2 * (p.0 - mx).powi(2)).sum();
            let sxy: f64 = points.iter().map(|p| p.2 * (p.0 - mx) * (p.1 / baseline - my)).sum();
            if sxx > 0.0 && sxy / sxx < 0.0 {
                let slope = sxy / sxx;
                let at = |level: f64| {
                    let day = (mx + (level - my) / slope).max(total_days);
                    first_t + (day * DAY as f64) as u64
                };
                h.degrade_at = Some((at(0.8), Basis::Trend));
                h.replace_at = Some((at(0.7), Basis::Trend));
            }
        }
    } else {
        h.need_days = Some(((2 * WINDOW_DAYS) as f64 - total_days).ceil().max(1.0) as u32);
    }

    // 傾向が出ないうちは、最近の充電ペースから一般的な寿命 (回数) に届く日
    if h.degrade_at.is_none() && span_days >= 1.0 {
        let per_day = (recent.drain + recent.gap) / 100.0 / span_days;
        if per_day > 0.0 {
            let at = |target: f64| now + (((target - h.cycles).max(0.0) / per_day) * DAY as f64) as u64;
            h.degrade_at = Some((at(CYCLES_TO_80), Basis::Cycles));
            h.replace_at = Some((at(CYCLES_TO_70), Basis::Cycles));
        }
    }
    h
}

/// 使い方 (時間帯ごとの減り方) に沿った残量の予測。(時刻, 残量) の列。0% に届くか 7 日先まで
/// current_rate があれば、今の時の残りは今の減り方で進める
pub fn forecast(h: &Health, now: u64, percent: f64, current_rate: Option<f64>, offset: i64) -> Option<Vec<(u64, f64)>> {
    let profile = h.profile?;
    let mut points = vec![(now, percent)];
    let mut t = now;
    let mut left = percent;
    while left > 0.0 && t < now + 7 * DAY {
        let next = (t / 3600 + 1) * 3600;
        let rate = if t == now { current_rate.unwrap_or(profile[local_hour(t, offset)]) } else { profile[local_hour(t, offset)] };
        let hours = (next - t) as f64 / 3600.0;
        if rate * hours >= left && rate > 0.0 {
            let end = t + (left / rate * 3600.0) as u64;
            points.push((end, 0.0));
            return Some(points);
        }
        left -= rate * hours;
        t = next;
        points.push((t, left));
    }
    Some(points)
}

/// 予測で 0% になる時刻
pub fn empty_at(points: &[(u64, f64)]) -> Option<u64> {
    points.last().filter(|p| p.1 <= 0.0).map(|p| p.0)
}
