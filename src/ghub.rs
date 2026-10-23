//! G HUB からバッテリー情報を取得する。
//! 優先: G HUB エージェントのローカル WebSocket API (ws://localhost:9010)
//! 予備: %LOCALAPPDATA%\LGHUB\settings.db のキャッシュ

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

/// 常駐から設定画面へ送るため serde で書き出せる
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Battery {
    /// 設定のキー (settings.db の battery/{name}/percentage と同じ名前)
    pub device_name: String,
    pub display_name: String,
    pub percentage: i32,
    pub charging: bool,
    pub fully_charged: bool,
    pub connected: bool,
    /// G HUB が推定した残り使用時間 (時間)
    pub remaining_hours: Option<f64>,
    /// どこから取得したか
    pub source: Option<Source>,
    /// 無線デバイスが省電力で応答しないため、前回の値を表示している
    pub asleep: bool,
    /// 放電量 (消費電力 mW)。G HUB の推定値
    pub power_mw: Option<f64>,
    /// G HUB が推定した満充電での持ち時間 (時間)
    pub max_life_hours: Option<f64>,
    /// 充電中の残量の上がり方 (%/時)。常駐が充電中の残量の変化から求める
    pub charge_rate: Option<f64>,
    /// 電池に入っている電力 (mW、推定) = 推定容量 × 上がり方
    pub charge_power_mw: Option<f64>,
}

