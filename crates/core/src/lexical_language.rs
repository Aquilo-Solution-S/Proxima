//! Per-row lexical language resolution for memory writes.
//!
//! The lexical language of a memory (the `PostgreSQL` text-search
//! configuration its stored vector tokenises with) is decided once, at
//! write time, with this precedence:
//!
//! 1. an explicit configuration name from the caller,
//! 2. `"auto"`: reliable detection from the content itself,
//! 3. neither: `None`, and storage applies the database default
//!    (`proxima_core.lexical_config()`).
//!
//! Detection is deliberately gated on the detector's own reliability
//! signal. Measured on real corpora (2,350 German book pages, 130 short
//! German questions, 300 English documentation paragraphs), gated
//! detection is ≥98% accurate in every slice including 40-character
//! truncations, while *ungated* detection under ~80 characters is
//! 50–83% — worse than useless, since a wrongly stamped language makes
//! the row unmatchable by its own content words. An unreliable
//! detection therefore falls back to the database default rather than
//! guessing.
//!
//! A reliably detected language with no shipped stemmer configuration
//! (CJK, most Slavic and Indic languages) maps to `simple`: exact-token
//! matching without wrong-language stemming is the best `PostgreSQL` can
//! do there, and strictly better than stemming Chinese with the English
//! Snowball rules.

/// Sentinel accepted by write surfaces to request content detection.
pub const LEXICAL_LANGUAGE_AUTO: &str = "auto";

/// Rejected `language` argument on a memory write surface.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "invalid lexical language {supplied:?}: pass a PostgreSQL text-search configuration name \
     (e.g. 'english', 'german', optionally schema-qualified), or 'auto' to detect from content"
)]
pub struct InvalidLexicalLanguage {
    pub supplied: String,
}

/// The text-search configuration shipped by `PostgreSQL` for a detected
/// language, if any.
///
/// Covers every whatlang-detectable language with a `pg_catalog`
/// configuration (PG 18 ships 30). Languages without one return `None`
/// and are handled by the caller (`simple`).
#[must_use]
fn shipped_config_for(lang: whatlang::Lang) -> Option<&'static str> {
    use whatlang::Lang;
    match lang {
        Lang::Ara => Some("arabic"),
        Lang::Hye => Some("armenian"),
        Lang::Cat => Some("catalan"),
        Lang::Dan => Some("danish"),
        Lang::Nld => Some("dutch"),
        Lang::Eng => Some("english"),
        Lang::Est => Some("estonian"),
        Lang::Fin => Some("finnish"),
        Lang::Fra => Some("french"),
        Lang::Deu => Some("german"),
        Lang::Ell => Some("greek"),
        Lang::Hin => Some("hindi"),
        Lang::Hun => Some("hungarian"),
        Lang::Ind => Some("indonesian"),
        Lang::Ita => Some("italian"),
        Lang::Lit => Some("lithuanian"),
        Lang::Nep => Some("nepali"),
        Lang::Nob => Some("norwegian"),
        Lang::Por => Some("portuguese"),
        Lang::Ron => Some("romanian"),
        Lang::Rus => Some("russian"),
        Lang::Srp => Some("serbian"),
        Lang::Spa => Some("spanish"),
        Lang::Swe => Some("swedish"),
        Lang::Tam => Some("tamil"),
        Lang::Tur => Some("turkish"),
        Lang::Yid => Some("yiddish"),
        _ => None,
    }
}

/// Detect the lexical configuration for a text, or `None` when the
/// detection is not reliable enough to act on.
#[must_use]
pub fn detect_lexical_language(text: &str) -> Option<&'static str> {
    let info = whatlang::detect(text)?;
    if !info.is_reliable() {
        return None;
    }
    Some(shipped_config_for(info.lang()).unwrap_or("simple"))
}

/// Resolve a write surface's optional `language` argument to the
/// configuration name storage should stamp, or `None` for the database
/// default.
///
/// # Errors
///
/// Returns [`InvalidLexicalLanguage`] when an explicit name is not a
/// plausible configuration identifier. Existence is verified by storage
/// against the actual catalog — this check only rejects what could
/// never be one, so the caller gets a clean error instead of a failed
/// transaction.
pub fn resolve_lexical_language(
    requested: Option<&str>,
    text: &str,
) -> Result<Option<String>, InvalidLexicalLanguage> {
    let Some(requested) = requested.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if requested.eq_ignore_ascii_case(LEXICAL_LANGUAGE_AUTO) {
        return Ok(detect_lexical_language(text).map(str::to_string));
    }
    let normalized = requested.to_ascii_lowercase();
    if is_plausible_config_name(&normalized) {
        Ok(Some(normalized))
    } else {
        Err(InvalidLexicalLanguage {
            supplied: requested.to_string(),
        })
    }
}

/// Shape check for a configuration name: an optionally schema-qualified
/// `PostgreSQL` identifier in its lowercase spelling. Deliberately allows
/// names beyond the shipped 30 — a deployment may `CREATE TEXT SEARCH
/// CONFIGURATION` (e.g. a hunspell-backed German with compound
/// splitting) and address it here.
fn is_plausible_config_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let (first, second, extra) = (parts.next(), parts.next(), parts.next());
    if extra.is_some() {
        return false;
    }
    let ident_ok = |ident: &str| {
        !ident.is_empty()
            && ident.len() <= 63
            && !ident.starts_with(|c: char| c.is_ascii_digit())
            && ident
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    match (first, second) {
        (Some(one), None) => ident_ok(one),
        (Some(schema), Some(config)) => ident_ok(schema) && ident_ok(config),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_language_is_the_default() {
        assert_eq!(resolve_lexical_language(None, "whatever"), Ok(None));
        assert_eq!(resolve_lexical_language(Some("  "), "whatever"), Ok(None));
    }

    #[test]
    fn explicit_names_normalize_and_validate() {
        assert_eq!(
            resolve_lexical_language(Some("German"), ""),
            Ok(Some("german".to_string()))
        );
        assert_eq!(
            resolve_lexical_language(Some("public.german_hunspell"), ""),
            Ok(Some("public.german_hunspell".to_string()))
        );
        for bad in ["ger man", "a.b.c", "1german", "gérman", "x'; DROP--"] {
            assert!(resolve_lexical_language(Some(bad), "").is_err(), "{bad}");
        }
    }

    #[test]
    fn auto_detects_german_reliably_and_falls_back_when_unsure() {
        let german = "Die Bauleitung wurde beauftragt, die Fluchtwege nach DIN 18040 \
                      barrierefrei zu planen und die Türbreiten im Erdgeschoss zu prüfen.";
        assert_eq!(
            resolve_lexical_language(Some("auto"), german),
            Ok(Some("german".to_string()))
        );
        // A string with no language signal must not be guessed at: the
        // resolution is None, storage applies the default.
        assert_eq!(resolve_lexical_language(Some("auto"), "42"), Ok(None));
    }

    #[test]
    fn reliably_detected_languages_without_a_stemmer_map_to_simple() {
        let chinese = "这是一个完全用中文写成的段落，用于验证语言检测的行为。\
                       它包含足够多的字符以便检测器能够可靠地识别出中文。";
        assert_eq!(detect_lexical_language(chinese), Some("simple"));
    }
}
