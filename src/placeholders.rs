//! 通知内容の置換文字 ({name} など) の定義・展開・補完候補の並べ替え

use crate::ghub::Battery;
use crate::health::{self, Run};
use crate::history::{self, Sample};

/// 置換文字の定義 (補完の一覧に出す)
pub struct Def {
    pub key: &'static str,
    /// どういう値か
    pub desc: &'static str,
    /// 例 (実際の値が出せないときに見せる)
    pub sample: &'static str,
}

/// アルファベット順
pub const DEFS: &[Def] = &[
    Def { key: "bar", desc: "残量のバー表示", sample: "■■□□□□□□□□" },
    Def { key: "battery", desc: "通知のしきい値 (%)", sample: "20" },
    Def { key: "cycles", desc: "充電サイクル", sample: "7.9回" },
    Def { key: "empty_at", desc: "0% になる予測時刻", sample: "21:40ごろ" },
    Def { key: "health", desc: "容量 (記録開始時比)", sample: "97%" },
    Def { key: "name", desc: "デバイス名", sample: "PRO 2 LIGHTSPEED" },
    Def { key: "now", desc: "現在の残量 (%)", sample: "18" },
    Def { key: "pace", desc: "平均との比較 (消費ペース)", sample: "平均より速い" },
    Def { key: "power", desc: "放電量", sample: "14.7 mW" },
    Def { key: "rate", desc: "現在の消費ペース", sample: "-5.3%/時" },
    Def { key: "remain", desc: "残り時間 (G HUB 起動時)", sample: "約3時間" },
    Def { key: "replace", desc: "買い替えの目安", sample: "2027年6月ごろ" },
    Def { key: "rn", desc: "改行", sample: "(改行)" },
    Def { key: "since", desc: "最後に充電してからの時間", sample: "9時間" },
    Def { key: "source", desc: "取得元", sample: "直接通信" },
    Def { key: "streak", desc: "充電せずに使い続けた最長記録", sample: "3日" },
    Def { key: "time", desc: "通知した時刻", sample: "18:05" },
];

/// 値が分からないとき
const UNKNOWN: &str = "不明";

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
    pub fn build(b: &Battery, threshold: Option<i32>, samples: &[Sample], runs: &[Run], now: u64) -> Context {
        let trend = history::trend(samples, now, b);
        let h = health::analyze(runs, now);
        let connected = b.connected && !b.asleep;

        // 最後に充電してからの時間: 履歴の中で最後に充電していた時刻から
        let since_secs = samples.iter().rev().find(|s| s.charging).map(|s| now.saturating_sub(s.t)).filter(|_| !b.charging);

        // いつもの減り方 = 記録した放電区間の中央値。今のペースと比べる
        let mut usual: Vec<f64> = runs.iter().map(|r| 100.0 / r.full_runtime_h()).filter(|v| v.is_finite() && *v > 0.0).collect();
        usual.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let pace = match (trend.drain_per_hour, usual.get(usual.len() / 2)) {
            (Some(cur), Some(&typ)) if !b.charging => Some(if cur > typ * 1.15 {
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
            empty_at: if connected { trend.empty_at(now, b.percentage) } else { None },
            rate: trend.drain_per_hour,
            power_mw: b.power_mw.filter(|_| connected && !b.charging),
            source: b.source.map(|s| s.label()),
            time: now,
            since_secs,
            health: h.health,
            cycles: (!runs.is_empty()).then_some(h.cycles),
            replace_at: h.replace_at.map(|(t, _)| t),
            pace,
            streak_secs,
            utc_offset: crate::log::utc_offset_secs(),
        }
    }

    /// 1 つの置換文字の値。知らない名前なら None
    pub fn value(&self, key: &str) -> Option<String> {
        let unknown = || UNKNOWN.to_string();
        Some(match key {
            "name" => self.name.clone(),
            "battery" => self.threshold.map(|v| v.to_string()).unwrap_or_else(unknown),
            "rn" => "\r\n".into(),
            "now" => self.now_pct.map(|v| v.to_string()).unwrap_or_else(unknown),
            "remain" => self.remain_h.map(|h| format!("約{}", duration_text((h * 3600.0) as u64))).unwrap_or_else(unknown),
            "empty_at" => self.empty_at.map(|t| format!("{}ごろ", clock_text(t, self.time, self.utc_offset))).unwrap_or_else(unknown),
            "rate" => self.rate.map(|r| format!("-{r:.1}%/時")).unwrap_or_else(unknown),
            "power" => self.power_mw.map(|p| format!("{p:.1} mW")).unwrap_or_else(unknown),
            "source" => self.source.map(str::to_string).unwrap_or_else(unknown),
            "time" => clock_text(self.time, self.time, self.utc_offset),
            "since" => self.since_secs.map(duration_text).unwrap_or_else(unknown),
            "health" => self.health.map(|h| format!("{:.0}%", h * 100.0)).unwrap_or_else(unknown),
            "cycles" => self.cycles.map(|c| format!("{c:.1}回")).unwrap_or_else(unknown),
            "replace" => self
                .replace_at
                .map(|t| {
                    let (y, m) = health::year_month(t);
                    format!("{y}年{m}月ごろ")
                })
                .unwrap_or_else(unknown),
            "bar" => self.now_pct.map(bar_text).unwrap_or_else(unknown),
            "pace" => match self.pace {
                Some(Pace::Faster) => "平均より速い".into(),
                Some(Pace::Usual) => "平均的".into(),
                Some(Pace::Slower) => "平均より遅い".into(),
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
    if h < 1.0 {
        format!("{}分", (secs / 60).max(1))
    } else if h < 48.0 {
        format!("{:.0}時間", h)
    } else {
        format!("{:.0}日", h / 24.0)
    }
}

/// 時刻を「21:40」(今日)・「明日 6:10」・「10/25 21:40」に
pub fn clock_text(t: u64, now: u64, utc_offset: i64) -> String {
    let local = |u: u64| u as i64 + utc_offset;
    let (lt, ln) = (local(t), local(now));
    let (day_t, day_n) = (lt.div_euclid(86400), ln.div_euclid(86400));
    let secs = lt.rem_euclid(86400);
    let hm = format!("{}:{:02}", secs / 3600, secs % 3600 / 60);
    match day_t - day_n {
        0 => hm,
        1 => format!("明日 {hm}"),
        _ => {
            let (_, m, d) = health::ymd(lt.max(0) as u64);
            format!("{m}/{d} {hm}")
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
            } else if d.desc.contains(query) {
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
