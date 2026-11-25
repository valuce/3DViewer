//! Tiny two-language UI localisation (Russian / English).

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lang {
    Ru,
    En,
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_lang(l: Lang) {
    LANG.store(l as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == Lang::En as u8 { Lang::En } else { Lang::Ru }
}

/// Language of the operating system UI: Russian for ru/uk/be locales, English otherwise.
pub fn detect() -> Lang {
    let loc = sys_locale::get_locale().unwrap_or_default().to_ascii_lowercase();
    if loc.starts_with("ru") || loc.starts_with("uk") || loc.starts_with("be") || loc.starts_with("kk") { Lang::Ru } else { Lang::En }
}

/// Pick the string for the current language.
#[inline]
pub fn tr(ru: &'static str, en: &'static str) -> &'static str {
    match lang() {
        Lang::Ru => ru,
        Lang::En => en,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching() {
        set_lang(Lang::En);
        assert_eq!(tr("Файл", "File"), "File");
        set_lang(Lang::Ru);
        assert_eq!(tr("Файл", "File"), "Файл");
    }
}
