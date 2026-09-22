//! Core SSP symbol operations — k ↔ (op, i, j) bijection per B.2.1
//! ssp4_local_v44.py lines 709-832

use std::collections::HashMap;

/// Convert (op, i, j) → super-symbol index k (1-based).
/// op: "SUM" or "DIFF"
/// i, j: 1-based element indices in S (j ≤ i for SUM; j < i for DIFF)
pub fn rank_symbol(op: &str, i: u64, j: u64) -> Result<u64, &'static str> {
    if i < 1 || j < 1 {
        return Err("i and j must be >= 1");
    }
    if i == 1 {
        if op != "SUM" || j != 1 {
            return Err("i=1 only allows SUM(1,1)");
        }
        return Ok(1);
    }
    let base = (i - 1) * (i - 1);
    if op == "DIFF" {
        if j >= i {
            return Err("DIFF requires j < i");
        }
        Ok(base + (i - j))
    } else {
        // SUM
        if j > i {
            return Err("SUM requires j <= i");
        }
        Ok(base + (i - 1) + j)
    }
}

/// Convert super-symbol index k (1-based) → (op, i, j).
/// Returns (op_str, i, j) where i,j are 1-based.
pub fn unrank_symbol(k: u64) -> Result<(&'static str, u64, u64), &'static str> {
    if k < 1 {
        return Err("k must be >= 1");
    }
    let i = (f64::sqrt((k - 1) as f64) as u64) + 1;
    let base = (i - 1) * (i - 1);
    let pos = k - base; // 1-based position within i-band
    if i == 1 {
        return Ok(("SUM", 1, 1));
    }
    if pos <= i - 1 {
        Ok(("DIFF", i, i - pos))
    } else {
        Ok(("SUM", i, pos - (i - 1)))
    }
}

/// Recover even number n from super-symbol k and sequence S (0-based array).
/// S must support indexing: S[i-1].
pub fn decode_symbol(k: u64, s: &[u64]) -> Result<u64, &'static str> {
    let (op, i, j) = unrank_symbol(k)?;
    let i = i as usize;
    let j = j as usize;
    if i == 0 || j == 0 || i > s.len() || j > s.len() {
        return Err("index out of range in decode_symbol");
    }
    let si = s[i - 1];
    let sj = s[j - 1];
    Ok(if op == "SUM" { si + sj } else { si - sj })
}

