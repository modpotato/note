//! SIMD byte kernels (see DESIGN.md, "simd.rs").
//!
//! x86_64: SSE2 is the baseline; AVX2 is selected once at runtime and cached in an atomic.
//! Every algorithm is written once as a generic kernel over the `Lanes` trait, and the two
//! entry points (`*_sse2`, `*_avx2`) are thin monomorphisations. The AVX2 wrappers carry
//! `#[target_feature(enable = "avx2")]`, so the inlined generic body is compiled with AVX2.
//!
//! Other targets: plain scalar code with the same public signatures.
//!
//! Head/tail handling: unaligned loads make a scalar head unnecessary, so the vector loop
//! starts at offset 0. Whatever does not fill a vector is finished with scalar code.

/// Index of first occurrence of `needle` in `hay`.
#[inline]
pub fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
    imp::find_byte(hay, needle)
}

/// Index of last occurrence.
#[inline]
pub fn rfind_byte(hay: &[u8], needle: u8) -> Option<usize> {
    imp::rfind_byte(hay, needle)
}

/// Number of occurrences of `needle` (used for '\n' counting).
#[inline]
pub fn count_byte(hay: &[u8], needle: u8) -> usize {
    imp::count_byte(hay, needle)
}

/// Push `base + i` for every index i where hay[i] == b'\n' into `out`.
#[inline]
pub fn newline_positions(hay: &[u8], base: usize, out: &mut Vec<usize>) {
    imp::newline_positions(hay, base, out)
}

/// Substring search. Empty needle -> Some(0).
#[inline]
pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    imp::find(hay, needle)
}

/// ASCII-case-insensitive substring search.
#[inline]
pub fn find_ascii_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    imp::find_ascii_ci(hay, needle)
}

/// Remove every b'\r' that is immediately followed by b'\n' (CRLF -> LF). Returns new Vec.
pub fn crlf_to_lf(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let mut pos = 0;
    while let Some(off) = find_byte(&src[pos..], b'\r') {
        let cr = pos + off;
        out.extend_from_slice(&src[pos..cr]);
        // Drop the '\r' only when a '\n' follows; otherwise keep it.
        if src.get(cr + 1) != Some(&b'\n') {
            out.push(b'\r');
        }
        pos = cr + 1;
    }
    out.extend_from_slice(&src[pos..]);
    out
}

/// LF -> CRLF.
pub fn lf_to_crlf(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() + count_byte(src, b'\n'));
    let mut pos = 0;
    while let Some(off) = find_byte(&src[pos..], b'\n') {
        let lf = pos + off;
        out.extend_from_slice(&src[pos..lf]);
        out.extend_from_slice(b"\r\n");
        pos = lf + 1;
    }
    out.extend_from_slice(&src[pos..]);
    out
}

/// true if all bytes < 0x80.
#[inline]
pub fn is_ascii(src: &[u8]) -> bool {
    imp::is_ascii(src)
}

// ---------------------------------------------------------------------------
// x86_64: SSE2 + AVX2 kernels
// ---------------------------------------------------------------------------
#[cfg(target_arch = "x86_64")]
mod imp {
    use core::arch::x86_64::{
        __m128i, __m256i, _mm256_cmpeq_epi8, _mm256_loadu_si256, _mm256_movemask_epi8,
        _mm256_or_si256, _mm256_set1_epi8, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8,
        _mm_or_si128, _mm_set1_epi8,
    };
    use std::sync::atomic::{AtomicU8, Ordering};

    const LEVEL_UNKNOWN: u8 = 0;
    const LEVEL_SSE2: u8 = 1;
    const LEVEL_AVX2: u8 = 2;

    static LEVEL: AtomicU8 = AtomicU8::new(LEVEL_UNKNOWN);

    /// Runtime choice between the AVX2 and SSE2 kernels, detected once and cached.
    #[inline]
    fn use_avx2() -> bool {
        match LEVEL.load(Ordering::Relaxed) {
            LEVEL_AVX2 => true,
            LEVEL_SSE2 => false,
            _ => {
                let avx2 = std::is_x86_feature_detected!("avx2");
                let level = if avx2 { LEVEL_AVX2 } else { LEVEL_SSE2 };
                LEVEL.store(level, Ordering::Relaxed);
                avx2
            }
        }
    }

