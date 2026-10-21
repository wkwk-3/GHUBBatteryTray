// exe にアプリ名・アイコンを埋め込む (Windows の通知やエクスプローラーに表示される)
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico")
            .set("FileDescription", "GHUB Battery")
            .set("ProductName", "GHUB Battery")
            .set("OriginalFilename", "GHUBBatteryTray.exe");
        res.compile().expect("リソースの埋め込みに失敗しました");
    }
}
