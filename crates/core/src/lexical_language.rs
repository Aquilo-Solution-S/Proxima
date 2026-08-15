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
//! Detection is gated on the detector's own reliability signal. Ungated
//! detection on short text is worse than the database default: a wrongly
//! stamped language makes the row unmatchable by its own content words.
//! An unreliable detection therefore falls back rather than guessing.
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
     (e.g. 'english', 'german', optionally schema-qualified), an ISO 639 / BCP-47 code \
     (e.g. 'de', 'de-DE'), or 'auto' to detect from content"
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

/// The configuration for an ISO 639-1/639-3 code (a BCP-47 primary
/// subtag), if the code names a language this module knows.
///
/// Callers speak ISO — agents, HTML `lang` attributes, PDF and Office
/// metadata all carry BCP-47 tags — while the stored value is a
/// `regconfig` catalog reference, so ISO codes are accepted as *aliases*
/// and the configuration name stays canonical. Codes for languages
/// without a shipped stemmer map to `simple`, exactly as the detector
/// treats those languages; unknown codes fall through to the
/// config-name path (no 2–3-letter shipped configuration exists, and a
/// custom one can always be addressed schema-qualified).
#[must_use]
fn config_for_iso_code(code: &str) -> Option<&'static str> {
    Some(match code {
        // Shipped Snowball configurations (ISO 639-1, then 639-2/3 forms).
        "ar" | "ara" => "arabic",
        "hy" | "hye" => "armenian",
        "eu" | "eus" | "baq" => "basque",
        "ca" | "cat" => "catalan",
        "da" | "dan" => "danish",
        "nl" | "nld" | "dut" => "dutch",
        "en" | "eng" => "english",
        "et" | "est" => "estonian",
        "fi" | "fin" => "finnish",
        "fr" | "fra" | "fre" => "french",
        "de" | "deu" | "ger" => "german",
        "el" | "ell" | "gre" => "greek",
        "hi" | "hin" => "hindi",
        "hu" | "hun" => "hungarian",
        "id" | "ind" => "indonesian",
        "ga" | "gle" => "irish",
        "it" | "ita" => "italian",
        "lt" | "lit" => "lithuanian",
        "ne" | "nep" => "nepali",
        "no" | "nb" | "nn" | "nor" | "nob" | "nno" => "norwegian",
        "pt" | "por" => "portuguese",
        "ro" | "ron" | "rum" => "romanian",
        "ru" | "rus" => "russian",
        "sr" | "srp" => "serbian",
        "es" | "spa" => "spanish",
        "sv" | "swe" => "swedish",
        "ta" | "tam" => "tamil",
        "tr" | "tur" => "turkish",
        "yi" | "yid" => "yiddish",
        // Known languages without a shipped stemmer → simple, matching
        // the detector's treatment of the same languages.
        "zh" | "cmn" | "zho" | "ja" | "jpn" | "ko" | "kor" | "pl" | "pol" | "cs" | "ces"
        | "cze" | "sk" | "slk" | "uk" | "ukr" | "be" | "bel" | "bg" | "bul" | "hr" | "hrv"
        | "sl" | "slv" | "mk" | "mkd" | "lv" | "lav" | "vi" | "vie" | "th" | "tha" | "he"
        | "heb" | "fa" | "fas" | "pes" | "ur" | "urd" | "bn" | "ben" | "mr" | "mar" | "gu"
        | "guj" | "pa" | "pan" | "te" | "tel" | "kn" | "kan" | "ml" | "mal" | "si" | "sin"
        | "my" | "mya" | "km" | "khm" | "az" | "aze" | "uz" | "uzb" | "tl" | "tgl" | "jv"
        | "jav" | "af" | "afr" | "la" | "lat" | "eo" | "epo" | "am" | "amh" => "simple",
        _ => return None,
    })
}

/// Resolve a write surface's optional `language` argument to the
/// configuration name storage should stamp, or `None` for the database
/// default.
///
/// Accepted forms: a `PostgreSQL` text-search configuration name
/// (`"german"`, optionally schema-qualified), an ISO 639 code or BCP-47
/// tag (`"de"`, `"deu"`, `"de-DE"` — the primary subtag decides), or
/// `"auto"`.
///
/// # Errors
///
/// Returns [`InvalidLexicalLanguage`] when an explicit name is neither a
/// known ISO code nor a plausible configuration identifier. Existence of
/// configuration names is verified by storage against the actual catalog
/// — this check only rejects what could never be one, so the caller gets
/// a clean error instead of a failed transaction.
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
    // BCP-47: the primary subtag carries the language; region and script
    // subtags ("de-DE", "zh-Hans-CN") never change the stemmer.
    let primary_subtag = normalized.split('-').next().unwrap_or(&normalized);
    if let Some(config) = config_for_iso_code(primary_subtag) {
        return Ok(Some(config.to_string()));
    }
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
    fn iso_codes_and_bcp47_tags_alias_the_shipped_configurations() {
        for (code, config) in [
            ("de", "german"),
            ("deu", "german"),
            ("DE-de", "german"),
            ("de-DE", "german"),
            ("en", "english"),
            ("pt-BR", "portuguese"),
            ("nb", "norwegian"),
            // Known languages without a shipped stemmer map to `simple`,
            // exactly as the detector treats them.
            ("zh", "simple"),
            ("zh-Hans-CN", "simple"),
            ("ja", "simple"),
            ("pl", "simple"),
        ] {
            assert_eq!(
                resolve_lexical_language(Some(code), ""),
                Ok(Some(config.to_string())),
                "{code}"
            );
        }
        // An unknown code with a subtag separator can only be a BCP-47 tag,
        // and an unknown tag is an error, not a config name.
        assert!(resolve_lexical_language(Some("xx-YY"), "").is_err());
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
