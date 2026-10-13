//! File bytes <-> internal UTF-8 with LF line endings (see DESIGN.md, "encoding.rs").

use crate::simd;
use std::borrow::Cow;

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Latin1,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineEnding {
    Lf,
    CrLf,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Decoded {
    /// Valid UTF-8, LF only.
    pub text: Vec<u8>,
    pub encoding: Encoding,
    pub line_ending: LineEnding,
}

/// Detect BOM / validity. Invalid UTF-8 without BOM -> Latin1 (each byte -> U+00xx), lossless.
/// Line ending = CrLf if the first '\n' in the file is preceded by '\r', else Lf (default CrLf for empty file).
/// Output text has CRLF normalised to LF.
pub fn decode(bytes: &[u8]) -> Decoded {
    let (encoding, utf8) = decode_to_utf8(bytes);
    finish(encoding, utf8)
}

/// Same result as `decode`, but takes ownership so the common cases (pure ASCII, UTF-8
/// with or without a BOM, no '\r' to strip) reuse the allocation instead of copying a
/// possibly very large file. Other cases fall back to `decode`'s conversion.
pub fn decode_owned(mut bytes: Vec<u8>) -> Decoded {
    match plain_utf8(&bytes) {
        Some((encoding, start)) => {
            if start > 0 {
                bytes.drain(..start);
            }
            finish(encoding, bytes)
        }
        None => {
            let (encoding, utf8) = decode_to_utf8(&bytes);
            finish(encoding, utf8)
        }
    }
}

/// Line-ending detection and CRLF normalisation for text already in UTF-8.
fn finish(encoding: Encoding, utf8: Vec<u8>) -> Decoded {
    let line_ending = detect_line_ending(&utf8);
    let text = if simd::find_byte(&utf8, b'\r').is_some() {
        simd::crlf_to_lf(&utf8)
    } else {
        utf8
    };
    Decoded {
        text,
        encoding,
        line_ending,
    }
}

/// Inverse: apply line ending then encode (write BOM where applicable).
/// Latin1: chars > U+00FF become b'?'.
pub fn encode(text: &[u8], encoding: Encoding, le: LineEnding) -> Vec<u8> {
    let lined: Cow<[u8]> = match le {
        LineEnding::Lf => Cow::Borrowed(text),
        LineEnding::CrLf => Cow::Owned(simd::lf_to_crlf(text)),
    };
    let bytes: &[u8] = &lined;
    match encoding {
        Encoding::Utf8 => bytes.to_vec(),
        Encoding::Utf8Bom => {
            let mut out = Vec::with_capacity(UTF8_BOM.len() + bytes.len());
            out.extend_from_slice(UTF8_BOM);
            out.extend_from_slice(bytes);
            out
        }
        Encoding::Utf16Le => {
            let s = String::from_utf8_lossy(bytes);
            let mut out = Vec::with_capacity(2 + bytes.len() * 2);
            out.extend_from_slice(UTF16_LE_BOM);
            for u in s.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
            out
        }
        Encoding::Utf16Be => {
            let s = String::from_utf8_lossy(bytes);
            let mut out = Vec::with_capacity(2 + bytes.len() * 2);
            out.extend_from_slice(UTF16_BE_BOM);
            for u in s.encode_utf16() {
                out.extend_from_slice(&u.to_be_bytes());
            }
            out
        }
        Encoding::Latin1 => {
            if simd::is_ascii(bytes) {
                return bytes.to_vec();
            }
            let s = String::from_utf8_lossy(bytes);
            let mut out = Vec::with_capacity(bytes.len());
            for c in s.chars() {
                let cp = c as u32;
                out.push(if cp <= 0xFF { cp as u8 } else { b'?' });
            }
            out
        }
    }
}

impl Encoding {
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Latin1 => "ANSI",
        }
    }
}

impl LineEnding {
    pub fn name(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
        }
    }
}

