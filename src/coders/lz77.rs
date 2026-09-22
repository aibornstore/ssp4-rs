//! LZ77 compression — greedy match-finding with hash chains
//!
//! Format: literals + match references (offset, length)
//! Minimum match: 3 bytes, max: 130 bytes
//! Window: 256KB (increased from 64KB)
//! Chain: 512 positions per hash (increased from 100)
//!
//! Dictionary improvements:
//! - Window 64KB → 256KB for longer-distance matches
//! - Chain 100 → 512 for better match coverage
//! - 4-byte hash (32-bit) for better selectivity vs 3-byte (24-bit)
//! - Lazy matching avoids short-match traps

use std::collections::HashMap;

/// LZ77 token
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Literal(u8),
    /// Match: copy `length` bytes from `offset` positions back
    Match { offset: u32, length: u8 },
}

/// Compute 4-byte hash for LZ77 dictionary.
/// Falls back to 3-byte hash if data is too short.
/// 4-byte hash provides better selectivity (fewer false positives).
fn compute_hash(data: &[u8], i: usize) -> u32 {
    if i + 4 <= data.len() {
        // Full 4-byte hash
        ((data[i] as u32) << 24) 
            | ((data[i + 1] as u32) << 16) 
            | ((data[i + 2] as u32) << 8) 
            | (data[i + 3] as u32)
    } else if i + 3 <= data.len() {
        // Fallback to 3-byte hash
        ((data[i] as u32) << 16) 
            | ((data[i + 1] as u32) << 8) 
            | (data[i + 2] as u32)
    } else if i + 2 <= data.len() {
        // 2-byte hash
        ((data[i] as u32) << 8) | (data[i + 1] as u32)
    } else if i + 1 <= data.len() {
        // 1-byte hash
        data[i] as u32
    } else {
        0
    }
}

/// Find the best match for data at position i, returning full match info
/// Used internally for lazy matching decisions
fn find_match_info(data: &[u8], chain: &HashMap<u32, Vec<u32>>, i: usize, max_match: usize, window_size: usize) -> Option<(u32, usize, usize)> {
    if i + 3 > data.len() {
        return None;
    }

    let h = compute_hash(data, i);
    let positions = chain.get(&h)?;

    let mut best_offset = 0u32;
    let mut best_length = 0usize;
    let mut best_pos = 0usize;

    for &pos in positions {
        let pos_usize = pos as usize;
        if pos_usize >= i {
            continue;
        }
        let offset = (i - pos_usize) as u32;
        if offset as usize > window_size {
            continue;
        }
        let max_len = (i - pos_usize).min(max_match);
        let mut length = 0usize;
        let mut pi = i;
        let mut pp = pos_usize;
        while pi < data.len() && pp < data.len() && length < max_len && data[pi] == data[pp] {
            length += 1;
            pi += 1;
            pp += 1;
        }
        if length >= 3 && length > best_length {
            best_offset = offset;
            best_length = length;
            best_pos = pos_usize;
        }
        if length >= max_match {
            break;
        }
    }

    if best_length >= 3 {
        Some((best_offset, best_length, best_pos))
    } else {
        None
    }
}