    /// A vector register of bytes. All methods are unsafe because they may require a
    /// target feature (AVX2) and read or write through raw pointers.
    pub(super) trait Lanes: Copy {
        /// Bytes per vector.
        const WIDTH: usize;
        /// Unaligned load of `WIDTH` bytes starting at `p`.
        unsafe fn load(p: *const u8) -> Self;
        /// Broadcast `b` into every lane.
        unsafe fn splat(b: u8) -> Self;
        /// Lane-wise equality, each lane becoming 0xFF or 0x00.
        unsafe fn cmpeq(self, other: Self) -> Self;
        /// Lane-wise OR.
        unsafe fn or(self, other: Self) -> Self;
        /// Bit i of the result is the high bit of lane i (bits >= WIDTH are zero).
        unsafe fn movemask(self) -> u64;
    }

    impl Lanes for __m128i {
        const WIDTH: usize = 16;

        #[inline(always)]
        unsafe fn load(p: *const u8) -> Self {
            // SAFETY: caller guarantees `p..p+16` is readable. Unaligned load, so no alignment needed.
            unsafe { _mm_loadu_si128(p as *const __m128i) }
        }
        #[inline(always)]
        unsafe fn splat(b: u8) -> Self {
            // SAFETY: SSE2 is part of the x86_64 baseline.
            unsafe { _mm_set1_epi8(b as i8) }
        }
        #[inline(always)]
        unsafe fn cmpeq(self, other: Self) -> Self {
            // SAFETY: SSE2 is part of the x86_64 baseline.
            unsafe { _mm_cmpeq_epi8(self, other) }
        }
        #[inline(always)]
        unsafe fn or(self, other: Self) -> Self {
            // SAFETY: SSE2 is part of the x86_64 baseline.
            unsafe { _mm_or_si128(self, other) }
        }
        #[inline(always)]
        unsafe fn movemask(self) -> u64 {
            // SAFETY: SSE2 is part of the x86_64 baseline. The result has 16 significant bits.
            unsafe { _mm_movemask_epi8(self) as u32 as u64 }
        }
    }

    impl Lanes for __m256i {
        const WIDTH: usize = 32;

        #[inline]
        #[target_feature(enable = "avx2")]
        unsafe fn load(p: *const u8) -> Self {
            // SAFETY: caller guarantees `p..p+32` is readable; the function is only reached
            // through the AVX2 wrappers, which run after runtime detection.
            unsafe { _mm256_loadu_si256(p as *const __m256i) }
        }
        #[inline]
        #[target_feature(enable = "avx2")]
        unsafe fn splat(b: u8) -> Self {
            _mm256_set1_epi8(b as i8)
        }
        #[inline]
        #[target_feature(enable = "avx2")]
        unsafe fn cmpeq(self, other: Self) -> Self {
            _mm256_cmpeq_epi8(self, other)
        }
        #[inline]
        #[target_feature(enable = "avx2")]
        unsafe fn or(self, other: Self) -> Self {
            _mm256_or_si256(self, other)
        }
        #[inline]
        #[target_feature(enable = "avx2")]
        unsafe fn movemask(self) -> u64 {
            // SAFETY: only reached after AVX2 detection (see `load`). 32 significant bits.
            _mm256_movemask_epi8(self) as u32 as u64
        }
    }

    /// Equality bitmask of one vector at `p` (WIDTH bits).
    #[inline(always)]
    unsafe fn eq_vec<V: Lanes>(p: *const u8, nv: V) -> u64 {
        // SAFETY: caller guarantees WIDTH readable bytes at `p`.
        unsafe { V::load(p).cmpeq(nv).movemask() }
    }

    /// Equality bitmask of the 64 bytes at `p`, one bit per byte.
    #[inline(always)]
    unsafe fn eq_block64<V: Lanes>(p: *const u8, nv: V) -> u64 {
        let mut m = 0u64;
        let mut k = 0;
        while k < 64 {
            // SAFETY: caller guarantees 64 readable bytes at `p`; k + WIDTH <= 64 since WIDTH divides 64.
            m |= unsafe { eq_vec::<V>(p.add(k), nv) } << k;
            k += V::WIDTH;
        }
        m
    }

