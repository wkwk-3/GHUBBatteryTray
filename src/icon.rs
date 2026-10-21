//! トレイアイコンの描画。色付きの角丸タイルに白い数字を、実サイズ (DPI 相当) で直接描く。
//! 色は設定の「残量の色」(既定: 緑 >50% / 橙 21〜50% / 赤 20%以下 / 青 充電中 / 灰 未接続)

use crate::ghub::Battery;
use crate::settings::LevelColors;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;


/// 残量の色 (設定の「残量の色」に従う)
pub fn level_color(b: Option<&Battery>, colors: &LevelColors) -> (u8, u8, u8) {
    let [r, g, bl] = match b {
        Some(b) if b.connected && b.charging => colors.charging,
        Some(b) if b.connected => colors.for_percent(b.percentage),
        _ => colors.disconnected,
    };
    (r, g, bl)
}

const FONT_FACES: [&str; 2] = ["Bahnschrift SemiBold SemiConden", "Segoe UI Semibold"];

pub fn create(b: Option<&Battery>, size: i32, colors: &LevelColors) -> HICON {
    let text = match b {
        Some(b) if b.connected => b.percentage.clamp(0, 100).to_string(),
        _ => "--".to_string(),
    };
    let (r, g, bl) = level_color(b, colors);

    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        ReleaseDC(None, screen);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        let old = SelectObject(dc, color.into());
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);

        // タイル色で塗ってから文字を描く (文字のアンチエイリアスがタイル色に馴染む)
        px.fill(u32::from_le_bytes([bl, g, r, 0]));
        draw_text(dc, &text, size);

        // 角丸の形に合わせてアルファを付ける
        let radius = size as f32 * 0.22;
        for y in 0..size {
            for x in 0..size {
                let a = rounded_coverage(x as f32 + 0.5, y as f32 + 0.5, size as f32, radius);
                let p = &mut px[(y * size + x) as usize];
                *p = (*p & 0x00FF_FFFF) | ((a * 255.0).round() as u32) << 24;
            }
        }

        SelectObject(dc, old);
        let _ = DeleteDC(dc);

        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO { fIcon: TRUE, hbmMask: mask, hbmColor: color, ..Default::default() };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        icon
    }
}

/// 角丸四角形の内側にどれだけ掛かっているか (0..1)
fn rounded_coverage(x: f32, y: f32, size: f32, r: f32) -> f32 {
    let cx = x.clamp(r, size - r);
    let cy = y.clamp(r, size - r);
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}

struct Bounds {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

/// 文字列のインク (実際に塗られる部分) の範囲をベースライン基準で測る
unsafe fn ink_bounds(dc: HDC, text: &str) -> Bounds {
    let mat = MAT2 { eM11: FIXED { fract: 0, value: 1 }, eM22: FIXED { fract: 0, value: 1 }, ..Default::default() };
    let mut pen = 0;
    let mut b = Bounds { left: i32::MAX, right: i32::MIN, top: i32::MIN, bottom: i32::MAX };
    for ch in text.encode_utf16() {
        let mut gm = GLYPHMETRICS::default();
        unsafe { GetGlyphOutlineW(dc, ch as u32, GGO_METRICS, &mut gm, 0, None, &mat) };
        b.left = b.left.min(pen + gm.gmptGlyphOrigin.x);
        b.right = b.right.max(pen + gm.gmptGlyphOrigin.x + gm.gmBlackBoxX as i32);
        b.top = b.top.max(gm.gmptGlyphOrigin.y);
        b.bottom = b.bottom.min(gm.gmptGlyphOrigin.y - gm.gmBlackBoxY as i32);
        pen += gm.gmCellIncX as i32;
    }
    b
}

unsafe fn make_font(face: &str, height: i32, width: i32) -> HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -height,
        lfWidth: width,
        lfWeight: FW_SEMIBOLD.0 as i32,
        lfQuality: ANTIALIASED_QUALITY,
        ..Default::default()
    };
    for (i, c) in face.encode_utf16().take(31).enumerate() {
        lf.lfFaceName[i] = c;
    }
    unsafe { CreateFontIndirectW(&lf) }
}

unsafe fn draw_text(dc: HDC, text: &str, size: i32) {
    unsafe {
        let face = installed_face();
        let pad_x = (size as f32 * 0.06).max(1.0);
        let pad_y = size as f32 * 0.14;
        let target_w = size as f32 - pad_x * 2.0;
        let target_h = size as f32 - pad_y * 2.0;

        // 基準フォントで数字 "0" の高さを測り、タイル高さに合わせた文字サイズを求める
        let probe = 200;
        let f = make_font(face, probe, 0);
        let old = SelectObject(dc, f.into());
        let digit = ink_bounds(dc, "0");
        let em = (probe as f32 * target_h / (digit.top - digit.bottom).max(1) as f32).round() as i32;
        SelectObject(dc, old);
        let _ = DeleteObject(f.into());

        let mut font = make_font(face, em, 0);
        let old = SelectObject(dc, font.into());
        let ink = ink_bounds(dc, text);
        let w = (ink.right - ink.left) as f32;
        if w > target_w {
            // 幅が足りないときは横方向だけ詰める
            let mut tm = TEXTMETRICW::default();
            let _ = GetTextMetricsW(dc, &mut tm);
            SelectObject(dc, old);
            let _ = DeleteObject(font.into());
            font = make_font(face, em, ((tm.tmAveCharWidth as f32) * target_w / w).floor().max(1.0) as i32);
            SelectObject(dc, font.into());
        }
        let ink = ink_bounds(dc, text);

        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        SetTextAlign(dc, TA_BASELINE | TA_LEFT);
        let x = ((size - (ink.right - ink.left)) as f32 / 2.0).round() as i32 - ink.left;
        let y = ((size - (ink.top - ink.bottom)) as f32 / 2.0).round() as i32 + ink.top;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let _ = TextOutW(dc, x, y, &wide);

        SelectObject(dc, old);
        let _ = DeleteObject(font.into());
    }
}

fn installed_face() -> &'static str {
    use std::sync::OnceLock;
    static FACE: OnceLock<&'static str> = OnceLock::new();
    FACE.get_or_init(|| {
        for face in FONT_FACES {
            let mut found = false;
            unsafe {
                let dc = GetDC(None);
                let mut lf = LOGFONTW { lfCharSet: DEFAULT_CHARSET, ..Default::default() };
                for (i, c) in face.encode_utf16().take(31).enumerate() {
                    lf.lfFaceName[i] = c;
                }
                unsafe extern "system" fn cb(_: *const LOGFONTW, _: *const TEXTMETRICW, _: u32, lp: LPARAM) -> i32 {
                    unsafe { *(lp.0 as *mut bool) = true };
                    0
                }
                EnumFontFamiliesExW(dc, &lf, Some(cb), LPARAM(&mut found as *mut bool as isize), 0);
                ReleaseDC(None, dc);
            }
            if found {
                return face;
            }
        }
        "Segoe UI"
    })
}

pub fn destroy(icon: HICON) {
    if !icon.is_invalid() {
        unsafe {
            let _ = DestroyIcon(icon);
        }
    }
}
