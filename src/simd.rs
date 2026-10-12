//! STUB — scalar placeholders; to be replaced by SIMD implementation (see DESIGN.md).
pub fn find_byte(hay: &[u8], needle: u8) -> Option<usize> { hay.iter().position(|&b| b == needle) }
pub fn rfind_byte(hay: &[u8], needle: u8) -> Option<usize> { hay.iter().rposition(|&b| b == needle) }
pub fn count_byte(hay: &[u8], needle: u8) -> usize { hay.iter().filter(|&&b| b == needle).count() }
pub fn newline_positions(hay: &[u8], base: usize, out: &mut Vec<usize>) {
    for (i, &b) in hay.iter().enumerate() { if b == b'\n' { out.push(base + i); } }
}
pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() { return Some(0); }
    hay.windows(needle.len()).position(|w| w == needle)
}
pub fn find_ascii_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() { return Some(0); }
    hay.windows(needle.len()).position(|w| w.eq_ignore_ascii_case(needle))
}
pub fn crlf_to_lf(src: &[u8]) -> Vec<u8> {
    let mut o = Vec::with_capacity(src.len());
    for (i, &b) in src.iter().enumerate() { if !(b == b'\r' && src.get(i + 1) == Some(&b'\n')) { o.push(b); } }
    o
}
pub fn lf_to_crlf(src: &[u8]) -> Vec<u8> {
    let mut o = Vec::with_capacity(src.len() + src.len() / 16);
    for &b in src { if b == b'\n' { o.push(b'\r'); } o.push(b); }
    o
}
pub fn is_ascii(src: &[u8]) -> bool { src.is_ascii() }