/// Encode data with lazy LZ77 match-finding
///
/// Lazy matching (DEFLATE-style): if position i+1 has a match significantly longer
/// than position i, emit a literal at i and take the longer match at i+1.
///
/// Algorithm:
/// 1. Find best match at position i (length L_i)
/// 2. If no match: emit literal, advance by 1
/// 3. If match found:
///    a. If L_i >= STRONG_MATCH_THRESHOLD (8 bytes): emit match immediately
///    b. Else: check positions i+1 through i+LOOKAHEAD for longer matches
///    c. If any position j in [i+1, i+LOOKAHEAD] has match length > L_i:
///       emit literal at i, advance by 1
///    d. Else: emit match at i, advance by L_i
///
/// This avoids short-match traps on repetitive data and improves ratio.
pub fn encode(data: &[u8]) -> Vec<Token> {
    if data.is_empty() { return Vec::new(); }
    let n = data.len();
    let min_match = 3;
    let max_match = 130;
    const WINDOW_SIZE: usize = 256 * 1024; // 256KB
    const STRONG_MATCH_THRESHOLD: usize = 8; // Skip lazy check for matches >= 8 bytes
    const LOOKAHEAD: usize = 3;             // Check i+1, i+2, i+3 for longer matches

    let mut tokens = Vec::with_capacity(n / 4);
    let mut i = 0usize;
    const MAX_CHAIN_SIZE: usize = 512;

    let mut chain: HashMap<u32, Vec<u32>> = HashMap::new();

    // Pre-populate for first 4 bytes (4-byte hash)
    if n >= 4 {
        let h = compute_hash(data, 0);
        chain.insert(h, vec![0]);
    } else if n >= 3 {
        let h = compute_hash(data, 0);
        chain.insert(h, vec![0]);
    }

    while i < n {
        let remaining = n - i;
        if remaining < min_match {
            for j in i..n { tokens.push(Token::Literal(data[j])); }
            break;
        }

        let current_match = find_match_info(data, &chain, i, max_match, WINDOW_SIZE);

        match current_match {
            None => {
                // No match at position i — emit literal
                tokens.push(Token::Literal(data[i]));
                if i + 3 <= n {
                    let h2 = compute_hash(data, i);
                    chain.entry(h2).or_default().push(i as u32);
                    if let Some(vec) = chain.get_mut(&h2) {
                        if vec.len() > MAX_CHAIN_SIZE {
                            vec.drain(0..vec.len() - MAX_CHAIN_SIZE);
                        }
                    }
                }
                i += 1;
                continue;
            }
            Some((best_offset, best_length, _best_pos)) => {
                // Match found at position i with length best_length

                // Optimization: strong match — take it immediately without lazy check
                if best_length >= STRONG_MATCH_THRESHOLD {
                    tokens.push(Token::Match { offset: best_offset, length: best_length as u8 });
                    let match_end = i + best_length as usize;
                    // Insert hashes for positions immediately after the match
                    for j in (match_end)..(match_end + 3).min(n) {
                        if j + 3 <= n {
                            let h2 = compute_hash(data, j);
                            chain.entry(h2).or_default().push(j as u32);
                            if let Some(vec) = chain.get_mut(&h2) {
                                if vec.len() > MAX_CHAIN_SIZE {
                                    vec.drain(0..vec.len() - MAX_CHAIN_SIZE);
                                }
                            }
                        }
                    }
                    i += best_length as usize;
                    if i % 16384 == 0 && !chain.is_empty() {
                        let cutoff = (i as u32).saturating_sub(WINDOW_SIZE as u32);
                        chain.retain(|_, v| {
                            v.retain(|&p| p >= cutoff);
                            !v.is_empty()
                        });
                    }
                    continue;
                }

                // Lazy matching: check positions i+1 through i+LOOKAHEAD for longer matches
                let lookahead_end = (i + 1 + LOOKAHEAD).min(n);
                let mut better_match_at = None;
                let mut better_match_len = best_length;

                for j in (i + 1)..lookahead_end {
                    if j + 3 > n {
                        break;
                    }
                    // Skip if this position can't have a match longer than best_length
                    // (the max possible match length from j is limited by remaining data
                    // and the distance to j from potential matches)
                    if let Some((_, len_j, _)) = find_match_info(data, &chain, j, max_match, WINDOW_SIZE) {
                        if len_j > better_match_len {
                            better_match_len = len_j;
                            better_match_at = Some(j);
                            if len_j >= STRONG_MATCH_THRESHOLD {
                                break; // Found a strong match, stop searching
                            }
                        }
                    }
                }

                // If a position in the lookahead window has a longer match,
                // emit a literal at i and advance by 1
                if let Some(_next_pos) = better_match_at {
                    if better_match_len > best_length {
                        tokens.push(Token::Literal(data[i]));
                        if i + 3 <= n {
                            let h2 = compute_hash(data, i);
                            chain.entry(h2).or_default().push(i as u32);
                            if let Some(vec) = chain.get_mut(&h2) {
                                if vec.len() > MAX_CHAIN_SIZE {
                                    vec.drain(0..vec.len() - MAX_CHAIN_SIZE);
                                }
                            }
                        }
                        i += 1;
                        continue;
                    }
                }

                // No better match found in lookahead — emit the match at i
                tokens.push(Token::Match { offset: best_offset, length: best_length as u8 });
                let match_end = i + best_length as usize;
                for j in (match_end)..(match_end + 3).min(n) {
                    if j + 3 <= n {
                        let h2 = compute_hash(data, j);
                        chain.entry(h2).or_default().push(j as u32);
                        if let Some(vec) = chain.get_mut(&h2) {
                            if vec.len() > MAX_CHAIN_SIZE {
                                vec.drain(0..vec.len() - MAX_CHAIN_SIZE);
                            }
                        }
                    }
                }
                i += best_length as usize;

                if i % 16384 == 0 && !chain.is_empty() {
                    let cutoff = (i as u32).saturating_sub(WINDOW_SIZE as u32);
                    chain.retain(|_, v| {
                        v.retain(|&p| p >= cutoff);
                        !v.is_empty()
                    });
                }
            }
        }
    }
    tokens
}