    /// Candidate mask for a case-pair (`CI` = case-insensitive), 64 bytes at `p`.
    #[inline(always)]
    unsafe fn pair_block64<V: Lanes, const CI: bool>(p: *const u8, lo: V, up: V) -> u64 {
        // SAFETY: caller guarantees 64 readable bytes at `p`.
        unsafe {
            if CI {
                eq_block64::<V>(p, lo) | eq_block64::<V>(p, up)
            } else {
                eq_block64::<V>(p, lo)
            }
        }
    }

    /// Candidate mask for a case-pair, one vector at `p`.
    #[inline(always)]
    unsafe fn pair_vec<V: Lanes, const CI: bool>(p: *const u8, lo: V, up: V) -> u64 {
        // SAFETY: caller guarantees WIDTH readable bytes at `p`.
        unsafe {
            if CI {
                eq_vec::<V>(p, lo) | eq_vec::<V>(p, up)
            } else {
                eq_vec::<V>(p, lo)
            }
        }
    }

    #[inline(always)]
    fn find_byte_gen<V: Lanes>(hay: &[u8], needle: u8) -> Option<usize> {
        let n = hay.len();
        let p = hay.as_ptr();
        let nv = unsafe { V::splat(needle) };
        let mut i = 0;
        while i + 64 <= n {
            // SAFETY: i + 64 <= n, so 64 bytes at p+i are in bounds.
            let m = unsafe { eq_block64::<V>(p.add(i), nv) };
            if m != 0 {
                return Some(i + m.trailing_zeros() as usize);
            }
            i += 64;
        }
        while i + V::WIDTH <= n {
            // SAFETY: i + WIDTH <= n.
            let m = unsafe { eq_vec::<V>(p.add(i), nv) };
            if m != 0 {
                return Some(i + m.trailing_zeros() as usize);
            }
            i += V::WIDTH;
        }
        hay[i..].iter().position(|&b| b == needle).map(|k| i + k)
    }

    #[inline(always)]
    fn rfind_byte_gen<V: Lanes>(hay: &[u8], needle: u8) -> Option<usize> {
        let p = hay.as_ptr();
        let nv = unsafe { V::splat(needle) };
        let mut end = hay.len();
        while end >= 64 {
            let s = end - 64;
            // SAFETY: s + 64 == end <= len.
            let m = unsafe { eq_block64::<V>(p.add(s), nv) };
            if m != 0 {
                return Some(s + 63 - m.leading_zeros() as usize);
            }
            end = s;
        }
        while end >= V::WIDTH {
            let s = end - V::WIDTH;
            // SAFETY: s + WIDTH == end <= len.
            let m = unsafe { eq_vec::<V>(p.add(s), nv) };
            if m != 0 {
                return Some(s + 63 - m.leading_zeros() as usize);
            }
            end = s;
        }
        hay[..end].iter().rposition(|&b| b == needle)
    }

    #[inline(always)]
    fn count_byte_gen<V: Lanes>(hay: &[u8], needle: u8) -> usize {
        let n = hay.len();
        let p = hay.as_ptr();
        let nv = unsafe { V::splat(needle) };
        let w = V::WIDTH;
        let mut i = 0;
        let mut total = 0usize;
        // 4x unrolled main loop: four independent compare+popcount chains per iteration.
        while i + 4 * w <= n {
            let mut c = 0u32;
            for k in 0..4 {
                // SAFETY: i + 4w <= n, so each of the four vectors is in bounds.
                c += unsafe { eq_vec::<V>(p.add(i + k * w), nv) }.count_ones();
            }
            total += c as usize;
            i += 4 * w;
        }
        while i + w <= n {
            // SAFETY: i + w <= n.
            total += unsafe { eq_vec::<V>(p.add(i), nv) }.count_ones() as usize;
            i += w;
        }
        total + hay[i..].iter().filter(|&&b| b == needle).count()
    }

    #[inline(always)]
    fn newline_positions_gen<V: Lanes>(hay: &[u8], base: usize, out: &mut Vec<usize>) {
        let n = hay.len();
        let p = hay.as_ptr();
        let nv = unsafe { V::splat(b'\n') };
        let mut i = 0;
        while i + 64 <= n {
            // SAFETY: i + 64 <= n.
            let mut m = unsafe { eq_block64::<V>(p.add(i), nv) };
            while m != 0 {
                out.push(base + i + m.trailing_zeros() as usize);
                m &= m - 1;
            }
            i += 64;
        }
        while i + V::WIDTH <= n {
            // SAFETY: i + WIDTH <= n.
            let mut m = unsafe { eq_vec::<V>(p.add(i), nv) };
            while m != 0 {
                out.push(base + i + m.trailing_zeros() as usize);
                m &= m - 1;
            }
            i += V::WIDTH;
        }
        for (k, &b) in hay[i..].iter().enumerate() {
            if b == b'\n' {
                out.push(base + i + k);
            }
        }
    }

