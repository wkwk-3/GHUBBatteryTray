//! アップデート (GitHub Releases)。
//! - 確認: 最新リリースのタグ (例 v2.2.0) を今のバージョンと比べる。起動時と 1 日 1 回 (設定でオフ可)
//! - 更新: リリースに添付した exe を 1 つダウンロード → 検証 → 今の exe を .old に改名して入れ替え → 再起動
//! - 起動時の確認: 自分の exe の SHA-256 を、GitHub がリリースの添付ファイルに付けたハッシュ値
//!   (digest) と比べる。一致しなければ起動しない
//! 通信は Webhook と同じく Windows 標準の curl.exe を使う (アプリ本体に TLS を持たない)。

use crate::{log, settings};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const REPO: &str = "wkwk-3/GHUBBatteryTray";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// 自動確認の間隔
pub const CHECK_INTERVAL_SECS: u64 = 24 * 3600;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub notes: String,
    pub page_url: String,
    pub exe_url: String,
    pub exe_size: u64,
    /// リリースノートに "SHA256: <64桁>" があれば照合に使う
    pub sha256: Option<String>,
}

impl Release {
    pub fn is_newer(&self) -> bool {
        parse_version(&self.tag).is_some_and(|v| Some(v) > parse_version(CURRENT))
    }
}

/// 確認結果の保存 (%LOCALAPPDATA%\GHUBBatteryTray\update.json)
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub last_check: u64,
    pub latest: Option<Release>,
    /// 通知を出したバージョン (同じ版で何度も通知しない)
    pub notified_tag: String,
    /// GitHub のハッシュ値と一致を確認できた exe のハッシュ値 (オフライン時に使う)
    pub verified_hash: String,
}

impl State {
    fn path() -> PathBuf {
        settings::dir().join("update.json")
    }

    pub fn load() -> State {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(settings::dir());
        let _ = std::fs::write(Self::path(), serde_json::to_string_pretty(self).unwrap_or_default());
    }

    /// 今の版より新しいリリースが分かっていれば返す
    pub fn available(&self) -> Option<&Release> {
        self.latest.as_ref().filter(|r| r.is_newer())
    }
}

/// "v2.10.0" -> (2, 10, 0)
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let s = s.split(['-', '+']).next()?;
    let mut it = s.split('.').map(|p| p.parse::<u32>());
    let major = it.next()?.ok()?;
    let minor = it.next().unwrap_or(Ok(0)).ok()?;
    let patch = it.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

fn api_url() -> String {
    // 開発・テスト用: 別の URL (ローカルの偽リリースなど) を使う
    std::env::var("GHBT_UPDATE_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"))
}

fn releases_url() -> String {
    std::env::var("GHBT_RELEASES_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases?per_page=100"))
}

fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    curl_with_timeout(60, args)
}

fn curl_with_timeout(seconds: u32, args: &[&str]) -> Result<Vec<u8>, String> {
    let timeout = seconds.to_string();
    let out = Command::new("curl.exe")
        .args(["-s", "-S", "-L", "--fail", "-m", &timeout, "-H", "User-Agent: GHUBBatteryTray", "-H", "Accept: application/vnd.github+json"])
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("curl.exe を起動できません: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.contains("404") { "リリースが見つかりません".into() } else if err.is_empty() { "通信に失敗しました".into() } else { err })
    }
}

/// GitHub のリリース情報 (JSON) を読む
pub fn parse_release(json: &serde_json::Value) -> Result<Release, String> {
    let tag = json["tag_name"].as_str().ok_or("リリース情報にタグがありません")?.to_string();
    let notes = json["body"].as_str().unwrap_or_default().to_string();
    let exe = json["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["name"].as_str().is_some_and(|n| n.to_ascii_lowercase().ends_with(".exe")))
        .max_by_key(|a| a["name"].as_str() == Some("GHUBBatteryTray.exe"))
        .ok_or("リリースに exe が添付されていません")?;
    // GitHub がリリース時に付けるハッシュ値 ("sha256:...")。無ければリリースノートの "SHA256: ..."
    let digest = exe["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).filter(|h| h.len() == 64).map(|h| h.to_ascii_lowercase());
    Ok(Release {
        sha256: digest.or_else(|| find_sha256(&notes)),
        tag,
        page_url: json["html_url"].as_str().unwrap_or_default().to_string(),
        exe_url: exe["browser_download_url"].as_str().ok_or("ダウンロード URL がありません")?.to_string(),
        exe_size: exe["size"].as_u64().unwrap_or(0),
        notes,
    })
}

/// リリースノートの "SHA256: xxxx" (64 桁の 16 進) を探す
pub fn find_sha256(notes: &str) -> Option<String> {
    let upper = notes.to_ascii_uppercase();
    let at = upper.find("SHA256").or_else(|| upper.find("SHA-256"))?;
    notes[at..].split(|c: char| !c.is_ascii_hexdigit()).find(|w| w.len() == 64).map(|h| h.to_ascii_lowercase())
}

/// 起動時の確認結果
#[derive(Debug, PartialEq)]
pub enum Integrity {
    /// GitHub のハッシュ値と一致
    Verified,
    /// 一致しない (改ざん・破損の可能性)。起動しない
    Mismatch { tag: String, expected: String, actual: String, page_url: String },
    /// 確認できなかった (オフライン・このバージョンのリリースが無いなど)。起動はする
    Unverified(String),
}

pub fn file_sha256(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("exe を読めません: {e}"))?;
    Ok(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect())
}

/// リリース一覧から、今のバージョンと同じタグのリリースを探す (v2.1 と 2.1.0 は同じ扱い)
pub fn release_for_version(list: &serde_json::Value, version: &str) -> Option<Result<Release, String>> {
    let want = parse_version(version)?;
    list.as_array()?
        .iter()
        .filter(|r| r["draft"] != true)
        .find(|r| r["tag_name"].as_str().and_then(parse_version) == Some(want))
        .map(parse_release)
}

/// 自分の exe と GitHub のハッシュ値を比べる (判定部分。通信の結果を受け取る)
pub fn judge(actual: &str, fetched: Result<serde_json::Value, String>, version: &str, cached: &str) -> Integrity {
    match fetched {
        Ok(list) => match release_for_version(&list, version) {
            Some(Ok(Release { sha256: Some(expected), tag, page_url, .. })) => {
                if expected == actual {
                    Integrity::Verified
                } else {
                    Integrity::Mismatch { tag, expected, actual: actual.to_string(), page_url }
                }
            }
            Some(Ok(r)) => Integrity::Unverified(format!("{} にハッシュ値がありません", r.tag)),
            Some(Err(e)) => Integrity::Unverified(e),
            None => Integrity::Unverified(format!("v{version} のリリースが GitHub にありません")),
        },
        // 通信できないときは、前回確認できたハッシュ値と同じなら確認済みとする
        Err(_) if !cached.is_empty() && cached == actual => Integrity::Verified,
        Err(e) => Integrity::Unverified(format!("GitHub に接続できません: {e}")),
    }
}

/// 起動前に自分の exe を確かめる
pub fn verify_self() -> Integrity {
    let actual = match std::env::current_exe().map_err(|e| e.to_string()).and_then(|p| file_sha256(&p)) {
        Ok(h) => h,
        Err(e) => return Integrity::Unverified(e),
    };
    let fetched = curl_with_timeout(10, &[&releases_url()])
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).map_err(|e| format!("リリース一覧を読めません: {e}")));
    let mut state = State::load();
    let result = judge(&actual, fetched, CURRENT, &state.verified_hash);
    if result == Integrity::Verified && state.verified_hash != actual {
        state.verified_hash = actual;
        state.save();
    }
    result
}