/// Encodings whose body is already UTF-8: returns the encoding and the offset where the
/// UTF-8 text starts (3 to skip a UTF-8 BOM). None for anything that needs conversion.
fn plain_utf8(bytes: &[u8]) -> Option<(Encoding, usize)> {
    // Pure ASCII cannot start with a BOM, so this is the fastest common case.
    if simd::is_ascii(bytes) {
        return Some((Encoding::Utf8, 0));
    }
    if let Some(body) = bytes.strip_prefix(UTF8_BOM) {
        if std::str::from_utf8(body).is_ok() {
            return Some((Encoding::Utf8Bom, UTF8_BOM.len()));
        }
        return None;
    }
    if bytes.starts_with(UTF16_LE_BOM) || bytes.starts_with(UTF16_BE_BOM) {
        return None;
    }
    std::str::from_utf8(bytes)
        .is_ok()
        .then_some((Encoding::Utf8, 0))
}

/// Returns the encoding and the decoded UTF-8 bytes (line endings not yet normalised).
fn decode_to_utf8(bytes: &[u8]) -> (Encoding, Vec<u8>) {
    if let Some((encoding, start)) = plain_utf8(bytes) {
        return (encoding, bytes[start..].to_vec());
    }
    if let Some(body) = bytes.strip_prefix(UTF16_LE_BOM) {
        return (Encoding::Utf16Le, utf16_to_utf8(body, u16::from_le_bytes));
    }
    if let Some(body) = bytes.strip_prefix(UTF16_BE_BOM) {
        return (Encoding::Utf16Be, utf16_to_utf8(body, u16::from_be_bytes));
    }
    (Encoding::Latin1, latin1_to_utf8(bytes))
}

/// Decode UTF-16 code units (given by `from`) into UTF-8. Unpaired surrogates and a
/// trailing odd byte become U+FFFD.
fn utf16_to_utf8(body: &[u8], from: fn([u8; 2]) -> u16) -> Vec<u8> {
    let units = body.chunks_exact(2).map(|c| from([c[0], c[1]]));
    let mut out = String::with_capacity(body.len() / 2 + 4);
    for r in char::decode_utf16(units) {
        out.push(r.unwrap_or('\u{FFFD}'));
    }
    if body.len() % 2 == 1 {
        out.push('\u{FFFD}');
    }
    out.into_bytes()
}

/// Every byte maps to the code point with the same value, so the mapping is lossless.
fn latin1_to_utf8(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 8);
    for &b in bytes {
        if b < 0x80 {
            out.push(b);
        } else {
            out.push(0xC0 | (b >> 6));
            out.push(0x80 | (b & 0x3F));
        }
    }
    out
}

