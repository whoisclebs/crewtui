//! UTF-16 code units, as the Windows console gives them, to UTF-8.

/// Turns a stream of UTF-16 code units into UTF-8 bytes. A surrogate pair
/// can arrive split over two calls, so the first half is kept until the
/// second comes. An unpaired surrogate becomes U+FFFD.
#[derive(Debug, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct Utf16Decoder {
    high: Option<u16>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Utf16Decoder {
    /// Appends the UTF-8 for `unit` to `out`, if it completes a character.
    pub(crate) fn push(&mut self, unit: u16, out: &mut Vec<u8>) {
        let mut put = |c: char| {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        };
        match (self.high.take(), unit) {
            (Some(high), 0xDC00..=0xDFFF) => {
                let c = 0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                put(char::from_u32(c).unwrap_or('\u{fffd}'));
            }
            (Some(_), _) => {
                // The high half had no partner.
                put('\u{fffd}');
                self.push(unit, out);
            }
            (None, 0xD800..=0xDBFF) => self.high = Some(unit),
            (None, 0xDC00..=0xDFFF) => put('\u{fffd}'),
            (None, u) => put(char::from_u32(u32::from(u)).unwrap_or('\u{fffd}')),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(units: &[u16]) -> String {
        let mut d = Utf16Decoder::default();
        let mut out = Vec::new();
        for &u in units {
            d.push(u, &mut out);
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn plain_text_and_the_escape_sequences_the_console_sends() {
        assert_eq!(decode(&[0x61, 0x62]), "ab");
        assert_eq!(decode(&[0x1b, b'[' as u16, b'A' as u16]), "\u{1b}[A");
        assert_eq!(decode(&[0x00e9, 0x65e5]), "é日");
    }

    #[test]
    fn a_surrogate_pair_is_one_character_even_over_two_calls() {
        let mut d = Utf16Decoder::default();
        let mut out = Vec::new();
        d.push(0xD83D, &mut out);
        assert!(out.is_empty());
        d.push(0xDE00, &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "😀");
    }

    #[test]
    fn unpaired_surrogates_become_the_replacement_character() {
        assert_eq!(decode(&[0xD83D, 0x61]), "\u{fffd}a");
        assert_eq!(decode(&[0xDE00, 0x61]), "\u{fffd}a");
        assert_eq!(decode(&[0xD83D, 0xD83D, 0xDE00]), "\u{fffd}😀");
        // One left over at the end waits for its partner.
        assert_eq!(decode(&[0x61, 0xD83D]), "a");
    }
}
