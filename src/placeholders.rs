//! 通知内容の置換文字 ({name} など) の定義・展開・補完候補の並べ替え

use crate::ghub::Battery;
use crate::health::{self, Run};
use crate::history::{self, Sample};
use crate::i18n::{dec, f, pct, t};

/// 置換文字の定義 (補完の一覧に出す)。説明と例は表示言語で
pub struct Def {
    pub key: &'static str,
}

/// アルファベット順
pub const DEFS: &[Def] = &[
    Def { key: "bar" },
    Def { key: "battery" },
    Def { key: "cycles" },
    Def { key: "empty_at" },
    Def { key: "health" },
    Def { key: "name" },
    Def { key: "now" },
    Def { key: "pace" },
    Def { key: "power" },
    Def { key: "rate" },
    Def { key: "remain" },
    Def { key: "replace" },
    Def { key: "rn" },
    Def { key: "since" },
    Def { key: "source" },
    Def { key: "streak" },
    Def { key: "time" },
];

impl Def {
    /// どういう値か
    pub fn desc(&self) -> &'static str {
        let t = t();
        match self.key {
            "bar" => t.ph_bar,
            "battery" => t.ph_battery,
            "cycles" => t.ph_cycles,
            "empty_at" => t.ph_empty_at,
            "health" => t.ph_health,
            "name" => t.ph_name,
            "now" => t.ph_now,
            "pace" => t.ph_pace,
            "power" => t.ph_power,
            "rate" => t.ph_rate,
            "remain" => t.ph_remain,
            "replace" => t.ph_replace,
            "rn" => t.ph_rn,
            "since" => t.ph_since,
            "source" => t.ph_source,
            "streak" => t.ph_streak,
            "time" => t.ph_time,
            _ => "",
        }
    }

    /// 例 (実際の値が出せないときに見せる)
    pub fn sample(&self) -> String {
        let t = t();
        // 例の時刻: 18:05 と 21:40 (UTC として作り、ずれ 0 で表示)
        let (now, later) = (18 * 3600 + 5 * 60, 21 * 3600 + 40 * 60);
        match self.key {
            "bar" => bar_text(20),
            "battery" => "20".into(),
            "cycles" => f(t.cycles_fmt, &[&dec(7.9, 1)]),
            "empty_at" => f(t.around_time_fmt, &[&clock_text(later, now, 0)]),
            "health" => pct(97),
            "name" => "PRO 2 LIGHTSPEED".into(),
            "now" => "18".into(),
            "pace" => t.pace_faster.into(),
            "power" => format!("{} mW", dec(14.7, 1)),
            "rate" => rate_text(5.3),
            "remain" => f(t.about_fmt, &[&duration_text(3 * 3600)]),
            "replace" => month_year_text(2027, 6),
            "rn" => t.rn_sample.into(),
            "since" => duration_text(9 * 3600),
            "source" => t.source_hid.into(),
            "streak" => duration_text(3 * 86400),
            "time" => clock_text(now, now, 0),
            _ => String::new(),
        }
    }
}

/// 値が分からないとき
fn unknown() -> String {
    t().unknown.to_string()
}

/// 「約2027年6月」などの年月
pub fn month_year_text(y: i64, m: u32) -> String {
    let t = t();
    f(t.month_year_fmt, &[&y, &t.months[(m.clamp(1, 12) - 1) as usize]])
}

/// 消費ペース「-5.3%/時」
pub fn rate_text(r: f64) -> String {
    format!("-{}{}", dec(r, 1), t().per_hour)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pace {
    Faster,
    Usual,
    Slower,
}

/// 置換に使う値 (通知を出す時点の状態から作る)
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub name: String,
    pub threshold: Option<i32>,
    pub now_pct: Option<i32>,
    pub remain_h: Option<f64>,
    pub empty_at: Option<u64>,
    /// 1 時間あたりの減少 % (正の値)
    pub rate: Option<f64>,
    pub power_mw: Option<f64>,
    pub source: Option<&'static str>,
    pub time: u64,
    pub since_secs: Option<u64>,
    pub health: Option<f64>,
    pub cycles: Option<f64>,
    pub replace_at: Option<u64>,
    pub pace: Option<Pace>,
    pub streak_secs: Option<u64>,
    /// 現地時刻の UTC からのずれ (秒)
    pub utc_offset: i64,
}

