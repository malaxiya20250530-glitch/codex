//! # codex-l10n — Lightweight localization for Codex CLI
//!
//! Provides compile-time embedded translations loaded from JSON locale files.
//! Strings are resolved at runtime via a simple key-based lookup with
//! variable substitution support.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use codex_l10n::{tr, tr_fmt, init_from_env};
//!
//! init_from_env();
//!
//! // Simple lookup — returns &str, falls back to message_id itself
//! let label = tr!("plan-mode");
//!
//! // With variable substitution
//! let msg = tr_fmt!("approval-needed-in", "thread" => thread_name);
//! ```

use std::collections::HashMap;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Translation stores
// ---------------------------------------------------------------------------

/// The active locale code (e.g. `"en-US"`, `"zh-CN"`).
static ACTIVE_LOCALE: OnceLock<String> = OnceLock::new();

/// Compiled translations for the active locale: `message_id → translated_text`.
static TRANSLATIONS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

/// Embedded locale bundles (compiled into the binary).
mod bundles {
    /// English (US) – the canonical source; IDs equal English text.
    pub(crate) const EN_US: &str = include_str!("locales/en-US.json");

    /// Simplified Chinese.
    pub(crate) const ZH_CN: &str = include_str!("locales/zh-CN.json");
}

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// Initialise the localisation subsystem from the `CODEX_LOCALE` or `LANG`
/// environment variable.  Falls back to `en-US` when neither is set or when
/// the requested locale has no bundled data.
///
/// Safe to call multiple times — only the first call takes effect.
pub fn init_from_env() {
    let lang = std::env::var("CODEX_LOCALE")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();

    // Normalise: "zh_CN.UTF-8" → "zh-CN", "en_US" → "en-US", etc.
    let lang = lang
        .split('.')
        .next()
        .unwrap_or("en-US")
        .replace('_', "-");

    let locale = match lang.as_str() {
        // Map zh variants to zh-CN
        l if l.starts_with("zh") => "zh-CN",
        // Add more mappings here in future PRs:
        // l if l.starts_with("ja") => "ja-JP",
        // l if l.starts_with("ko") => "ko-KR",
        _ => "en-US",
    };

    init(locale);
}

/// Initialise with an explicit locale code (e.g. `"zh-CN"`).
pub fn init(locale: &str) {
    ACTIVE_LOCALE.set(locale.to_owned()).ok();

    let raw: &str = match locale {
        "zh-CN" => bundles::ZH_CN,
        _ => bundles::EN_US, // en-US is the fallback
    };

    let map: HashMap<&str, &str> = serde_json::from_str(raw)
        .expect("Invalid embedded locale JSON");

    // Leak the HashMap so it lives for the program's lifetime as `&'static`.
    let leaked: &'static HashMap<&'static str, &'static str> = Box::leak(Box::new(
        map.into_iter().map(|(k, v)| {
            // Leak each key and value too.
            let k: &'static str = Box::leak(k.into_boxed_str());
            let v: &'static str = Box::leak(v.into_boxed_str());
            (k, v)
        }).collect(),
    ));

    TRANSLATIONS.set(leaked).ok();
}

/// Return the active locale string (e.g. `"zh-CN"`), or `"en-US"` if not
/// initialised.
pub fn active_locale() -> &'static str {
    ACTIVE_LOCALE.get().map(|s| s.as_str()).unwrap_or("en-US")
}

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

/// Look up a translated string by its message ID.
///
/// Returns the translated text when the ID exists in the active locale,
/// otherwise returns `id` itself (English = identity).
pub fn lookup(id: &str) -> &str {
    if let Some(map) = TRANSLATIONS.get() {
        if let Some(translated) = map.get(id) {
            return translated;
        }
    }
    // Fallback: the ID is the English text.
    id
}

/// Look up a translated string and substitute positional `{var}` placeholders.
///
/// Each entry in `args` is a `(key, value)` pair.  Every occurrence of
/// `{key}` in the translated text is replaced with `value`.
pub fn lookup_with_args(id: &str, args: &[(&str, &str)]) -> String {
    let mut s = lookup(id).to_string();
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------

/// Look up a translated string by its message ID.
///
/// ```
/// use codex_l10n::tr;
/// let label = tr!("plan-mode");
/// ```
#[macro_export]
macro_rules! tr {
    ($id:literal) => {
        $crate::lookup($id)
    };
}

/// Look up a translated string and substitute `{var}` placeholders.
///
/// ```
/// use codex_l10n::tr_fmt;
/// let msg = tr_fmt!("approval-needed-in", "thread" => "main");
/// ```
#[macro_export]
macro_rules! tr_fmt {
    ($id:literal $(, $key:expr => $val:expr)* $(,)?) => {{
        let args: &[(&str, &str)] = &[$((stringify!($key), $val),)*];
        $crate::lookup_with_args($id, args)
    }};
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_en_us_fallback() {
        // Without init, lookup returns the ID itself.
        assert_eq!(lookup("plan-mode"), "plan-mode");
    }

    #[test]
    fn test_zh_cn_lookup() {
        init("zh-CN");
        assert_eq!(lookup("plan-mode"), "计划模式");
        assert_eq!(lookup("sandbox-type"), "沙箱");
    }

    #[test]
    fn test_args_substitution() {
        init("zh-CN");
        let msg = lookup_with_args("approval-needed-in", &[("thread", "test" )]);
        assert_eq!(msg, "test 需要审批");
    }

    #[test]
    fn test_missing_key_falls_back() {
        init("zh-CN");
        // Keys not in zh-CN fall back to the key itself.
        assert_eq!(lookup("nonexistent-key"), "nonexistent-key");
    }

    #[test]
    fn test_macro_tr() {
        init("zh-CN");
        assert_eq!(tr!("plan-mode"), "计划模式");
    }

    #[test]
    fn test_macro_tr_fmt() {
        init("zh-CN");
        let msg = tr_fmt!("approval-needed-in", "thread" => "session-1");
        assert_eq!(msg, "session-1 需要审批");
    }
}
