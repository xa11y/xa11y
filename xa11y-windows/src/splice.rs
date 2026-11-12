//! Pure helpers shared between the live UIA backend and tests.
//!
//! Kept out of `uia.rs` so the logic is exercised on every platform's
//! `cargo test` run, not just Windows CI.

/// Splice `insert` into `current` and return the new caret's UTF-16 offset.
///
/// The input offset is measured in Unicode scalar values. Offsets past the end of `current`
/// are clamped, so callers can treat "end of string" and "no caret" the
/// same way by passing `current.chars().count()`.
// Called only from the Windows-gated `uia` module; the stub backend skips it.
// Kept always-compiled so the unit tests run on every platform's CI.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn splice_at_char_offset(
    current: &str,
    insert: &str,
    caret_char_offset: usize,
) -> (String, u32) {
    let byte_offset = current
        .char_indices()
        .nth(caret_char_offset)
        .map(|(i, _)| i)
        .unwrap_or(current.len());
    let mut out = String::with_capacity(current.len() + insert.len());
    out.push_str(&current[..byte_offset]);
    out.push_str(insert);
    let caret = xa11y_core::text_api::utf16_len(&out);
    out.push_str(&current[byte_offset..]);
    (out, caret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_at_start() {
        assert_eq!(splice_at_char_offset("abc", "X", 0), ("Xabc".into(), 1));
    }

    #[test]
    fn splice_in_middle() {
        assert_eq!(
            splice_at_char_offset("hello world", "X", 5),
            ("helloX world".into(), 6)
        );
    }

    #[test]
    fn splice_at_end() {
        assert_eq!(splice_at_char_offset("abc", "X", 3), ("abcX".into(), 4));
    }

    #[test]
    fn splice_past_end_clamps() {
        assert_eq!(splice_at_char_offset("abc", "X", 100), ("abcX".into(), 4));
    }

    #[test]
    fn splice_empty_current() {
        assert_eq!(splice_at_char_offset("", "hello", 0), ("hello".into(), 5));
    }

    #[test]
    fn splice_multibyte_char_offset_is_byte_safe() {
        // 'é' is two bytes in UTF-8; caret char offset 2 sits after it.
        // A naive byte-index splice at 2 would land inside the 'é' and panic.
        assert_eq!(splice_at_char_offset("héllo", "X", 2), ("héXllo".into(), 3));
    }

    #[test]
    fn splice_multibyte_at_end() {
        assert_eq!(splice_at_char_offset("héllo", "X", 5), ("hélloX".into(), 6));
    }

    #[test]
    fn splice_caret_counts_surrogate_pairs() {
        assert_eq!(splice_at_char_offset("a😀b", "🚀", 2), ("a😀🚀b".into(), 5));
    }
}