impl Context {
    pub fn build(b: &Battery, threshold: Option<i32>, samples: &[Sample], runs: &[Run], hours: &[crate::stats::Hour], now: u64) -> Context {
        let h = crate::stats::analyze(hours, runs, now, crate::log::utc_offset_secs());
        Context::build_with(b, threshold, samples, runs, &h, !hours.is_empty(), now)
    }

    /// 統計の計算結果 (h) を渡して作る (設定画面は計算結果を使い回す)
    pub fn build_with(b: &Battery, threshold: Option<i32>, samples: &[Sample], runs: &[Run], h: &health::Health, has_stats: bool, now: u64) -> Context {
        let trend = history::trend(samples, now, b);
        let offset = crate::log::utc_offset_secs();
        let connected = b.connected && !b.asleep;
        // 0% になる時刻: 使い方の統計があればそれに沿って、なければ今の減り方で
        let empty_at = if connected && !b.charging {
            crate::stats::forecast(h, now, b.percentage as f64, trend.drain_per_hour, offset)
                .and_then(|p| crate::stats::empty_at(&p))
                .or_else(|| trend.empty_at(now, b.percentage))
        } else {
            None
        };

        // 最後に充電してからの時間: 履歴の中で最後に充電していた時刻から
        let since_secs = samples.iter().rev().find(|s| s.charging).map(|s| now.saturating_sub(s.t)).filter(|_| !b.charging);

        // いつもの減り方 = 統計の使用中の減り (なければ記録した放電区間の中央値)。今のペースと比べる
        let mut usual: Vec<f64> = runs.iter().map(|r| 100.0 / r.full_runtime_h()).filter(|v| v.is_finite() && *v > 0.0).collect();
        usual.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let typical = h.active_rate.or_else(|| usual.get(usual.len() / 2).copied());
        let pace = match (trend.drain_per_hour, typical) {
            (Some(cur), Some(typ)) if !b.charging => Some(if cur > typ * 1.15 {
                Pace::Faster
            } else if cur < typ * 0.85 {
                Pace::Slower
            } else {
                Pace::Usual
            }),
            _ => None,
        };

        // 充電せずに使い続けた最長記録: 記録した放電区間と、今の区間のうち長い方
        let streak_secs = runs.iter().map(|r| (r.hours * 3600.0) as u64).chain(since_secs).max();

        Context {
            name: b.label().to_string(),
            threshold,
            now_pct: connected.then_some(b.percentage),
            remain_h: b.remaining_hours.filter(|_| connected && !b.charging),
            empty_at,
            rate: trend.drain_per_hour,
            power_mw: b.power_mw.filter(|_| connected && !b.charging),
            source: b.source.map(|s| s.label()),
            time: now,
            since_secs,
            health: h.health,
            cycles: (!runs.is_empty() || has_stats).then_some(h.cycles),
            replace_at: h.replace_at.map(|(t, _)| t),
            pace,
            streak_secs,
            utc_offset: offset,
        }
    }

    /// 1 つの置換文字の値。知らない名前なら None
    pub fn value(&self, key: &str) -> Option<String> {
        Some(match key {
            "name" => self.name.clone(),
            "battery" => self.threshold.map(|v| v.to_string()).unwrap_or_else(unknown),
            "rn" => "\r\n".into(),
            "now" => self.now_pct.map(|v| v.to_string()).unwrap_or_else(unknown),
            "remain" => self.remain_h.map(|h| f(t().about_fmt, &[&duration_text((h * 3600.0) as u64)])).unwrap_or_else(unknown),
            "empty_at" => self.empty_at.map(|at| f(t().around_time_fmt, &[&clock_text(at, self.time, self.utc_offset)])).unwrap_or_else(unknown),
            "rate" => self.rate.map(rate_text).unwrap_or_else(unknown),
            "power" => self.power_mw.map(|p| format!("{} mW", dec(p, 1))).unwrap_or_else(unknown),
            "source" => self.source.map(str::to_string).unwrap_or_else(unknown),
            "time" => clock_text(self.time, self.time, self.utc_offset),
            "since" => self.since_secs.map(duration_text).unwrap_or_else(unknown),
            "health" => self.health.map(|h| pct(format!("{:.0}", h * 100.0))).unwrap_or_else(unknown),
            "cycles" => self.cycles.map(|c| f(t().cycles_fmt, &[&dec(c, 1)])).unwrap_or_else(unknown),
            "replace" => self
                .replace_at
                .map(|at| {
                    let (y, m) = health::year_month(at);
                    month_year_text(y, m)
                })
                .unwrap_or_else(unknown),
            "bar" => self.now_pct.map(bar_text).unwrap_or_else(unknown),
            "pace" => match self.pace {
                Some(Pace::Faster) => t().pace_faster.into(),
                Some(Pace::Usual) => t().pace_usual.into(),
                Some(Pace::Slower) => t().pace_slower.into(),
                None => unknown(),
            },
            "streak" => self.streak_secs.map(duration_text).unwrap_or_else(unknown),
            _ => return None,
        })
    }
}