    #[inline(always)]
    fn verify_at<const CI: bool>(hay: &[u8], j: usize, needle: &[u8]) -> bool {
        let window = &hay[j..j + needle.len()];
        if CI {
            window.eq_ignore_ascii_case(needle)
        } else {
            window == needle
        }
    }

    /// Generic-SIMD substring search: candidates are positions where the first needle byte
    /// matches at offset 0 AND the last needle byte matches at offset n-1. Survivors are
    /// verified with a slice comparison.
    #[inline(always)]
    fn find_gen<V: Lanes, const CI: bool>(hay: &[u8], needle: &[u8]) -> Option<usize> {
        let m = needle.len();
        if m == 0 {
            return Some(0);
        }
        if m > hay.len() {
            return None;
        }
        let last = m - 1;
        // Candidate starts are 0..=max_start.
        let max_start = hay.len() - m;
        let end = max_start + 1; // exclusive bound on candidate starts
        let p = hay.as_ptr();

        let fold = |b: u8| {
            if CI {
                (b.to_ascii_lowercase(), b.to_ascii_uppercase())
            } else {
                (b, b)
            }
        };
        let (f_lo, f_up) = fold(needle[0]);
        let (l_lo, l_up) = fold(needle[last]);
        // SAFETY: splats have no memory preconditions.
        let (fl, fu, ll, lu) = unsafe {
            (
                V::splat(f_lo),
                V::splat(f_up),
                V::splat(l_lo),
                V::splat(l_up),
            )
        };

        let mut i = 0;
        // 64 candidates at once: loads reach i+63+last <= max_start+last < hay.len().
        while i + 64 <= end {
            // SAFETY: i + 64 <= end implies i+63+last < hay.len() and i+63 < hay.len().
            let mut mask = unsafe {
                pair_block64::<V, CI>(p.add(i), fl, fu)
                    & pair_block64::<V, CI>(p.add(i + last), ll, lu)
            };
            while mask != 0 {
                let j = i + mask.trailing_zeros() as usize;
                if verify_at::<CI>(hay, j, needle) {
                    return Some(j);
                }
                mask &= mask - 1;
            }
            i += 64;
        }
        // One vector of candidates at a time.
        while i + V::WIDTH <= end {
            // SAFETY: i + WIDTH <= end implies the same bounds as above, for WIDTH bytes.
            let mut mask = unsafe {
                pair_vec::<V, CI>(p.add(i), fl, fu) & pair_vec::<V, CI>(p.add(i + last), ll, lu)
            };
            while mask != 0 {
                let j = i + mask.trailing_zeros() as usize;
                if verify_at::<CI>(hay, j, needle) {
                    return Some(j);
                }
                mask &= mask - 1;
            }
            i += V::WIDTH;
        }
        // Scalar tail.
        (i..end).find(|&j| verify_at::<CI>(hay, j, needle))
    }

    #[inline(always)]
    fn is_ascii_gen<V: Lanes>(src: &[u8]) -> bool {
        let n = src.len();
        let p = src.as_ptr();
        let w = V::WIDTH;
        let mut i = 0;
        while i + 4 * w <= n {
            // SAFETY: i + 4w <= n.
            let acc = unsafe {
                let a = V::load(p.add(i)).or(V::load(p.add(i + w)));
                let b = V::load(p.add(i + 2 * w)).or(V::load(p.add(i + 3 * w)));
                a.or(b).movemask()
            };
            // The high bit of each lane is what makes a byte non-ASCII.
            if acc != 0 {
                return false;
            }
            i += 4 * w;
        }
        while i + w <= n {
            // SAFETY: i + w <= n.
            if unsafe { V::load(p.add(i)).movemask() } != 0 {
                return false;
            }
            i += w;
        }
        src[i..].is_ascii()
    }

    // --- SSE2 entry points (baseline, always valid on x86_64) ---

