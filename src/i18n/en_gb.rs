//! 英語 (英国): 綴りと日付・時刻の書き方だけ米国と変える

use super::{Style, Texts};

pub static T: Texts = Texts {
    style: Style { clock_12h: false, ..super::en_us::C.style },
    date_md_fmt: "{1}/{0}",
    month_year_fmt: "around {1} {0}",
    colors_title: "Level colours",
    colors_info: "Colours of the tray icon and window by battery level. From the top, each step means \"this colour at or below this %\"; the last step covers up to 100%.\nClick a swatch to change it.",
    notify_info: "Holds notifications whilst a full-screen app is running and shows them together afterwards.\nWebhooks are sent right away.",
    ..super::en_us::C
};
