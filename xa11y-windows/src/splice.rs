//! Pure helpers shared between the live UIA backend and tests.
//!
//! Kept out of `uia.rs` so the logic is exercised on every platform's
//! `cargo test` run, not just Windows CI.

/// Splice `insert` into `current` at the given character offset.
///
/// The offset is measured in Unicode characters (to match UIA's UTF-16
/// text-range semantics), not bytes. Offsets past the end of `current`
/// are clamped, so callers can treat "end of string" and "no caret" the
/// same way by passing `current.chars().count()`.
// Called only from the Windows-gated `uia` module; the stub backend skips it.
// Kept always-compiled so the unit tests run on every platform's CI.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn splice_at_char_offset(current: &str, insert: &str, caret_char_offset: usize) -> String {
    let byte_offset = current
        .char_indices()
        .nth(caret_char_offset)
        .map(|(i, _)| i)
        .unwrap_or(current.len());
    let mut out = String::with_capacity(current.len() + insert.len());
    out.push_str(&current[..byte_offset]);
    out.push_str(insert);
    out.push_str(&current[byte_offset..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_at_start() {
        assert_eq!(splice_at_char_offset("abc", "X", 0), "Xabc");
    }

    #[test]
    fn splice_in_middle() {
        assert_eq!(splice_at_char_offset("hello world", "X", 5), "helloX world");
    }

    #[test]
    fn splice_at_end() {
        assert_eq!(splice_at_char_offset("abc", "X", 3), "abcX");
    }

    #[test]
    fn splice_past_end_clamps() {
        assert_eq!(splice_at_char_offset("abc", "X", 100), "abcX");
    }

    #[test]
    fn splice_empty_current() {
        assert_eq!(splice_at_char_offset("", "hello", 0), "hello");
    }

    #[test]
    fn splice_multibyte_char_offset_is_byte_safe() {
        // 'é' is two bytes in UTF-8; caret char offset 2 sits after it.
        // A naive byte-index splice at 2 would land inside the 'é' and panic.
        assert_eq!(splice_at_char_offset("héllo", "X", 2), "héXllo");
    }

    #[test]
    fn splice_multibyte_at_end() {
        assert_eq!(splice_at_char_offset("héllo", "X", 5), "hélloX");
    }
}