    pub(super) unsafe fn find_byte_sse2(hay: &[u8], needle: u8) -> Option<usize> {
        find_byte_gen::<__m128i>(hay, needle)
    }
    pub(super) unsafe fn rfind_byte_sse2(hay: &[u8], needle: u8) -> Option<usize> {
        rfind_byte_gen::<__m128i>(hay, needle)
    }
    pub(super) unsafe fn count_byte_sse2(hay: &[u8], needle: u8) -> usize {
        count_byte_gen::<__m128i>(hay, needle)
    }
    pub(super) unsafe fn newline_positions_sse2(hay: &[u8], base: usize, out: &mut Vec<usize>) {
        newline_positions_gen::<__m128i>(hay, base, out)
    }
    pub(super) unsafe fn find_sse2(hay: &[u8], needle: &[u8]) -> Option<usize> {
        find_gen::<__m128i, false>(hay, needle)
    }
    pub(super) unsafe fn find_ascii_ci_sse2(hay: &[u8], needle: &[u8]) -> Option<usize> {
        find_gen::<__m128i, true>(hay, needle)
    }
    pub(super) unsafe fn is_ascii_sse2(src: &[u8]) -> bool {
        is_ascii_gen::<__m128i>(src)
    }

    // --- AVX2 entry points: only call after `use_avx2()` returned true ---

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn find_byte_avx2(hay: &[u8], needle: u8) -> Option<usize> {
        find_byte_gen::<__m256i>(hay, needle)
    }
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn rfind_byte_avx2(hay: &[u8], needle: u8) -> Option<usize> {
        rfind_byte_gen::<__m256i>(hay, needle)
    }
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn count_byte_avx2(hay: &[u8], needle: u8) -> usize {
        count_byte_gen::<__m256i>(hay, needle)
    }
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn newline_positions_avx2(hay: &[u8], base: usize, out: &mut Vec<usize>) {
        newline_positions_gen::<__m256i>(hay, base, out)
    }
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn find_avx2(hay: &[u8], needle: &[u8]) -> Option<usize> {
        find_gen::<__m256i, false>(hay, needle)
    }
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn find_ascii_ci_avx2(hay: &[u8], needle: &[u8]) -> Option<usize> {
        find_gen::<__m256i, true>(hay, needle)
    }
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn is_ascii_avx2(src: &[u8]) -> bool {
        is_ascii_gen::<__m256i>(src)
    }

    // --- Dispatch ---

    pub(super) fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
        if use_avx2() {
            // SAFETY: AVX2 was detected at runtime by `use_avx2`.
            unsafe { find_byte_avx2(hay, needle) }
        } else {
            // SAFETY: SSE2 is baseline on x86_64.
            unsafe { find_byte_sse2(hay, needle) }
        }
    }
    pub(super) fn rfind_byte(hay: &[u8], needle: u8) -> Option<usize> {
        if use_avx2() {
            // SAFETY: AVX2 detected at runtime.
            unsafe { rfind_byte_avx2(hay, needle) }
        } else {
            // SAFETY: SSE2 is baseline.
            unsafe { rfind_byte_sse2(hay, needle) }
        }
    }
    pub(super) fn count_byte(hay: &[u8], needle: u8) -> usize {
        if use_avx2() {
            // SAFETY: AVX2 detected at runtime.
            unsafe { count_byte_avx2(hay, needle) }
        } else {
            // SAFETY: SSE2 is baseline.
            unsafe { count_byte_sse2(hay, needle) }
        }
    }
    pub(super) fn newline_positions(hay: &[u8], base: usize, out: &mut Vec<usize>) {
        if use_avx2() {
            // SAFETY: AVX2 detected at runtime.
            unsafe { newline_positions_avx2(hay, base, out) }
        } else {
            // SAFETY: SSE2 is baseline.
            unsafe { newline_positions_sse2(hay, base, out) }
        }
    }
    pub(super) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        if use_avx2() {
            // SAFETY: AVX2 detected at runtime.
            unsafe { find_avx2(hay, needle) }
        } else {
            // SAFETY: SSE2 is baseline.
            unsafe { find_sse2(hay, needle) }
        }
    }
    pub(super) fn find_ascii_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
        if use_avx2() {
            // SAFETY: AVX2 detected at runtime.
            unsafe { find_ascii_ci_avx2(hay, needle) }
        } else {
            // SAFETY: SSE2 is baseline.
            unsafe { find_ascii_ci_sse2(hay, needle) }
        }
    }
    pub(super) fn is_ascii(src: &[u8]) -> bool {
        if use_avx2() {
            // SAFETY: AVX2 detected at runtime.
            unsafe { is_ascii_avx2(src) }
        } else {
            // SAFETY: SSE2 is baseline.
            unsafe { is_ascii_sse2(src) }
        }
    }
}