/// Lf only if the first '\n' is not preceded by '\r'; CrLf (the Windows default) otherwise,
/// including files without any newline.
fn detect_line_ending(text: &[u8]) -> LineEnding {
    match simd::find_byte(text, b'\n') {
        Some(i) if i == 0 || text[i - 1] != b'\r' => LineEnding::Lf,
        _ => LineEnding::CrLf,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENCODINGS: [Encoding; 5] = [
        Encoding::Utf8,
        Encoding::Utf8Bom,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Latin1,
    ];
    const ENDINGS: [LineEnding; 2] = [LineEnding::Lf, LineEnding::CrLf];

    /// Text every encoding can represent: BMP, Latin-1 range, and an astral character
    /// (surrogate pair in UTF-16) when the encoding allows it.
    fn sample_text(enc: Encoding) -> Vec<u8> {
        let s = if enc == Encoding::Latin1 {
            "first line\ncafé déjà vu\n\nlast: ÿ\n"
        } else {
            "first line\ncafé déjà vu 日本語 🎉\n\nlast: ÿ\n"
        };
        s.as_bytes().to_vec()
    }

    #[test]
    fn round_trip_every_encoding_and_line_ending() {
        for &enc in &ENCODINGS {
            for &le in &ENDINGS {
                let text = sample_text(enc);
                let bytes = encode(&text, enc, le);
                let d = decode(&bytes);
                assert_eq!(d.text, text, "text {enc:?} {le:?}");
                assert_eq!(d.encoding, enc, "encoding {enc:?} {le:?}");
                assert_eq!(d.line_ending, le, "line ending {enc:?} {le:?}");
            }
        }
    }

    #[test]
    fn round_trip_is_byte_exact_for_encoded_output() {
        // Decoding then re-encoding with the detected settings reproduces the file.
        for &enc in &ENCODINGS {
            for &le in &ENDINGS {
                let file = encode(&sample_text(enc), enc, le);
                let d = decode(&file);
                assert_eq!(
                    encode(&d.text, d.encoding, d.line_ending),
                    file,
                    "{enc:?} {le:?}"
                );
            }
        }
    }

    #[test]
    fn utf8_bom_is_detected_and_stripped() {
        let d = decode(b"\xEF\xBB\xBFhi\r\nthere");
        assert_eq!(d.encoding, Encoding::Utf8Bom);
        assert_eq!(d.text, b"hi\nthere");
        assert_eq!(d.line_ending, LineEnding::CrLf);
    }

    #[test]
    fn utf16_bom_is_detected_for_both_byte_orders() {
        let le = decode(b"\xFF\xFEa\x00\n\x00b\x00");
        assert_eq!(le.encoding, Encoding::Utf16Le);
        assert_eq!(le.text, b"a\nb");
        assert_eq!(le.line_ending, LineEnding::Lf);

        let be = decode(b"\xFE\xFF\x00a\x00\r\x00\n\x00b");
        assert_eq!(be.encoding, Encoding::Utf16Be);
        assert_eq!(be.text, b"a\nb");
        assert_eq!(be.line_ending, LineEnding::CrLf);
    }

    #[test]
    fn utf16_surrogates_and_odd_bytes_become_replacement() {
        // Lone high surrogate D800 followed by 'a'.
        let d = decode(b"\xFF\xFE\x00\xD8a\x00");
        assert_eq!(d.text, "\u{FFFD}a".as_bytes());
        // Trailing odd byte: 'a' is complete, the final 'b' is half a code unit.
        let d = decode(b"\xFF\xFEa\x00b");
        assert_eq!(d.text, "a\u{FFFD}".as_bytes());
    }

    #[test]
    fn bom_with_invalid_body_falls_back_to_latin1_losslessly() {
        let file = b"\xEF\xBB\xBF\xE9x";
        let d = decode(file);
        assert_eq!(d.encoding, Encoding::Latin1);
        assert_eq!(encode(&d.text, d.encoding, d.line_ending), file);
    }

    #[test]
    fn invalid_utf8_becomes_latin1_and_round_trips_byte_exact() {
        let lf = b"caf\xE9 \xFF\x80 end\nline2 \xC0\n";
        let d = decode(lf);
        assert_eq!(d.encoding, Encoding::Latin1);
        assert_eq!(d.line_ending, LineEnding::Lf);
        assert_eq!(encode(&d.text, d.encoding, d.line_ending), lf);

        let crlf = b"caf\xE9\r\n\xFF\x80\r\n";
        let d = decode(crlf);
        assert_eq!(d.encoding, Encoding::Latin1);
        assert_eq!(d.line_ending, LineEnding::CrLf);
        assert_eq!(d.text, "café\n\u{FF}\u{80}\n".as_bytes());
        assert_eq!(encode(&d.text, d.encoding, d.line_ending), crlf);
    }

    #[test]
    fn random_high_bytes_round_trip_byte_exact() {
        // No '\r' and no FE/FF bytes, so there is one consistent LF ending and no UTF-16 BOM.
        let alphabet: &[u8] = b"ab\n\xEF\xBB\xBF\xC3\xA9\x80\xE9\xFD";
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..500 {
            let mut file = Vec::new();
            let len = {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % 200) as usize
            };
            for _ in 0..len {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                file.push(alphabet[(state % alphabet.len() as u64) as usize]);
            }
            let d = decode(&file);
            assert_eq!(
                encode(&d.text, d.encoding, d.line_ending),
                file,
                "file {file:?}"
            );
        }
    }

    #[test]
    fn latin1_encode_replaces_unrepresentable_chars() {
        let out = encode("aé€😀".as_bytes(), Encoding::Latin1, LineEnding::Lf);
        assert_eq!(out, b"a\xE9??");
    }

    #[test]
    fn empty_file() {
        let d = decode(b"");
        assert_eq!(d.text, b"");
        assert_eq!(d.encoding, Encoding::Utf8);
        assert_eq!(d.line_ending, LineEnding::CrLf);
        for &enc in &ENCODINGS {
            for &le in &ENDINGS {
                let bytes = encode(b"", enc, le);
                let d = decode(&bytes);
                assert_eq!(d.text, b"", "{enc:?} {le:?}");
                // An empty Latin1 file is indistinguishable from empty UTF-8.
                if enc != Encoding::Latin1 {
                    assert_eq!(d.encoding, enc, "{enc:?} {le:?}");
                }
            }
        }
        assert_eq!(encode(b"", Encoding::Utf16Le, LineEnding::Lf), b"\xFF\xFE");
    }

    #[test]
    fn line_ending_uses_first_newline() {
        assert_eq!(decode(b"a\nb\r\n").line_ending, LineEnding::Lf);
        assert_eq!(decode(b"a\r\nb\n").line_ending, LineEnding::CrLf);
        assert_eq!(decode(b"no newline").line_ending, LineEnding::CrLf);
        assert_eq!(decode(b"\nx").line_ending, LineEnding::Lf);
        // A bare '\r' is not a line break and is kept.
        assert_eq!(decode(b"a\rb").text, b"a\rb");
    }

    /// Every input the decode tests above exercise, plus a few plain/BOM/CR edge cases.
    fn corpus() -> Vec<Vec<u8>> {
        let mut v: Vec<Vec<u8>> = Vec::new();
        for &enc in &ENCODINGS {
            for &le in &ENDINGS {
                v.push(encode(&sample_text(enc), enc, le));
            }
        }
        let fixed: [&[u8]; 16] = [
            b"",
            b"\xEF\xBB\xBFhi\r\nthere",
            b"\xEF\xBB\xBF",
            b"\xEF\xBB\xBF\xE9x",
            b"\xEF\xBB\xBF\xE9x\r\n",
            b"\xFF\xFEa\x00\n\x00b\x00",
            b"\xFE\xFF\x00a\x00\r\x00\n\x00b",
            b"\xFF\xFE\x00\xD8a\x00",
            b"\xFF\xFEa\x00b",
            b"caf\xE9 \xFF\x80 end\nline2 \xC0\n",
            b"caf\xE9\r\n\xFF\x80\r\n",
            b"a\nb\r\n",
            b"a\r\nb\n",
            b"no newline",
            b"a\rb",
            b"\nx",
        ];
        v.extend(fixed.iter().map(|f| f.to_vec()));
        // Pseudo-random mixes of bytes, as in `random_high_bytes_round_trip_byte_exact`.
        let alphabet: &[u8] = b"ab\n\r\xEF\xBB\xBF\xC3\xA9\x80\xE9\xFD\xFE\xFF\x00";
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..300 {
            let mut file = Vec::new();
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            for _ in 0..(state % 120) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                file.push(alphabet[(state % alphabet.len() as u64) as usize]);
            }
            v.push(file);
        }
        v
    }

    #[test]
    fn decode_owned_matches_decode_on_all_inputs() {
        for file in corpus() {
            let expected = decode(&file);
            let owned = decode_owned(file.clone());
            assert_eq!(owned, expected, "file {file:?}");
        }
    }

    #[test]
    fn decode_owned_reuses_allocation_for_plain_utf8() {
        let file = b"plain text\nno carriage returns\n".to_vec();
        let ptr = file.as_ptr();
        let d = decode_owned(file);
        assert_eq!(d.text.as_ptr(), ptr, "ASCII is not copied");

        let file = "caf\u{e9}\nunicode\n".as_bytes().to_vec();
        let ptr = file.as_ptr();
        let d = decode_owned(file);
        assert_eq!(d.encoding, Encoding::Utf8);
        assert_eq!(d.text.as_ptr(), ptr, "valid UTF-8 is not copied");

        let mut file = b"\xEF\xBB\xBF".to_vec();
        file.extend_from_slice("caf\u{e9}\n".as_bytes());
        let d = decode_owned(file);
        assert_eq!(d.encoding, Encoding::Utf8Bom);
        assert_eq!(d.text, "caf\u{e9}\n".as_bytes());
    }

    #[test]
    fn names() {
        assert_eq!(Encoding::Utf8.name(), "UTF-8");
        assert_eq!(Encoding::Utf8Bom.name(), "UTF-8 BOM");
        assert_eq!(Encoding::Utf16Le.name(), "UTF-16 LE");
        assert_eq!(Encoding::Utf16Be.name(), "UTF-16 BE");
        assert_eq!(Encoding::Latin1.name(), "ANSI");
        assert_eq!(LineEnding::Lf.name(), "LF");
        assert_eq!(LineEnding::CrLf.name(), "CRLF");
    }
}
