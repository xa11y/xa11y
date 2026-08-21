//! Standard base64 encoding for MCP image content.
//!
//! Hand-rolled rather than pulled in as a dependency: this is the only
//! encoding the MCP server needs, and `xa11y` is deliberately thin (its whole
//! dependency set is `xa11y-core`, the platform provider, and `serde_json`).
//! A crate for thirty lines would also land in the Python wheel and the
//! Node addon, both of which link this crate.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD: u8 = b'=';

/// Encode `input` as standard base64 with padding (RFC 4648 §4).
pub(crate) fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        // Pack up to three bytes into one 24-bit group, left-aligned, so the
        // six-bit extractions below are uniform regardless of chunk length.
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let group = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(group >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(group >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(group >> 6) as usize & 0x3F] as char
        } else {
            PAD as char
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[group as usize & 0x3F] as char
        } else {
            PAD as char
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn matches_the_rfc_4648_test_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(input.as_bytes()), expected, "input: {input:?}");
        }
    }

    #[test]
    fn encodes_high_bytes_without_sign_extension() {
        // A PNG is full of bytes above 0x7F; a signed cast would corrupt them.
        assert_eq!(encode(&[0xFF, 0xFE, 0xFD]), "//79");
        assert_eq!(encode(&[0x89, 0x50, 0x4E, 0x47]), "iVBORw==");
    }

    #[test]
    fn output_length_is_always_a_multiple_of_four() {
        for len in 0..32 {
            let encoded = encode(&vec![0xAB; len]);
            assert_eq!(encoded.len() % 4, 0, "len {len} produced {encoded:?}");
        }
    }
}