// ---------------------------------------------------------------------------
// Non-x86_64: scalar fallbacks
// ---------------------------------------------------------------------------
#[cfg(not(target_arch = "x86_64"))]
mod imp {
    pub(super) fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
        hay.iter().position(|&b| b == needle)
    }
    pub(super) fn rfind_byte(hay: &[u8], needle: u8) -> Option<usize> {
        hay.iter().rposition(|&b| b == needle)
    }
    pub(super) fn count_byte(hay: &[u8], needle: u8) -> usize {
        hay.iter().filter(|&&b| b == needle).count()
    }
    pub(super) fn newline_positions(hay: &[u8], base: usize, out: &mut Vec<usize>) {
        for (i, &b) in hay.iter().enumerate() {
            if b == b'\n' {
                out.push(base + i);
            }
        }
    }
    pub(super) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() {
            return Some(0);
        }
        hay.windows(needle.len()).position(|w| w == needle)
    }
    pub(super) fn find_ascii_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() {
            return Some(0);
        }
        hay.windows(needle.len())
            .position(|w| w.eq_ignore_ascii_case(needle))
    }
    pub(super) fn is_ascii(src: &[u8]) -> bool {
        src.is_ascii()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift64 PRNG (no external crates).
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn bytes(&mut self, len: usize, alphabet: &[u8]) -> Vec<u8> {
            (0..len)
                .map(|_| alphabet[self.below(alphabet.len())])
                .collect()
        }
    }

    // ---- naive scalar references ----
    fn ref_find_byte(h: &[u8], n: u8) -> Option<usize> {
        h.iter().position(|&b| b == n)
    }
    fn ref_rfind_byte(h: &[u8], n: u8) -> Option<usize> {
        (0..h.len()).rev().find(|&i| h[i] == n)
    }
    fn ref_count(h: &[u8], n: u8) -> usize {
        let mut c = 0;
        for &b in h {
            if b == n {
                c += 1;
            }
        }
        c
    }
    fn ref_newlines(h: &[u8], base: usize) -> Vec<usize> {
        h.iter()
            .enumerate()
            .filter(|(_, &b)| b == b'\n')
            .map(|(i, _)| base + i)
            .collect()
    }
    fn ref_find(h: &[u8], n: &[u8]) -> Option<usize> {
        if n.is_empty() {
            return Some(0);
        }
        if n.len() > h.len() {
            return None;
        }
        (0..=h.len() - n.len()).find(|&i| &h[i..i + n.len()] == n)
    }
    fn ref_find_ci(h: &[u8], n: &[u8]) -> Option<usize> {
        if n.is_empty() {
            return Some(0);
        }
        if n.len() > h.len() {
            return None;
        }
        (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
    }
    fn ref_crlf_to_lf(s: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        for i in 0..s.len() {
            if !(s[i] == b'\r' && s.get(i + 1) == Some(&b'\n')) {
                o.push(s[i]);
            }
        }
        o
    }
    fn ref_lf_to_crlf(s: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        for &b in s {
            if b == b'\n' {
                o.push(b'\r');
            }
            o.push(b);
        }
        o
    }

    /// A table of kernel entry points, so one suite can exercise a specific code path.
    struct Kernels {
        find_byte: unsafe fn(&[u8], u8) -> Option<usize>,
        rfind_byte: unsafe fn(&[u8], u8) -> Option<usize>,
        count_byte: unsafe fn(&[u8], u8) -> usize,
        newline_positions: unsafe fn(&[u8], usize, &mut Vec<usize>),
        find: unsafe fn(&[u8], &[u8]) -> Option<usize>,
        find_ascii_ci: unsafe fn(&[u8], &[u8]) -> Option<usize>,
        is_ascii: unsafe fn(&[u8]) -> bool,
    }

    /// The public API, which dispatches on its own.
    fn public_kernels() -> Kernels {
        // Safe fns coerce to unsafe fn pointers; the public API is safe to call.
        Kernels {
            find_byte: |h, n| find_byte(h, n),
            rfind_byte: |h, n| rfind_byte(h, n),
            count_byte: |h, n| count_byte(h, n),
            newline_positions: |h, b, o| newline_positions(h, b, o),
            find: |h, n| find(h, n),
            find_ascii_ci: |h, n| find_ascii_ci(h, n),
            is_ascii: |h| is_ascii(h),
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn sse2_kernels() -> Kernels {
        Kernels {
            find_byte: imp::find_byte_sse2,
            rfind_byte: imp::rfind_byte_sse2,
            count_byte: imp::count_byte_sse2,
            newline_positions: imp::newline_positions_sse2,
            find: imp::find_sse2,
            find_ascii_ci: imp::find_ascii_ci_sse2,
            is_ascii: imp::is_ascii_sse2,
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn avx2_kernels() -> Option<Kernels> {
        if !std::is_x86_feature_detected!("avx2") {
            return None;
        }
        Some(Kernels {
            find_byte: imp::find_byte_avx2,
            rfind_byte: imp::rfind_byte_avx2,
            count_byte: imp::count_byte_avx2,
            newline_positions: imp::newline_positions_avx2,
            find: imp::find_avx2,
            find_ascii_ci: imp::find_ascii_ci_avx2,
            is_ascii: imp::is_ascii_avx2,
        })
    }

    const ALPHABETS: &[&[u8]] = &[b"ab\n\r", b"abcdefgh", b"aAbB\n-", b" \n\r\t\x7f\x80\xff"];

    /// Runs every kernel against the references on many inputs.
    fn run_suite(k: &Kernels) {
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        for &alphabet in ALPHABETS {
            for len in 0..300 {
                for off in 0..4 {
                    // Slice at `off` so the vector loads are unaligned.
                    let mut buf = vec![0u8; off + len + 1];
                    buf[off..off + len].copy_from_slice(&rng.bytes(len, alphabet));
                    let hay = &buf[off..off + len];

                    for &needle in &[b'\n', b'\r', b'a', b'A', 0x80u8] {
                        // SAFETY: test-only; the table only holds AVX2 kernels after detection.
                        unsafe {
                            assert_eq!((k.find_byte)(hay, needle), ref_find_byte(hay, needle));
                            assert_eq!((k.rfind_byte)(hay, needle), ref_rfind_byte(hay, needle));
                            assert_eq!((k.count_byte)(hay, needle), ref_count(hay, needle));
                        }
                    }

                    let base = rng.below(1000);
                    let mut got = vec![7usize];
                    // SAFETY: as above.
                    unsafe { (k.newline_positions)(hay, base, &mut got) };
                    let mut want = vec![7usize];
                    want.extend(ref_newlines(hay, base));
                    assert_eq!(got, want, "newline_positions len={len} off={off}");

                    // SAFETY: as above.
                    let ascii = unsafe { (k.is_ascii)(hay) };
                    assert_eq!(ascii, hay.is_ascii(), "is_ascii len={len} off={off}");

                    // Needles: random ones, ones copied from the haystack, and the empty one.
                    let mut needles: Vec<Vec<u8>> = Vec::new();
                    for &nl in &[0usize, 1, 2, 3, 5, 8, 16, 31, 32, 33, 65] {
                        needles.push(rng.bytes(nl, alphabet));
                        if nl <= len {
                            let s = rng.below(len - nl + 1);
                            needles.push(hay[s..s + nl].to_vec());
                        }
                    }
                    // Needle longer than the haystack.
                    needles.push(rng.bytes(len + 1, alphabet));
                    // Needle sitting at the very end of the haystack.
                    for nl in 1..=len.min(40) {
                        needles.push(hay[len - nl..].to_vec());
                    }

                    for nd in &needles {
                        // SAFETY: as above.
                        unsafe {
                            assert_eq!(
                                (k.find)(hay, nd),
                                ref_find(hay, nd),
                                "find len={len} off={off} nd={nd:?}"
                            );
                            assert_eq!(
                                (k.find_ascii_ci)(hay, nd),
                                ref_find_ci(hay, nd),
                                "find_ascii_ci len={len} off={off} nd={nd:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Non-ASCII byte placed at every position, plus a fully ASCII buffer.
    fn run_is_ascii(k: &Kernels) {
        for len in 0..300 {
            let mut buf = vec![b'a'; len];
            // SAFETY: test-only, see run_suite.
            assert!(unsafe { (k.is_ascii)(&buf) });
            for pos in 0..len {
                buf[pos] = 0x80;
                assert!(!unsafe { (k.is_ascii)(&buf) }, "high bit at {pos} of {len}");
                buf[pos] = 0x7F;
                assert!(unsafe { (k.is_ascii)(&buf) });
            }
        }
    }

    #[test]
    fn public_api_matches_reference() {
        run_suite(&public_kernels());
        run_is_ascii(&public_kernels());
    }

    #[test]
    fn public_crlf_and_lf_match_reference() {
        let mut rng = XorShift(0xDEAD_BEEF_1234_5678);
        for &alphabet in &[&b"ab\r\n"[..], &b"\r\n\r\n\x80x"[..]] {
            for len in 0..300 {
                for off in 0..3 {
                    let mut buf = vec![0u8; off + len];
                    buf[off..].copy_from_slice(&rng.bytes(len, alphabet));
                    let s = &buf[off..];
                    assert_eq!(crlf_to_lf(s), ref_crlf_to_lf(s), "crlf len={len}");
                    assert_eq!(lf_to_crlf(s), ref_lf_to_crlf(s), "lf len={len}");
                }
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn sse2_path_matches_reference() {
        run_suite(&sse2_kernels());
        run_is_ascii(&sse2_kernels());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx2_path_matches_reference() {
        match avx2_kernels() {
            Some(k) => {
                run_suite(&k);
                run_is_ascii(&k);
            }
            None => eprintln!("avx2 not available on this host; skipping"),
        }
    }

    #[test]
    fn find_edge_cases() {
        assert_eq!(find(b"abc", b""), Some(0));
        assert_eq!(find(b"", b""), Some(0));
        assert_eq!(find(b"", b"a"), None);
        assert_eq!(find(b"ab", b"abc"), None, "needle longer than haystack");
        let long_x = vec![b'x'; 300];
        assert_eq!(find(&long_x, b"xy"), None);
        // Match at the very last byte.
        let mut h = vec![b'.'; 200];
        h.extend_from_slice(b"needle");
        assert_eq!(find(&h, b"needle"), Some(200));
        assert_eq!(find(&h, b"dle"), Some(203));
        // Case-insensitive, match at the end.
        assert_eq!(find_ascii_ci(&h, b"NEEDLE"), Some(200));
        // Overlapping candidates: first hit must be the leftmost.
        assert_eq!(find(b"aaaaaab", b"aab"), Some(4));
    }

    #[test]
    fn find_byte_basics() {
        assert_eq!(find_byte(b"", b'a'), None);
        assert_eq!(rfind_byte(b"", b'a'), None);
        assert_eq!(rfind_byte(b"abcabc", b'a'), Some(3));
        assert_eq!(count_byte(b"", b'\n'), 0);
        assert_eq!(count_byte(b"\n\n\nx\n", b'\n'), 4);
    }

    #[test]
    fn newline_positions_appends_with_base() {
        let mut out = vec![99];
        newline_positions(b"a\nbb\n", 10, &mut out);
        assert_eq!(out, vec![99, 11, 14]);
    }

    #[test]
    fn crlf_conversion_edge_cases() {
        assert_eq!(crlf_to_lf(b"\r\n"), b"\n");
        assert_eq!(crlf_to_lf(b"\r\r\n"), b"\r\n");
        assert_eq!(crlf_to_lf(b"a\r"), b"a\r");
        assert_eq!(crlf_to_lf(b"\r"), b"\r");
        assert_eq!(crlf_to_lf(b""), b"");
        assert_eq!(lf_to_crlf(b"\n"), b"\r\n");
        assert_eq!(lf_to_crlf(b"\r\n"), b"\r\r\n");
        assert_eq!(lf_to_crlf(b"no newline"), b"no newline");
        // Round trip of a consistently-CRLF buffer.
        let crlf = b"one\r\ntwo\r\n\r\nthree";
        assert_eq!(lf_to_crlf(&crlf_to_lf(crlf)), crlf);
    }
}
