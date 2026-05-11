//! String hygiene applied to platform-reported text fields.
//!
//! Platforms (notably macOS for LTR apps in some configurations, and RTL apps
//! on every platform) embed Unicode bidi format controls into the strings they
//! return for `name`, `value`, and `description`. These are a presentation-layer
//! hint, not part of the logical text — but they break naive equality
//! assertions like `assert el.value == "5"`.
//!
//! We strip them at the core layer so all three bindings inherit consistent
//! behavior. Consumers who need the original platform string can read it from
//! [`crate::element::ElementData::raw`] (see provider-specific keys).
//!
//! Stripped code points (Unicode Cf-category bidi controls):
//! - U+200E LEFT-TO-RIGHT MARK, U+200F RIGHT-TO-LEFT MARK
//! - U+202A..=U+202E embeddings, overrides, pop-directional-formatting
//! - U+2066..=U+2069 isolates and pop-directional-isolate

/// Returns true if `c` is a Unicode bidi format control.
#[inline]
pub fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Strip bidi format controls from `s`. Returns the input unchanged when none
/// are present so the common path avoids allocation.
pub fn strip_bidi(s: &str) -> String {
    if s.chars().any(is_bidi_control) {
        s.chars().filter(|c| !is_bidi_control(*c)).collect()
    } else {
        s.to_owned()
    }
}

/// Strip in-place for an `Option<String>`. Convenience for provider code that
/// derives `name`/`value`/`description` then wants to scrub the result.
pub fn strip_bidi_opt(s: Option<String>) -> Option<String> {
    s.map(|s| {
        if s.chars().any(is_bidi_control) {
            s.chars().filter(|c| !is_bidi_control(*c)).collect()
        } else {
            s
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrm_stripped() {
        assert_eq!(strip_bidi("\u{200E}5"), "5");
    }

    #[test]
    fn all_bidi_controls_stripped() {
        let raw = "a\u{200E}b\u{200F}c\u{202A}d\u{202B}e\u{202C}f\u{202D}g\u{202E}h\
                   \u{2066}i\u{2067}j\u{2068}k\u{2069}l";
        assert_eq!(strip_bidi(raw), "abcdefghijkl");
    }

    #[test]
    fn unaffected_text_unchanged() {
        let clean = "Hello, 世界! 🎉";
        assert_eq!(strip_bidi(clean), clean);
    }

    #[test]
    fn empty_string() {
        assert_eq!(strip_bidi(""), "");
    }

    #[test]
    fn non_bidi_format_chars_preserved() {
        // ZWJ (U+200D) and ZWNJ (U+200C) are Cf but NOT bidi controls. Leave them alone.
        let s = "a\u{200C}b\u{200D}c";
        assert_eq!(strip_bidi(s), s);
    }

    #[test]
    fn opt_some_stripped() {
        assert_eq!(
            strip_bidi_opt(Some("\u{200E}5".to_owned())),
            Some("5".to_owned())
        );
    }

    #[test]
    fn opt_none() {
        assert_eq!(strip_bidi_opt(None), None);
    }

    #[test]
    fn no_allocation_path_returns_equal() {
        let s = "no bidi here";
        assert_eq!(strip_bidi(s), s);
    }
}
