//! Interface languages: Russian, English, Ukrainian.
//! Strings live next to the code as `tr!("ru", "en", "uk")` or `trf!(["ru {}", "en {}", "uk {}"], args)`.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Ru,
    En,
    Uk,
}

pub const ALL: [Lang; 3] = [Lang::Ru, Lang::En, Lang::Uk];

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Ru => "ru",
            Lang::En => "en",
            Lang::Uk => "uk",
        }
    }

    pub fn from_code(s: &str) -> Option<Lang> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ru" => Some(Lang::Ru),
            "en" => Some(Lang::En),
            "uk" | "ua" => Some(Lang::Uk),
            _ => None,
        }
    }

    /// Name of the language in that language.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::Ru => "Русский",
            Lang::En => "English",
            Lang::Uk => "Українська",
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::En,
        2 => Lang::Uk,
        _ => Lang::Ru,
    }
}

pub fn set(l: Lang) {
    LANG.store(
        match l {
            Lang::Ru => 0,
            Lang::En => 1,
            Lang::Uk => 2,
        },
        Ordering::Relaxed,
    );
}

pub fn pick(ru: &'static str, en: &'static str, uk: &'static str) -> &'static str {
    match lang() {
        Lang::Ru => ru,
        Lang::En => en,
        Lang::Uk => uk,
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

/// Best guess from the Windows display language (used to preselect the first-run choice).
pub fn system() -> Lang {
    let id = unsafe { GetUserDefaultUILanguage() } & 0x3ff;
    match id {
        0x19 => Lang::Ru,
        0x22 => Lang::Uk,
        _ => Lang::En,
    }
}

#[macro_export]
macro_rules! tr {
    ($ru:literal, $en:literal, $uk:literal $(,)?) => {
        $crate::i18n::pick($ru, $en, $uk)
    };
}

#[macro_export]
macro_rules! trf {
    ([$ru:literal, $en:literal, $uk:literal] $($rest:tt)*) => {
        match $crate::i18n::lang() {
            $crate::i18n::Lang::Ru => format!($ru $($rest)*),
            $crate::i18n::Lang::En => format!($en $($rest)*),
            $crate::i18n::Lang::Uk => format!($uk $($rest)*),
        }
    };
}