/// Decode LZ77 tokens
pub fn decode(tokens: &[Token]) -> Vec<u8> {
    let mut out = Vec::new();
    for token in tokens {
        match token {
            Token::Literal(b) => out.push(*b),
            Token::Match { offset, length } => {
                let base = out.len().saturating_sub(*offset as usize);
                for k in 0..*length as usize {
                    let src = base + k;
                    if src < out.len() {
                        out.push(out[src]);
                    }
                }
            }
        }
    }
    out
}

use super::bit_io::{BitReader, BitWriter};

const RICE_FORMAT_MAGIC: u8 = 0x4c;
const RICE_FORMAT_VERSION: u8 = 1;

/// Encode tokens to bytes.
/// Format: [magic][version][k][token_count ULEB][bitstream]
/// Bitstream tokens: 1-bit tag, literal value or Rice(offset, k) + length.
pub fn tokens_to_bytes(tokens: &[Token]) -> Vec<u8> {
    let mut bw = BitWriter::new();
    bw.write_bits(RICE_FORMAT_MAGIC as u64, 8);
    bw.write_bits(RICE_FORMAT_VERSION as u64, 8);

    let k = choose_rice_k(tokens);
    bw.write_bits(k as u64, 8);
    bw.write_uleb(tokens.len() as u64);

    for token in tokens {
        match token {
            Token::Literal(b) => {
                bw.write_bit(0);
                bw.write_bits(*b as u64, 8);
            }
            Token::Match { offset, length } => {
                bw.write_bit(1);
                bw.write_rice(*offset as u64, k);
                bw.write_bits(*length as u64, 8);
            }
        }
    }

    bw.flush()
}

fn choose_rice_k(tokens: &[Token]) -> usize {
    let offsets: Vec<u32> = tokens.iter().filter_map(|token| match token {
        Token::Literal(_) => None,
        Token::Match { offset, .. } => Some(*offset),
    }).collect();

    if offsets.is_empty() {
        return 4;
    }

    let mut best_k = 0usize;
    let mut best_bits = usize::MAX;

    for k in 0..=16 {
        let bits = offsets.iter().map(|&offset| {
            let quotient = offset >> k;
            (quotient as usize) + 1 + k
        }).sum::<usize>();

        if bits < best_bits {
            best_bits = bits;
            best_k = k;
        }
    }

    best_k
}

/// Decode bytes to tokens.
pub fn bytes_to_tokens(data: &[u8]) -> Vec<Token> {
    let mut br = BitReader::new(data);
    let magic = br.read_bits(8).unwrap() as u8;
    if magic != RICE_FORMAT_MAGIC {
        return old_bytes_to_tokens(data);
    }

    let version = br.read_bits(8).unwrap() as u8;
    if version != RICE_FORMAT_VERSION {
        return old_bytes_to_tokens(data);
    }

    let k = br.read_bits(8).unwrap() as usize;
    let token_count = br.read_uleb().unwrap() as usize;
    let mut tokens = Vec::with_capacity(token_count);

    for _ in 0..token_count {
        let tag = br.read_bit().unwrap();
        if tag == 0 {
            tokens.push(Token::Literal(br.read_bits(8).unwrap() as u8));
        } else {
            let offset = br.read_rice(k).unwrap() as u32;
            let length = br.read_bits(8).unwrap() as u8;
            tokens.push(Token::Match { offset, length });
        }
    }

    tokens
}