impl Battery {
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() { &self.device_name } else { &self.display_name }
    }

    /// "65%  ·  残り約12時間" のような状態表示
    pub fn status(&self) -> String {
        use crate::i18n::{f, t};
        let t = t();
        if !self.connected {
            return t.disconnected.into();
        }
        let mut s = crate::i18n::pct(self.percentage);
        if self.asleep {
            s += &format!(" / {}", t.asleep);
        } else if self.charging {
            s += &format!(" / {}", if self.fully_charged { t.fully_charged } else { t.charging });
            if !self.fully_charged {
                if let Some(mw) = self.charge_power_mw {
                    s += &format!(" / {}", f(t.charge_power_fmt, &[&crate::i18n::dec(mw, 0)]));
                }
                if let Some(rate) = self.charge_rate.filter(|r| *r > 0.0) {
                    let left = (100 - self.percentage).max(0) as f64 / rate;
                    let about = f(t.about_fmt, &[&crate::placeholders::duration_text((left * 3600.0) as u64)]);
                    s += &format!(" / {}", f(t.full_in_fmt, &[&about]));
                }
            }
        } else if let Some(h) = self.remaining_hours {
            let about = f(t.about_fmt, &[&crate::placeholders::duration_text((h * 3600.0) as u64)]);
            s += &format!(" / {}", f(t.remaining_fmt, &[&about]));
        }
        if !self.charging && !self.asleep {
            if let Some(mw) = self.power_mw {
                s += &format!(" / {}", f(t.power_fmt, &[&crate::i18n::dec(mw, 1)]));
            }
        }
        s
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Source {
    /// G HUB なしで HID++ 直接通信
    Hid,
    Agent,
    SettingsDb,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Hid => crate::i18n::t().source_hid,
            Source::Agent => "G HUB API",
            Source::SettingsDb => "settings.db",
        }
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub struct Agent {
    ws: WebSocket<TcpStream>,
    seq: u64,
    /// G HUB の deviceId -> バッテリー
    pub devices: BTreeMap<String, Battery>,
}

impl Agent {
    pub fn connect() -> Result<Self> {
        let stream = TcpStream::connect_timeout(&"127.0.0.1:9010".parse()?, Duration::from_secs(2))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_nodelay(true)?;

        let mut req = "ws://localhost:9010".into_client_request()?;
        req.headers_mut().insert("Origin", "file://".parse()?);
        req.headers_mut().insert("Sec-WebSocket-Protocol", "json".parse()?);

        let (ws, _) = tungstenite::client::client(req, stream).map_err(|e| format!("G HUB API handshake: {e}"))?;
        Ok(Agent { ws, seq: 0, devices: BTreeMap::new() })
    }

    fn send(&mut self, verb: &str, path: &str) -> Result<String> {
        self.seq += 1;
        let id = format!("t{}", self.seq);
        let msg = json!({ "msgId": id, "verb": verb, "path": path }).to_string();
        self.ws.send(Message::text(msg))?;
        Ok(id)
    }

    /// 1 メッセージ読む。タイムアウトなら None
    fn read(&mut self) -> Result<Option<Value>> {
        match self.ws.read() {
            Ok(Message::Text(t)) => Ok(Some(serde_json::from_str(t.as_str())?)),
            Ok(Message::Binary(b)) => Ok(Some(serde_json::from_slice(&b)?)),
            Ok(Message::Close(_)) => Err("G HUB API の接続が閉じられました".into()),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn request(&mut self, verb: &str, path: &str) -> Result<Option<Value>> {
        let id = self.send(verb, path)?;
        loop {
            let Some(msg) = self.read()? else { return Err(format!("G HUB API {path} タイムアウト").into()) };
            if msg["msgId"].as_str() != Some(id.as_str()) {
                continue;
            }
            return Ok(if msg["result"]["code"] == "SUCCESS" { Some(msg["payload"].clone()) } else { None });
        }
    }

    /// デバイス一覧とバッテリー状態をすべて取り直す
    pub fn refresh_all(&mut self) -> Result<()> {
        let list = self.request("GET", "/devices/list")?.ok_or("devices/list failed")?;
        let mut devices = BTreeMap::new();

        for d in list["deviceInfos"].as_array().into_iter().flatten() {
            if d["capabilities"]["hasBatteryStatus"] != true {
                continue;
            }
            let Some(id) = d["id"].as_str() else { continue };
            let pick = |keys: &[&str]| keys.iter().filter_map(|k| d[*k].as_str()).find(|s| !s.is_empty()).unwrap_or(id).to_string();

            let mut b = Battery {
                device_name: pick(&["slotPrefix", "deviceModel"]),
                display_name: pick(&["givenName", "displayName"]),
                connected: d["state"] == "ACTIVE",
                source: Some(Source::Agent),
                ..Default::default()
            };

            match self.request("GET", &format!("/battery/{id}/state"))? {
                Some(state) => apply_state(&mut b, &state),
                None => b.connected = false,
            }
            devices.insert(id.to_string(), b);
        }

        self.devices = devices;
        Ok(())
    }

    pub fn batteries(&self) -> Vec<Battery> {
        self.devices.values().cloned().collect()
    }
}

fn apply_state(b: &mut Battery, state: &Value) {
    if let Some(p) = state["percentage"].as_i64() {
        b.percentage = p as i32;
        b.connected = true;
    }
    b.charging = state["charging"] == true;
    b.fully_charged = state["fullyCharged"] == true;
    b.remaining_hours = state["mileage"].as_f64().filter(|h| *h > 0.0);
    b.max_life_hours = state["maxLifeSpan"].as_f64().filter(|h| *h > 0.0);
    b.power_mw = (state["consumption"]["useMw"] == true).then(|| state["consumption"]["value"].as_f64()).flatten().filter(|v| *v > 0.0);
}

/// 予備ルート: settings.db に G HUB がキャッシュした残量を読む
pub fn read_settings_db() -> Result<Vec<Battery>> {
    let path = std::path::Path::new(&std::env::var("LOCALAPPDATA")?).join(r"LGHUB\settings.db");
    let conn = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let blob: Vec<u8> = conn.query_row("SELECT file FROM data LIMIT 1", [], |r| r.get(0))?;
    let root: Value = serde_json::from_slice(&blob)?;

    let mut list = Vec::new();
    for (key, value) in root.as_object().into_iter().flatten() {
        let parts: Vec<&str> = key.split('/').collect();
        if parts.len() != 3 || parts[0] != "battery" || parts[2] != "percentage" {
            continue;
        }
        if let Some(p) = value["percentage"].as_i64() {
            list.push(Battery { device_name: parts[1].into(), percentage: p as i32, connected: true, source: Some(Source::SettingsDb), ..Default::default() });
        }
    }
    Ok(list)
}

/// G HUB API から 1 回だけ取得する
pub fn fetch_agent() -> Option<Vec<Battery>> {
    let result = Agent::connect().and_then(|mut agent| agent.refresh_all().map(|_| agent.batteries()));
    match result {
        Ok(list) => {
            crate::log::problem("ghub", None, ("I003", "G HUB API に接続しました"));
            crate::log::debug("D001", format!("G HUB API: {} 台", list.len()));
            Some(list)
        }
        Err(e) => {
            // G HUB を入れていない環境では毎回失敗するので、内容が変わったときだけ書く
            crate::log::problem("ghub", Some(("W001", format!("G HUB API を使えません: {e}"))), ("", ""));
            None
        }
    }
}