/// Find minimal super-symbol k such that n = S[i] op S[j].
/// S: sorted sequence of special primes (0-based array).
/// S_index: optional HashMap for O(1) value→index lookup.
/// Returns (k, op, 1-based i, 1-based j) or None.
///
/// Uses two-pointer SUM + hash-based DIFF.
pub fn find_symbol(
    n: u64,
    s: &[u64],
    s_index: Option<&HashMap<u64, usize>>,
) -> Option<(u64, &'static str, u64, u64)> {
    if n == 0 {
        return None;
    }
    let len = s.len();
    if len == 0 {
        return None;
    }

    // Build index on demand
    let index: HashMap<u64, usize> = if let Some(idx) = s_index {
        idx.iter().map(|(&k, &v)| (k, v)).collect()
    } else {
        s.iter()
            .enumerate()
            .map(|(i, &v)| (v, i))
            .collect()
    };

    // Phase 1: two-pointer SUM
    // SUM(i,j): n = S[i] + S[j], j <= i
    // k = (i-1)² + (i-1) + j
    let mut best_k: Option<u64> = None;
    let mut best_result: Option<(u64, &'static str, u64, u64)> = None;

    // Binary search for upper bound
    let mut hi_lim = len.saturating_sub(1);
    for (idx, &val) in s.iter().enumerate() {
        if val >= n {
            hi_lim = idx.saturating_sub(1);
            break;
        }
        hi_lim = idx;
    }

    let mut lo = 0usize;
    let mut hi = hi_lim.min(len - 1);
    while lo <= hi {
        let i_idx = hi + 1; // 1-based
        let i_sym = i_idx as u64;
        // min possible SUM k = i² - i + 1
        let k_min = i_sym * i_sym - i_sym + 1;
        if let Some(bk) = best_k {
            if k_min >= bk {
                break;
            }
        }
        let sum = s[lo] + s[hi];
        if sum == n as u64 {
            let j_sym = (lo + 1) as u64;
            let k = (i_sym - 1) * (i_sym - 1) + (i_sym - 1) + j_sym;
            if best_k.is_none() || k < best_k.unwrap() {
                best_k = Some(k);
                best_result = Some((k, "SUM", i_sym, j_sym));
            }
            lo += 1;
            if hi > 0 {
                hi -= 1;
            }
        } else if sum < n as u64 {
            lo += 1;
        } else {
            if hi == 0 { break; }
            hi -= 1;
        }
    }

    // Phase 2: hash-based DIFF
    // DIFF(i,j): n = S[i] - S[j], j < i
    // k = (i-1)² + (i - j)
    for idx_i in 0..len {
        let i_sym = (idx_i + 1) as u64;
        let i_base = (i_sym - 1) * (i_sym - 1);
        if let Some(bk) = best_k {
            if i_base + 1 >= bk {
                break;
            }
        }
        if let Some(&j_idx) = index.get(&(s[idx_i].saturating_sub(n))) {
            if j_idx < idx_i {
                let j_sym = (j_idx + 1) as u64;
                let k = i_base + (i_sym - j_sym);
                if best_k.is_none() || k < best_k.unwrap() {
                    best_result = Some((k, "DIFF", i_sym, j_sym));
                }
            }
        }
    }

    best_result
}

/// Build a lookup table for value → packed_k (k*2+odd) for a given S sequence.
/// Returns a HashMap: value → packed_k.
/// Decoder reads pk, computes k=pk>>1, odd=pk&1, then val = decode_symbol(k) + odd.
/// Only for block_bits <= 16 (packed mode).
/// Mirrors Python build_lut (ssp4_local_v44.py lines 14250-14329).
pub fn build_lut(s: &[u64], block_bits: usize) -> HashMap<u64, u64> {
    let mut lut = HashMap::new();
    if block_bits > 16 {
        return lut;
    }
    let max_val = 1u64 << block_bits;
    let raw_bits = 2 + block_bits as u64;

    // Max ULEB bytes that still beats raw encoding
    let max_uleb_bytes = (raw_bits - 2) / 8;
    let max_pk = if max_uleb_bytes >= 9 {
        u64::MAX
    } else {
        (1u64 << (max_uleb_bytes * 7)).saturating_sub(1)
    };

    // k ≈ sqrt(pk), so i ≤ sqrt(max_pk)
    let max_i = (f64::sqrt(max_pk as f64) as usize + 2).min(s.len());

    for i in 0..max_i {
        let si = s[i];
        let i_sym = i as u64 + 1;
        for j in 0..=i {
            let sj = s[j];
            let j_sym = j as u64 + 1;

            // SUM(i+1, j+1): val = si + sj
            let sum = si + sj;
            if sum > 0 && sum < max_val {
                if let Ok(k) = rank_symbol("SUM", i_sym, j_sym) {
                    let pk_even = k * 2;
                    if pk_even <= max_pk {
                        lut.entry(sum).or_insert(pk_even);
                    }
                    if sum + 1 < max_val {
                        let pk_odd = k * 2 + 1;
                        if pk_odd <= max_pk {
                            lut.entry(sum + 1).or_insert(pk_odd);
                        }
                    }
                }
            }

            // DIFF(i+1, j+1) where i > j: val = si - sj
            if i > j {
                let diff = si.saturating_sub(sj);
                if diff > 0 && diff < max_val {
                    if let Ok(k) = rank_symbol("DIFF", i_sym, j_sym) {
                        let pk_even = k * 2;
                        if pk_even <= max_pk {
                            lut.entry(diff).or_insert(pk_even);
                        }
                        if diff + 1 < max_val {
                            let pk_odd = k * 2 + 1;
                            if pk_odd <= max_pk {
                                lut.entry(diff + 1).or_insert(pk_odd);
                            }
                        }
                    }
                }
            }
        }
    }

    // val=1 sentinel (Python line 14294-14296)
    if !lut.contains_key(&1) && 2 + 8 <= raw_bits as u64 {
        lut.insert(1, 1); // packed_k=1 sentinel
    }

    // Extended pair search: DIFF pairs with large i but small i-j gap
    // (Python lines 14298-14329)
    let gap_max = (max_i as u64).min(5);
    let ext_max_i = (max_i * 4).min(s.len());
    for i in max_i..ext_max_i {
        let si = s[i];
        let i_sym = i as u64 + 1;
        for gap in 1..=gap_max as usize {
            let j = i as isize - gap as isize;
            if j < 0 {
                break;
            }
            let j = j as usize;
            let sj = s[j];
            let diff = si.saturating_sub(sj);
            if diff == 0 || diff >= max_val {
                continue;
            }
            let j_sym = j as u64 + 1;
            // rank DIFF: k = (i-1)² + (i-j)
            let k = (i_sym - 1) * (i_sym - 1) + (i_sym - j_sym);
            let pk_even = k * 2;
            if pk_even <= max_pk {
                lut.entry(diff).or_insert(pk_even);
            }
        }
    }

    lut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rank_unrank_roundtrip() {
        // Test roundtrip: k = (i-1)² for DIFF, k = (i-1)²+(i-1)+j for SUM
        let cases = [
            ("SUM", 1, 1, 1),
            ("DIFF", 2, 1, 2),
            ("SUM", 2, 1, 3),
            ("SUM", 2, 2, 4),
            ("DIFF", 3, 1, 6),
            ("DIFF", 3, 2, 5),
            ("SUM", 3, 1, 7),
            ("SUM", 3, 2, 8),
            ("SUM", 3, 3, 9),
            ("DIFF", 4, 1, 12),
        ];
        for &(op, i, j, expected_k) in &cases {
            let k = rank_symbol(op, i, j).unwrap();
            assert_eq!(k, expected_k, "rank_symbol({op}, {i}, {j})");
            let (op2, i2, j2) = unrank_symbol(k).unwrap();
            assert_eq!((op2, i2, j2), (op, i, j), "unrank_symbol({k})");
        }
    }

    #[test]
    fn test_decode_symbol() {
        // S = [2, 3, 5, 7, 11, 13]
        let s = [2, 3, 5, 7, 11, 13];
        // k=1: SUM(1,1) → 2+2 = 4
        assert_eq!(decode_symbol(1, &s).unwrap(), 4);
        // k=2: DIFF(2,1) → 3-2 = 1
        assert_eq!(decode_symbol(2, &s).unwrap(), 1);
        // k=3: SUM(2,1) → 3+2 = 5
        assert_eq!(decode_symbol(3, &s).unwrap(), 5);
        // k=4: SUM(2,2) → 3+3 = 6
        assert_eq!(decode_symbol(4, &s).unwrap(), 6);
    }

    #[test]
    fn test_find_symbol() {
        let s: Vec<u64> = (2..20).filter(|&x| is_prime(x)).collect();
        // Find 10: 5+5 = SUM(3,3) → k=9
        let result = find_symbol(10, &s, None);
        assert!(result.is_some());
        let (k, op, _, _) = result.unwrap();
        assert_eq!(op, "SUM");
        assert_eq!(decode_symbol(k, &s).unwrap(), 10);

        // Find 7: 7 = DIFF(4,1) → 11-4 = 7
        let result = find_symbol(7, &s, None);
        assert!(result.is_some());
        let (k, _, _, _) = result.unwrap();
        assert_eq!(decode_symbol(k, &s).unwrap(), 7);

        // Find 3: 3 = SUM(2,1) → 3+2... no wait: 3 = DIFF(3,2) → 5-2 = 3
        let result = find_symbol(3, &s, None);
        assert!(result.is_some());
        let (k, _, _, _) = result.unwrap();
        assert_eq!(decode_symbol(k, &s).unwrap(), 3);
    }

    fn is_prime(n: u64) -> bool {
        if n < 2 {
            return false;
        }
        if n % 2 == 0 {
            return n == 2;
        }
        let mut i = 3;
        while i * i <= n {
            if n % i == 0 {
                return false;
            }
            i += 2;
        }
        true
    }
}