fn old_bytes_to_tokens(data: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b == 0x80 {
            if i + 1 < data.len() {
                tokens.push(Token::Literal(data[i + 1]));
                i += 2;
            } else {
                break;
            }
        } else if b == 0x81 {
            if i + 5 < data.len() {
                let offset = (data[i + 1] as u32)
                    | ((data[i + 2] as u32) << 8)
                    | ((data[i + 3] as u32) << 16)
                    | ((data[i + 4] as u32) << 24);
                let length = data[i + 5];
                if offset > 0 && length >= 3 {
                    tokens.push(Token::Match { offset, length });
                }
                i += 6;
            } else {
                break;
            }
        } else {
            tokens.push(Token::Literal(b));
            i += 1;
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lz77_token_serialization_roundtrip() {
        let test_cases = vec![
            b"AAAA".to_vec(),
            b"ABABABAB".to_vec(),
            b"The quick brown fox".to_vec(),
            b"Hello World! Hello World!".to_vec(),
            b"\r\n\r\n\r\n      Text\r\n".to_vec(),
        ];

        for data in test_cases {
            let tokens = encode(&data);
            let serialized = tokens_to_bytes(&tokens);
            let tokens2 = bytes_to_tokens(&serialized);
            let decoded = decode(&tokens2);

            assert_eq!(data, decoded, "Token serialization failed for {:?}", String::from_utf8_lossy(&data));
        }
    }

    #[test]
    fn test_lz77_alice29_segment() {
        let segment = b"fact, I\r\ndidn't know that cat";
        let tokens = encode(segment);
        let bytes = tokens_to_bytes(&tokens);
        let tokens2 = bytes_to_tokens(&bytes);
        let decoded = decode(&tokens2);
        assert_eq!(segment.to_vec(), decoded, "LZ77 failed on alice29 segment");
    }

    #[test]
    fn test_lz77_encode_only_roundtrip() {
        let data: Vec<u8> = (0..=255).chain(0..=255).chain(0..=255).take(152089).collect();
        let tokens = encode(&data);
        let decoded = decode(&tokens);
        assert_eq!(data, decoded, "LZ77 encode->decode failed on full data");
    }

    #[test]
    fn test_lz77_long_pattern() {
        let pattern = b"The quick brown fox jumps over the lazy dog. fact, I\r\ndidn't know that cat";
        let tokens = encode(pattern);
        let decoded = decode(&tokens);
        assert_eq!(pattern.to_vec(), decoded, "LZ77 failed on long pattern");
    }

    #[test]
    fn test_lz77_empty() {
        let tokens = encode(b"");
        assert!(tokens.is_empty());
        assert_eq!(b"".to_vec(), decode(&tokens));
    }

    #[test]
    fn test_lz77_roundtrip() {
        let data = b"Hello World! World! World! The quick brown fox.";
        let tokens = encode(data);
        let decoded = decode(&tokens);
        assert_eq!(data.to_vec(), decoded);
    }

    #[test]
    fn test_lz77_repeated() {
        let data: Vec<u8> = b"The quick brown fox ".iter()
            .cycle().take(500).copied().collect();
        let tokens = encode(&data);
        let decoded = decode(&tokens);
        assert_eq!(data, decoded);
        let bytes = tokens_to_bytes(&tokens);
        let ratio = 100.0 * bytes.len() as f64 / data.len() as f64;
        let mat = tokens.iter().filter(|t| matches!(t, Token::Match { .. })).count();
        println!("Repeated 500B: {} matches, ratio={:.1}%", mat, ratio);
        assert!(mat > 0, "Should find matches");
    }

    #[test]
    fn test_lz77_aaa() {
        let data: Vec<u8> = b"AAAAAAAAAA".iter().cycle().take(1000).copied().collect();
        let tokens = encode(&data);
        let decoded = decode(&tokens);
        assert_eq!(data, decoded);
        let bytes = tokens_to_bytes(&tokens);
        let ratio = 100.0 * bytes.len() as f64 / data.len() as f64;
        let mat = tokens.iter().filter(|t| matches!(t, Token::Match { .. })).count();
        println!("AAA 1000x: {} matches, ratio={:.1}%", mat, ratio);
        assert!(mat > 0, "Should find matches");
    }

    #[test]
    fn test_lz77_tokens_bytes() {
        let data = b"Test data. Test data. Test data. ";
        let tokens = encode(data);
        let bytes = tokens_to_bytes(&tokens);
        let decoded = bytes_to_tokens(&bytes);
        assert_eq!(tokens, decoded);
        assert_eq!(data.to_vec(), decode(&decoded));
    }
}

/// Baseline LZ77: greedy only, original parameters (64KB window, 100 chain)
/// Used for benchmark comparison with optimized lazy matching version.
mod baseline {
    use super::*;

    pub fn encode_greedy(data: &[u8]) -> Vec<Token> {
        if data.is_empty() { return Vec::new(); }
        let n = data.len();
        let min_match = 3;
        let max_match = 130;
        const WINDOW_SIZE: usize = 64 * 1024; // 64KB original
        const MAX_CHAIN_SIZE: usize = 100;    // 100 original

        let mut tokens = Vec::with_capacity(n / 4);
        let mut i = 0usize;
        let mut chain: HashMap<u32, Vec<u32>> = HashMap::new();

        if n >= 3 {
            let h = ((data[0] as u32) << 16) | ((data[1] as u32) << 8) | (data[2] as u32);
            chain.insert(h, vec![0]);
        }

        while i < n {
            let remaining = n - i;
            if remaining < min_match {
                for j in i..n { tokens.push(Token::Literal(data[j])); }
                break;
            }

            let h = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | (data[i + 2] as u32);
            let positions = match chain.get(&h) {
                Some(p) => p.clone(),
                None => Vec::new(),
            };

            let mut best_offset = 0u32;
            let mut best_length = 0usize;

            for &pos in &positions {
                if pos >= i as u32 { continue; }
                let offset = (i - pos as usize) as u32;
                if offset as usize > WINDOW_SIZE { continue; }
                let max_len = (i - pos as usize).min(max_match);
                let mut length = 0usize;
                let mut pi = i;
                let mut pp = pos as usize;
                while pi < n && pp < n && length < max_len && data[pi] == data[pp] {
                    length += 1; pi += 1; pp += 1;
                }
                if length >= 3 && length > best_length {
                    best_offset = offset;
                    best_length = length;
                }
            }

            if best_length >= 3 {
                tokens.push(Token::Match { offset: best_offset, length: best_length as u8 });
                let match_end = i + best_length as usize;
                for j in (match_end + 1)..=(match_end + 2).min(n) {
                    if j + 3 <= n {
                        let h2 = ((data[j] as u32) << 16) | ((data[j + 1] as u32) << 8) | (data[j + 2] as u32);
                        chain.entry(h2).or_default().push(j as u32);
                        if let Some(vec) = chain.get_mut(&h2) {
                            if vec.len() > MAX_CHAIN_SIZE {
                                vec.drain(0..vec.len() - MAX_CHAIN_SIZE);
                            }
                        }
                    }
                }
                i += best_length as usize;
            } else {
                tokens.push(Token::Literal(data[i]));
                if i + 3 <= n {
                    let h2 = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | (data[i + 2] as u32);
                    chain.entry(h2).or_default().push(i as u32);
                    if let Some(vec) = chain.get_mut(&h2) {
                        if vec.len() > MAX_CHAIN_SIZE {
                            vec.drain(0..vec.len() - MAX_CHAIN_SIZE);
                        }
                    }
                }
                i += 1;
            }

            if i % 16384 == 0 && !chain.is_empty() {
                let cutoff = (i as u32).saturating_sub(WINDOW_SIZE as u32);
                chain.retain(|_, v| { v.retain(|&p| p >= cutoff); !v.is_empty() });
            }
        }
        tokens
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// Generate test data of various types
    fn gen_repetitive() -> Vec<u8> {
        b"The quick brown fox jumps over the lazy dog. fact, I didn't know that cat".iter()
            .cycle().take(200_000).copied().collect()
    }

    fn gen_alice_like() -> Vec<u8> {
        // Simulate alice29.txt-like text
        let text = b"The Famines and Locusts, and the other plagues, were sent upon Egypt for the sins of its people. When the river should overflow, it did not overflow. When it should have receded, it did not recede. The land was parched with drought, and the harvests failed. The cattle lowed for water, and the people lifted up their voices to heaven.";
        text.iter().cycle().take(150_000).copied().collect()
    }

    fn gen_csv_like() -> Vec<u8> {
        // Simulate CSV with repeated patterns
        let row = b"name,age,city,country,profession,salary,department,manager,start_date,status\n";
        row.iter().cycle().take(100_000).copied().collect()
    }

    fn gen_random() -> Vec<u8> {
        let mut rng: u64 = 42;
        (0..100_000).map(|_| {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            (rng >> 33) as u8
        }).collect()
    }

    fn gen_aaa() -> Vec<u8> {
        b"A".iter().cycle().take(50_000).copied().collect()
    }

    fn gen_sparse() -> Vec<u8> {
        // Sparse repeats: pattern repeats every 10KB
        let pattern = b"ABCD1234EFGH5678IJKL9012MNOP3456QRST7890";
        let mut data = Vec::with_capacity(100_000);
        for _ in 0..1250 {
            data.extend_from_slice(pattern);
        }
        data
    }

    /// Run benchmark for one data type
    fn bench_data(name: &str, data: &[u8]) {
        // Baseline (greedy, 64KB, 100 chain)
        let start = std::time::Instant::now();
        let baseline_tokens = baseline::encode_greedy(data);
        let baseline_time = start.elapsed();
        let baseline_bytes = tokens_to_bytes(&baseline_tokens);
        let baseline_ratio = 100.0 * baseline_bytes.len() as f64 / data.len() as f64;
        let baseline_matches = baseline_tokens.iter().filter(|t| matches!(t, Token::Match { .. })).count();

        // Optimized (lazy, 256KB, 512 chain)
        let start = std::time::Instant::now();
        let opt_tokens = encode(data);
        let opt_time = start.elapsed();
        let opt_bytes = tokens_to_bytes(&opt_tokens);
        let opt_ratio = 100.0 * opt_bytes.len() as f64 / data.len() as f64;
        let opt_matches = opt_tokens.iter().filter(|t| matches!(t, Token::Match { .. })).count();

        // Verify correctness
        assert_eq!(decode(&baseline_tokens), decode(&opt_tokens));
        assert_eq!(decode(&baseline_tokens), data.to_vec());

        println!("\n=== {} ({} bytes) ===", name, data.len());
        println!("{:<12} {:>10} {:>8} {:>10} {:>10}", "", "time", "ratio", "matches", "size");
        println!("{:<12} {:>10.3}ms {:>7.2}% {:>10} {:>10}B",
            "Baseline:", baseline_time.as_secs_f64() * 1000.0, baseline_ratio, baseline_matches, baseline_bytes.len());
        println!("{:<12} {:>10.3}ms {:>7.2}% {:>10} {:>10}B",
            "Optimized:", opt_time.as_secs_f64() * 1000.0, opt_ratio, opt_matches, opt_bytes.len());

        let ratio_improvement = baseline_ratio - opt_ratio;
        let time_change = (opt_time.as_secs_f64() - baseline_time.as_secs_f64()) / baseline_time.as_secs_f64() * 100.0;
        if ratio_improvement > 0.0 {
            println!("  Ratio:  -{:.2}pp (better)", ratio_improvement);
        } else {
            println!("  Ratio: +{:.2}pp (worse)", -ratio_improvement);
        }
        println!("  Speed:  {:+.1}% ({})", time_change,
            if time_change < 0.0 { "faster" } else { "slower" });
    }

    #[test]
    fn bench_lazy_vs_greedy() {
        println!("\n========================================");
        println!("LZ77 BENCHMARK: Lazy Matching vs Greedy Baseline");
        println!("========================================");
        println!("Baseline: greedy, 64KB window, 100 chain, no lazy");
        println!("Optimized: lazy, 256KB window, 512 chain, strong_match=8");

        bench_data("Repetitive text", &gen_repetitive());
        bench_data("Alice-like", &gen_alice_like());
        bench_data("CSV-like", &gen_csv_like());
        bench_data("Sparse repeats", &gen_sparse());
        bench_data("AAAA...", &gen_aaa());
        bench_data("Random", &gen_random());

        println!("\n========================================");
    }
}