/// 最新リリースを確認し、結果を保存する
pub fn check() -> Result<State, String> {
    let body = curl(&[&api_url()])?;
    let json: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("リリース情報を読めません: {e}"))?;
    let release = parse_release(&json)?;
    let mut state = State::load();
    state.last_check = crate::history::now();
    log::info(format!("更新を確認しました: 最新 {} / 現在 v{CURRENT}{}", release.tag, if release.is_newer() { " (更新あり)" } else { "" }));
    state.latest = Some(release);
    state.save();
    Ok(state)
}

/// exe をダウンロードして検証し、一時ファイルのパスを返す
pub fn download(release: &Release) -> Result<PathBuf, String> {
    let dest = std::env::temp_dir().join(format!("GHUBBatteryTray-{}.exe", release.tag.trim_start_matches(['v', 'V'])));
    let _ = std::fs::remove_file(&dest);
    curl(&["-o", &dest.to_string_lossy(), &release.exe_url])?;
    verify(&dest, release).inspect_err(|_| {
        let _ = std::fs::remove_file(&dest);
    })?;
    Ok(dest)
}

/// サイズ・exe の形式・(あれば) SHA-256 を確かめる
pub fn verify(file: &Path, release: &Release) -> Result<(), String> {
    let bytes = std::fs::read(file).map_err(|e| format!("ダウンロードしたファイルを読めません: {e}"))?;
    if release.exe_size > 0 && bytes.len() as u64 != release.exe_size {
        return Err(format!("ファイルサイズが一致しません ({} / {} バイト)", bytes.len(), release.exe_size));
    }
    if !bytes.starts_with(b"MZ") {
        return Err("ダウンロードしたファイルが exe ではありません".into());
    }
    if let Some(expected) = &release.sha256 {
        let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if &actual != expected {
            return Err("SHA-256 が一致しません (ファイルが壊れているか、改ざんされています)".into());
        }
    }
    Ok(())
}

/// 今の exe を .old に改名して新しい exe を置く。失敗したら元に戻す
pub fn install(new_exe: &Path, current: &Path) -> Result<(), String> {
    let old = old_path(current);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(current, &old).map_err(|e| format!("今の exe を入れ替えられません (書き込み権限がない場所かもしれません): {e}"))?;
    if let Err(e) = std::fs::copy(new_exe, current) {
        let _ = std::fs::remove_file(current);
        let _ = std::fs::rename(&old, current);
        return Err(format!("新しい exe を置けませんでした: {e}"));
    }
    let _ = std::fs::remove_file(new_exe);
    Ok(())
}

pub fn old_path(current: &Path) -> PathBuf {
    let mut s = current.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

/// 前回の更新で残った .old を消す (起動時)
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        let old = old_path(&exe);
        if old.exists() && std::fs::remove_file(&old).is_ok() {
            log::info("更新前の exe を削除しました");
        }
    }
}

/// 更新を最後まで行う: ダウンロード → 入れ替え → 常駐を終了 → 新しい exe を起動
pub fn update_and_restart(release: &Release) -> Result<(), String> {
    log::info(format!("{} への更新を開始します", release.tag));
    let file = download(release)?;
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    install(&file, &current)?;
    log::info(format!("{} に入れ替えました。再起動します", release.tag));

    crate::tray::request_quit();
    Command::new(&current).arg("--after-update").spawn().map_err(|e| format!("新しいバージョンを起動できません: {e}"))?;
    Ok(())
}

/// ブラウザーでリリースページを開く
pub fn open_page(release: Option<&Release>) {
    let url = release.map(|r| r.page_url.clone()).filter(|u| !u.is_empty()).unwrap_or_else(|| format!("https://github.com/{REPO}/releases/latest"));
    let _ = Command::new("explorer.exe").arg(url).spawn();
}
