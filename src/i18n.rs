//! UI language (English by default). `tr!` and `trf!` pick the Czech or English text.
use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(true);

pub fn set_english(en: bool) {
    ENGLISH.store(en, Ordering::Relaxed);
}

pub fn is_en() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// `tr!("czech", "english")` -> &'static str for the current language.
#[macro_export]
macro_rules! tr {
    ($cs:expr, $en:expr) => {
        if $crate::i18n::is_en() {
            $en
        } else {
            $cs
        }
    };
}

/// `trf!("czech {x}", "english {x}", args...)` -> String (both templates are checked at compile time).
#[macro_export]
macro_rules! trf {
    ($cs:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_en() {
            format!($en $(, $arg)*)
        } else {
            format!($cs $(, $arg)*)
        }
    };
}