/// "{name} の残量 {now}%" のような文を展開する。知らない {xxx} はそのまま残す
pub fn expand(template: &str, ctx: &Context) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if ctx.value(&after[..close]).is_some() => {
                out.push_str(&ctx.value(&after[..close]).unwrap_or_default());
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 残量のバー (10 マス)
pub fn bar_text(pct: i32) -> String {
    let filled = ((pct.clamp(0, 100) + 5) / 10) as usize;
    // 游ゴシックにもある ■□ を使う (▰▱ は設定画面で豆腐になる)
    "■".repeat(filled) + &"□".repeat(10 - filled)
}

/// 秒数を「45分 / 9時間 / 3日」に
pub fn duration_text(secs: u64) -> String {
    let h = secs as f64 / 3600.0;
    let t = t();
    if h < 1.0 {
        f(t.minutes_fmt, &[&(secs / 60).max(1)])
    } else if h < 48.0 {
        f(t.hours_fmt, &[&format!("{h:.0}")])
    } else {
        f(t.days_fmt, &[&format!("{:.0}", h / 24.0)])
    }
}

/// 時刻を「21:40」(今日)・「明日 6:10」・「10/25 21:40」に
pub fn clock_text(at: u64, now: u64, utc_offset: i64) -> String {
    let local = |u: u64| u as i64 + utc_offset;
    let (lt, ln) = (local(at), local(now));
    let (day_t, day_n) = (lt.div_euclid(86400), ln.div_euclid(86400));
    let secs = lt.rem_euclid(86400);
    let t = crate::i18n::t();
    let (h, min) = (secs / 3600, secs % 3600 / 60);
    let hm = if crate::i18n::style().clock_12h {
        let h12 = if h % 12 == 0 { 12 } else { h % 12 };
        format!("{h12}:{min:02} {}", if h < 12 { t.am } else { t.pm })
    } else {
        format!("{h}:{min:02}")
    };
    match day_t - day_n {
        0 => hm,
        1 => f(t.tomorrow_fmt, &[&hm]),
        _ => {
            let (_, m, d) = health::ymd(lt.max(0) as u64);
            format!("{} {hm}", f(t.date_md_fmt, &[&m, &d]))
        }
    }
}

/// 補完候補: 入力に近い順 (前方一致 → 部分一致 → 説明に含む → 順番どおりに含む → その他)、同じなら名前順
pub fn candidates(query: &str) -> Vec<&'static Def> {
    let q = query.to_lowercase();
    let mut list: Vec<(u8, usize, &'static Def)> = DEFS
        .iter()
        .map(|d| {
            let tier = if q.is_empty() || d.key.starts_with(&q) {
                0
            } else if d.key.contains(&q) {
                1
            } else if d.desc().contains(query) {
                2
            } else if is_subsequence(&q, d.key) {
                3
            } else {
                4
            };
            (tier, if q.is_empty() { 0 } else { levenshtein(&q, d.key) }, d)
        })
        .collect();
    list.sort_by(|a, b| (a.0, a.1, a.2.key).cmp(&(b.0, b.1, b.2.key)));
    list.into_iter().map(|(_, _, d)| d).collect()
}

fn is_subsequence(q: &str, key: &str) -> bool {
    let mut it = key.chars();
    q.chars().all(|c| it.any(|k| k == c))
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (row[j + 1] + 1).min(row[j] + 1).min(prev + usize::from(ca != *cb));
            prev = cur;
        }
    }
    row[b.len()]
}
