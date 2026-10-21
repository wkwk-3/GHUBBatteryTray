//! 通知を Webhook (Discord / Slack / 汎用 JSON) に送る。
//! HTTP は Windows 標準の curl.exe に任せ、常駐プロセスに TLS ライブラリを持たない。

use crate::settings::{Settings, WebhookFormat};
use serde_json::json;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Message {
    pub title: String,
    pub text: String,
    pub device: String,
    pub battery: i32,
    /// "low_battery" / "full_charge" / "test"
    pub event: &'static str,
}

fn payload(format: WebhookFormat, m: &Message) -> String {
    match format {
        WebhookFormat::Discord => json!({ "username": "GHUB Battery", "content": format!("**{}**\n{}", m.title, m.text) }),
        WebhookFormat::Slack => json!({ "text": format!("*{}*\n{}", m.title, m.text) }),
        WebhookFormat::Json => json!({ "title": m.title, "message": m.text, "device": m.device, "battery": m.battery, "event": m.event }),
    }
    .to_string()
}

/// 有効なら別スレッドで送る (結果はログへ)
pub fn send_if_enabled(settings: &Settings, m: Message) {
    if !settings.webhook_enabled || settings.webhook_url.trim().is_empty() {
        return;
    }
    let url = settings.webhook_url.trim().to_string();
    let format = settings.webhook_format;
    std::thread::spawn(move || {
        // URL にはトークンが含まれるのでログに書かない
        match send(&url, format, &m) {
            Ok(code) => crate::log::info(format!("Webhook を送信しました ({}, HTTP {code})", m.event)),
            Err(e) => crate::log::error(format!("Webhook の送信に失敗しました ({}): {e}", m.event)),
        }
    });
}

/// 同期で送る。HTTP ステータスが 2xx 以外ならエラー
pub fn send(url: &str, format: WebhookFormat, m: &Message) -> Result<u16, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("URL は http:// または https:// で始めてください".into());
    }
    let mut child = Command::new("curl.exe")
        .args(["-s", "-o", "NUL", "-w", "%{http_code}", "-m", "10", "-X", "POST", "-H", "Content-Type: application/json", "--data-binary", "@-", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("curl.exe を起動できません: {e}"))?;

    child.stdin.take().ok_or("stdin")?.write_all(payload(format, m).as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let code: u16 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
    if (200..300).contains(&code) { Ok(code) } else if code == 0 { Err("接続できませんでした".into()) } else { Err(format!("HTTP {code}")) }
}
