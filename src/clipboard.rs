//! Copying text to the system clipboard with OSC 52.

/// The most bytes of text [`Cmd::copy_to_clipboard`](crate::Cmd::copy_to_clipboard)
/// sends, 1 MiB. Terminals cap what they accept, and some drop the whole
/// sequence when it is over their limit, so a larger text is not sent.
pub const CLIPBOARD_LIMIT: usize = 1 << 20;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding. Its output is only letters, digits, `+`, `/`
/// and `=`, which is why it is safe inside an escape sequence.
fn base64(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63]);
        out.push(ALPHABET[(n >> 12) as usize & 63]);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63]
        } else {
            b'='
        });
    }
    out
}

/// The bytes that copy `text` to the clipboard: `OSC 52 ; c ; base64 ST`.
/// `None` when the text is empty or over [`CLIPBOARD_LIMIT`]. The sequence
/// ends with ST (`ESC \`), which every terminal that reads OSC 52 accepts.
pub(crate) fn osc52(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || text.len() > CLIPBOARD_LIMIT {
        return None;
    }
    let mut out = b"\x1b]52;c;".to_vec();
    out.extend(base64(text.as_bytes()));
    out.extend_from_slice(b"\x1b\\");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        String::from_utf8(base64(s.as_bytes())).unwrap()
    }

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        assert_eq!(b64(""), "");
        assert_eq!(b64("f"), "Zg==");
        assert_eq!(b64("fo"), "Zm8=");
        assert_eq!(b64("foo"), "Zm9v");
        assert_eq!(b64("foob"), "Zm9vYg==");
        assert_eq!(b64("fooba"), "Zm9vYmE=");
        assert_eq!(b64("foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_of_all_byte_values_uses_only_the_alphabet() {
        let all: Vec<u8> = (0..=255).collect();
        let out = base64(&all);
        assert_eq!(out.len(), 344);
        assert!(
            out.iter()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(b))
        );
    }

    #[test]
    fn the_sequence_is_osc_52_with_the_clipboard_selection() {
        assert_eq!(osc52("hello").unwrap(), b"\x1b]52;c;aGVsbG8=\x1b\\");
        // Multibyte text is encoded as its UTF-8 bytes.
        assert_eq!(osc52("é").unwrap(), b"\x1b]52;c;w6k=\x1b\\");
    }

    #[test]
    fn escape_sequences_and_bell_in_the_text_cannot_end_the_osc() {
        let evil = "x\x07\x1b\\\x1b]52;c;evil\x07\u{9c}\x1b[2J";
        let out = osc52(evil).unwrap();
        let body = &out[b"\x1b]52;c;".len()..out.len() - 2];
        assert!(
            body.iter()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(b))
        );
        // Only the opening ESC and the closing ST are control bytes.
        assert_eq!(out.iter().filter(|&&b| b == 0x1b).count(), 2);
        assert!(!out.contains(&0x07));
    }

    #[test]
    fn empty_and_oversized_text_is_not_sent() {
        assert_eq!(osc52(""), None);
        assert!(osc52(&"a".repeat(CLIPBOARD_LIMIT)).is_some());
        assert_eq!(osc52(&"a".repeat(CLIPBOARD_LIMIT + 1)), None);
    }
}
