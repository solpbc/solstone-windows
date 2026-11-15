// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Device-side presentation. The OS seam supplies preferences; this pure module
//! negotiates only the locales admitted to the generated production bundle.

#[allow(dead_code, clippy::all)]
#[rustfmt::skip]
mod generated {
    include!("../i18n/generated/rust/windows/api.rs");
}
pub use generated::Value;
use std::sync::OnceLock;

include!("../i18n/locales.rs");
static LOCALE: OnceLock<String> = OnceLock::new();

pub fn negotiate(preferred: &[String], supported: &[&str]) -> String {
    use fluent_langneg::{negotiate_languages, NegotiationStrategy};
    use unic_langid::LanguageIdentifier;
    let requested: Vec<LanguageIdentifier> = preferred
        .iter()
        .filter_map(|tag| tag.parse().ok())
        .collect();
    let available: Vec<LanguageIdentifier> = supported
        .iter()
        .filter_map(|tag| tag.parse().ok())
        .collect();
    let fallback: LanguageIdentifier = "en-US".parse().expect("source locale");
    negotiate_languages(
        &requested,
        &available,
        Some(&fallback),
        NegotiationStrategy::Lookup,
    )
    .first()
    .map_or_else(|| fallback.to_string(), |tag| tag.to_string())
}

pub fn initialize(preferred: &[String]) {
    let _ = LOCALE.set(negotiate(preferred, SUPPORTED_LOCALES));
}

pub fn locale() -> &'static str {
    LOCALE.get().map_or("en-US", String::as_str)
}

pub fn message(id: &str, args: &[(&str, Value<'_>)]) -> String {
    generated::render_dotted_with_options(locale(), id, args, locale() != "en-US")
        .unwrap_or_else(|error| panic!("invalid compiled message {id}: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::negotiate;
    fn preferences(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|tag| (*tag).to_string()).collect()
    }
    #[test]
    fn today_every_preference_falls_back_to_source() {
        for requested in [
            vec![],
            preferences(&["es-MX", "fr-FR"]),
            preferences(&["en-GB"]),
            preferences(&["bad_tag"]),
        ] {
            assert_eq!(negotiate(&requested, &["en-US"]), "en-US");
        }
    }
    #[test]
    fn supported_preferences_are_considered_in_owner_order() {
        let available = &["en-US", "es-419", "fr", "zh-Hans", "zh-Hant"];
        assert_eq!(
            negotiate(&preferences(&["de-DE", "fr-FR", "es-419"]), available),
            "fr"
        );
        assert_eq!(
            negotiate(&preferences(&["es-MX", "fr"]), available),
            "es-419"
        );
        assert_eq!(
            negotiate(&preferences(&["zh-TW", "zh-CN"]), available),
            "zh-Hant"
        );
        assert_eq!(negotiate(&preferences(&["EN-us"]), available), "en-US");
    }
}
