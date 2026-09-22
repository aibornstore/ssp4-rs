//! Core SSP codec — encodes byte sequences using special prime sequences
//! Corresponds to ssp4_local_v44.py encode_data/decode_data core SSP path (lines ~14043-14582)
//!
//! Format: SSP5 magic + flags + block_bits + ULEB(m,r,len,nblocks) + body
//! Body: variable-length tag encoding with ULEB k-values for symbols

use std::collections::HashMap;

use super::bit_io::{BitReader, BitWriter};
use super::range_coder::{RangeDecoder, RangeEncoder};
use super::symbols::{build_lut, decode_symbol, find_symbol};

/// SSP5 codec magic bytes
pub const SSP5_MAGIC: &[u8; 4] = b"SSP5";
/// SSP5 codec version (v3: odd-bit separation)
pub const SSP5_VERSION: u8 = 3;

/// Encode data using an SSP family (special sequence of primes).
///
/// - `data`: raw bytes to encode
/// - `s`: special prime sequence (0-based array)
/// - `block_bits`: bits per block (8, 16, 24, 32)
/// - `delta`: if true, apply subtraction-delta before encoding
/// Returns encoded archive bytes.
pub fn encode(data: &[u8], s: &[u64], block_bits: usize, delta: bool) -> Vec<u8> {
    let block_bytes = block_bits / 8;
    let data_len = data.len();
    let num_blocks = (data_len + block_bytes - 1) / block_bytes;
    if data_len == 0 {
        return encode_empty(block_bits);
    }

    let max_block_val = (1u64 << block_bits) - 1;

    // Pad data to full blocks
    let mut padded = data.to_vec();
    let pad_len = num_blocks * block_bytes - data_len;
    padded.extend(std::iter::repeat(0).take(pad_len));

    // Adaptive format: packed (bb<=16) or separate-odd (bb>=32)
    let use_odd_sep = block_bits >= 32;
    let raw_bits = 2 + block_bits as u64;

    // LUT for bb <= 16 (packed mode: val → packed_k)
    let lut: Option<HashMap<u64, u64>> = if block_bits <= 16 {
        Some(build_lut(s, block_bits))
    } else {
        None
    };

    // Encode each block as (tag, value)
    let mut symbols: Vec<(u8, u64)> = Vec::with_capacity(num_blocks * 2);
    let mut _encoded_count = 0u32;
    let mut _raw_count = 0u32;
    let mut _zero_count = 0u32;
    let mut prev_n: u64 = 0;

    for bi in 0..num_blocks {
        let start = bi * block_bytes;
        let end = (start + block_bytes).min(padded.len());
        let chunk = &padded[start..end];
        let mut n = 0u64;
        // Big-endian: MSB first — iterate from first to last byte
        for &b in chunk.iter() {
            n = (n << 8) | b as u64;
        }
        // Zero-pad high bytes if chunk < block_bytes
        // (n is already 0, shifting left by 8 adds 0)

        let val = if delta {
            let d = (n.wrapping_sub(prev_n)) & max_block_val;
            prev_n = n;
            d
        } else {
            n
        };

        if val == 0 {
            symbols.push((2, 0)); // ZERO tag
            _zero_count += 1;
            continue;
        }

        // LUT fast path (bb <= 16)
        if let Some(ref l) = lut {
            if let Some(&pk) = l.get(&val) {
                symbols.push((0, pk)); // SYM tag, packed_k
                _encoded_count += 1;
                continue;
            }
            // Not in LUT → RAW
            symbols.push((1, val));
            _raw_count += 1;
            continue;
        }

        // Slow path: bb > 16 — per-block find_symbol
        let odd = val & 1;
        let search_val = val - odd;

        if search_val == 0 && odd == 1 {
            if use_odd_sep {
                // odd=1, k=1 → encode as SYM
                let k = 1u64;
                // Cost check: 2 + 1(bit) + uleb_bytes*8 vs raw_bits
                let uleb_bytes = 1u64; // k=1 fits in 1 byte
                if 3 + uleb_bytes * 8 <= raw_bits as u64 {
                    symbols.push((0, (odd << 32) | k)); // encode as (odd, k) in odd_sep mode
                    _encoded_count += 1;
                    continue;
                }
            } else {
                // packed mode: k*2+1 = 1 → k=0... no, k=1 gives pk=3
                // For odd=1 and search_val=0: k=0 gives pk=1 (sentinel)
                let pk = 1u64; // sentinel packed_k for val=1
                symbols.push((0, pk));
                _encoded_count += 1;
                continue;
            }
        }

        if search_val > 0 {
            if let Some((k, _, _, _)) = find_symbol(search_val, s, None) {
                // Cost check: is encoding cheaper than raw?
                let uleb_bytes = uleb_size(if use_odd_sep { k } else { k * 2 + odd });
                let cost = if use_odd_sep {
                    2 + 1 + uleb_bytes * 8 // tag + 1-bit odd + ULEB(k)
                } else {
                    2 + uleb_bytes * 8 // tag + ULEB(pk)
                };
                if cost as u64 <= raw_bits {
                    if use_odd_sep {
                        symbols.push((0, (odd << 32) | k));
                    } else {
                        symbols.push((0, k * 2 + odd));
                    }
                    _encoded_count += 1;
                    continue;
                }
            }
        }

        // Fallback: RAW encoding
        symbols.push((1, val));
        _raw_count += 1;
    }

    // Phase B: TWO-PASS compression over Phase A symbols
    // Pass 1: detect MFS (most frequent symbol) and RAW frequencies
    let mut k_freq: HashMap<u64, u32> = HashMap::new();
    let mut raw_freq: HashMap<u64, u32> = HashMap::new();
    for &(tag, val) in &symbols {
        if tag == 0 {
            *k_freq.entry(val).or_insert(0) += 1;
        } else if tag == 1 {
            *raw_freq.entry(val).or_insert(0) += 1;
        }
    }

    // MFS: most frequent SYM value (threshold >= 8)
    let mfs_val = if let Some((&k, &cnt)) = k_freq.iter().max_by_key(|&(_, c)| c) {
        if cnt >= 8 { Some(k) } else { None }
    } else {
        None
    };

    // RAW-MFS: disabled in v45 (interferes with header parsing)
    // RAW-RLE threshold
    let rle_threshold = 4;

    // Pass 2: build compressed_symbols with Phase B optimizations
    // Using separate lists: (tag, val) and (run_count for RAW-RLE)
    let mut compressed: Vec<(u8, u64)> = Vec::with_capacity(symbols.len());
    let mut raw_rle_runs: Vec<u64> = Vec::new(); // run counts parallel to compressed
    let mut i = 0;
    while i < symbols.len() {
        let (tag, val) = symbols[i];

        // SYM-MFS: replace with short tag (tag=4)
        if tag == 0 {
            if let Some(mfs) = mfs_val {
                if val == mfs {
                    compressed.push((4, 0)); // tag 4: MFS, no payload
                    raw_rle_runs.push(0);
                    i += 1;
                    continue;
                }
            }
            compressed.push((tag, val));
            raw_rle_runs.push(0);
            i += 1;
            continue;
        }

        // ZERO-RLE (tag=3): consecutive zeros
        if tag == 2 {
            let mut run = 1;
            while i + run < symbols.len() && symbols[i + run].0 == 2 {
                run += 1;
            }
            if run >= rle_threshold {
                compressed.push((3, run as u64)); // tag 3: ZERO-RLE
                raw_rle_runs.push(0);
                i += run;
                continue;
            }
            // Fall through: emit individual zeros
            for _ in 0..run {
                compressed.push((2, 0));
                raw_rle_runs.push(0);
            }
            i += run;
            continue;
        }

        // RAW-RLE (tag=5): consecutive identical RAW values
        if tag == 1 {
            let mut run = 1;
            while i + run < symbols.len()
                && symbols[i + run].0 == 1
                && symbols[i + run].1 == val
            {
                run += 1;
            }
            if run >= rle_threshold {
                compressed.push((5, val)); // tag 5: RAW-RLE
                raw_rle_runs.push(run as u64);
                i += run;
                continue;
            }
            compressed.push((1, val));
            raw_rle_runs.push(0);
            i += 1;
            continue;
        }

        compressed.push((tag, val));
        raw_rle_runs.push(0);
        i += 1;
    }

    // Check what Phase B features were used
    let rle_used = compressed.iter().any(|(t, _)| *t == 3 || *t == 5);
    let mfs_used = mfs_val.is_some() && compressed.iter().any(|(t, _)| *t == 4);

    // Build archive header
    let mut header = Vec::with_capacity(64);
    header.extend_from_slice(SSP5_MAGIC);
    header.push(SSP5_VERSION);

    // flags: bit0=delta, bit1=RLE, bit2=MFS, bit3=odd_sep, bit7=range_coded
    let flags = (if delta { 1u8 } else { 0 })
        | (if rle_used { 2u8 } else { 0 })
        | (if mfs_used { 4u8 } else { 0 })
        | (if use_odd_sep { 8u8 } else { 0 });
    header.push(flags);
    // block_bits BEFORE ULEB fields (matches decode reading order)
    header.push(block_bits as u8);
    // ULEB(m), ULEB(r), ULEB(data_len), ULEB(num_blocks)
    header.extend_from_slice(&uleb_vec(1));
    header.extend_from_slice(&uleb_vec(1));
    header.extend_from_slice(&uleb_vec(data_len as u64));
    header.extend_from_slice(&uleb_vec(num_blocks as u64));

    // MFS: write mfs_k to header AFTER ULEB fields (matches Python order)
    if mfs_used {
        if let Some(mfs) = mfs_val {
            if use_odd_sep {
                let mfs_odd = (mfs & 1) as u8;
                let mfs_k = mfs >> 1;
                header.push(mfs_odd);
                header.extend_from_slice(&uleb_vec(mfs_k));
            } else {
                header.extend_from_slice(&uleb_vec(mfs)); // packed_k
            }
        }
    }

    // Encode body: bit-packed Phase B
    let mut bw = BitWriter::new();
    // Write RLE-compact entries (matches Python bitpacked format).
    // Decode loop reads until bitstream exhausted, expanding RLE inline.
    for ((tag, value), &run) in compressed.iter().zip(raw_rle_runs.iter()) {
        match *tag {
            0 => {
                // SYM
                bw.write_bits(0, 2); // 00
                if use_odd_sep {
                    let odd = ((value >> 32) & 1) as u8;
                    let k = value & 0xFFFF_FFFF;
                    bw.write_bits(odd as u64, 1);
                    bw.write_uleb(k);
                } else {
                    bw.write_uleb(*value);
                }
            }
            1 => {
                // RAW
                bw.write_bits(1, 2); // 01
                bw.write_bits(*value, block_bits);
            }
            2 => {
                // ZERO — no payload
                bw.write_bits(2, 2); // 10
            }
            3 => {
                // ZERO-RLE: 4-bit tag 1100
                bw.write_bits(0b1100, 4);
                bw.write_uleb(*value);
            }
            4 => {
                // MFS: 4-bit tag 1101, no payload
                bw.write_bits(0b1101, 4);
            }
            5 => {
                // RAW-RLE: 4-bit tag 1110, value bits + run ULEB
                bw.write_bits(0b1110, 4);
                bw.write_bits(*value, block_bits);
                bw.write_uleb(run);
            }
            _ => {}
        }
    }

    let bitpacked_body = bw.flush();

    // Try range coding: expand RAW-RLE and AC-encode tags + RAW values
    let (rc_body, num_csymbols) = range_code_symbols(&compressed, &raw_rle_runs, block_bits, use_odd_sep);

    // Try Huffman coding: encode the expanded Phase B stream (no RLE collapse)
    // Skip ZERO-RLE (tag=3) from the stream.
    // For the extra array: store only non-zero entries as (gap, run) pairs.
    // Gap = number of normal entries since the LAST ZERO-RLE (incremental count).
    // Decoding: decrement remaining_gap for each normal symbol; when remaining_gap==0, emit ZERO-RLE.
    let mut hf_stream: Vec<usize> = Vec::new();
    let mut zero_rle_gaps: Vec<u64> = Vec::new(); // normal entries since last ZERO-RLE
    let mut zero_rle_runs: Vec<u64> = Vec::new(); // run counts
    let mut since_last = 0u64;
    for ((tag, val), &run) in compressed.iter().zip(raw_rle_runs.iter()) {
        if *tag == 3 {
            let gap = since_last;
            zero_rle_gaps.push(gap);
            zero_rle_runs.push(run);
            since_last = 0;
        } else {
            hf_stream.push(((*val as usize) << 3) | (*tag as usize));
            since_last += 1;
        }
    }
    let hf_nsyms = hf_stream.len() as u64;
    let num_zero_rle = zero_rle_gaps.len() as u64;

    let (hf_body, _) = huffman_encode_symbols(&hf_stream);

    // Check if any hf_stream symbol exceeds u8 range
    let max_sym = hf_stream.iter().max().cloned().unwrap_or(0);
    let use_huffman_safe = !hf_body.is_empty()
        && hf_body.len() < bitpacked_body.len()
        && hf_body.len() < rc_body.len()
        && data_len >= 256
        && max_sym <= 255;

    // Use the best of three methods
    let use_range_coded = !rc_body.is_empty()
        && rc_body.len() < bitpacked_body.len()
        && rc_body.len() <= hf_body.len()
        && data_len >= 256;
    let use_huffman = use_huffman_safe;

    if use_huffman {
        // Set Huffman flag (bit 6 = 64) only — NOT range_coded
        header[5] |= 64u8;
        let mut body_with_count = Vec::new();
        body_with_count.extend_from_slice(&uleb_vec(hf_nsyms));
        body_with_count.extend_from_slice(&uleb_vec(num_zero_rle));
        for i in 0..num_zero_rle as usize {
            body_with_count.extend_from_slice(&uleb_vec(zero_rle_gaps[i]));
            body_with_count.extend_from_slice(&uleb_vec(zero_rle_runs[i]));
        }
        body_with_count.extend_from_slice(&hf_body);
        header.extend_from_slice(&body_with_count);
    } else if use_range_coded {
        // Set range_coded flag (bit 7 = 128)
        header[5] |= 128u8;
        let mut body_with_count = Vec::new();
        body_with_count.extend_from_slice(&uleb_vec(num_csymbols));
        body_with_count.extend_from_slice(&rc_body);
        header.extend_from_slice(&body_with_count);
    } else {
        header.extend_from_slice(&bitpacked_body);
    }


    header
}

/// Range-code a symbol stream (tags + RAW values + SYM k-values).
/// Matches Python _rc_compress (ssp4_local_v44.py lines 1049-1171).
/// Returns: (rc_body, num_csymbols) or (body, num_csymbols) if not beneficial.
fn range_code_symbols(
    compressed: &[(u8, u64)],
    raw_rle_runs: &[u64],
    block_bits: usize,
    use_odd_sep: bool,
) -> (Vec<u8>, u64) {
    // Expand RAW-RLE (tag=5) into individual RAW (tag=1)
    let mut clean: Vec<(u8, u64)> = Vec::with_capacity(compressed.len() * 4);
    for ((tag, val), &run) in compressed.iter().zip(raw_rle_runs.iter()) {
        match *tag {
            5 => {
                // RAW-RLE: val=raw_val, run=count
                for _ in 0..run as usize {
                    clean.push((1, *val));
                }
            }
            _ => {
                clean.push((*tag, *val));
            }
        }
    }

    let num_csymbols = clean.len() as u64;
    if num_csymbols == 0 {
        return (Vec::new(), 0);
    }

    // Count tag frequencies
    let mut tag_counts = [0u64; 8];
    for &(tag, _) in &clean {
        if (tag as usize) < 8 {
            tag_counts[tag as usize] += 1;
        }
    }

    // Build active tags list and scaled frequencies
    const SCALE: u64 = 4096;
    let active_tags: Vec<u8> = (0u8..8).filter(|&t| tag_counts[t as usize] > 0).collect();
    let n_tags = active_tags.len();
    if n_tags == 0 {
        return (Vec::new(), 0);
    }

    // Normalize frequencies to SCALE
    let total_raw: u64 = tag_counts.iter().sum();
    let mut scaled: Vec<u64> = active_tags
        .iter()
        .map(|&t| {
            let f = tag_counts[t as usize];
            (f * SCALE / total_raw).max(1)
        })
        .collect();
    // Adjust to sum = SCALE (add to largest element, like Python)
    let diff = SCALE as i64 - scaled.iter().sum::<u64>() as i64;
    if diff != 0 {
        let mx_idx = scaled.iter().enumerate().max_by_key(|&(_, &v)| v).map(|(i, _)| i).unwrap_or(0);
        scaled[mx_idx] = (scaled[mx_idx] as i64 + diff).max(1) as u64;
    }

    // Build cumulative freq table
    let mut cum = vec![0u64; n_tags + 1];
    for i in 0..n_tags {
        cum[i + 1] = cum[i] + scaled[i];
    }

    // Count RAW value frequencies
    let mut raw_val_counts: HashMap<u64, u64> = HashMap::new();
    for &(tag, val) in &clean {
        if tag == 1 {
            *raw_val_counts.entry(val).or_insert(0) += 1;
        }
    }
    let n_raw_vals = raw_val_counts.len();
    let raw_scale: u64 = (4096u64).max(((n_raw_vals as u64) * 16).max(4096));
    let mut raw_vals_sorted: Vec<u64> = raw_val_counts.keys().cloned().collect();
    raw_vals_sorted.sort_by_key(|&v| std::cmp::Reverse(*raw_val_counts.get(&v).unwrap_or(&0)));
    let raw_val_to_idx: HashMap<u64, usize> = raw_vals_sorted
        .iter()
        .enumerate()
        .map(|(i, &v)| (v, i))
        .collect();

    let (raw_cum, raw_scaled) = if n_raw_vals > 0 {
        let total_rv: u64 = raw_vals_sorted.iter().map(|&v| *raw_val_counts.get(&v).unwrap()).sum();
        let mut rs: Vec<u64> = raw_vals_sorted
            .iter()
            .map(|&v| {
                let f = *raw_val_counts.get(&v).unwrap();
                (f * raw_scale / total_rv).max(1)
            })
            .collect();
        let rd = raw_scale as i64 - rs.iter().sum::<u64>() as i64;
        if rd != 0 {
            rs[0] = (rs[0] as i64 + rd).max(1) as u64;
        }
        let mut rc = vec![0u64; n_raw_vals + 1];
        for i in 0..n_raw_vals {
            rc[i + 1] = rc[i] + rs[i];
        }
        (rc, rs)
    } else {
        (vec![0u64], vec![])
    };

    // Phase 2: encode
    let mut rc_tags = RangeEncoder::new();
    let mut rc_raw = RangeEncoder::new();
    let mut payload_bw = BitWriter::new();

    for &(tag, value) in &clean {
        let idx = active_tags.iter().position(|&t| t == tag).unwrap_or(0);
        rc_tags.encode(cum[idx], cum[idx + 1] - cum[idx], SCALE);

        match tag {
            0 => {
                // SYM
                if use_odd_sep {
                    let odd = ((value >> 32) & 1) as u8;
                    let k = value & 0xFFFF_FFFF;
                    payload_bw.write_bit(odd);
                    payload_bw.write_uleb(k);
                } else {
                    payload_bw.write_uleb(value);
                }
            }
            1 => {
                // RAW — AC encode value
                if n_raw_vals > 0 {
                    if let Some(&ri) = raw_val_to_idx.get(&value) {
                        rc_raw.encode(raw_cum[ri], raw_cum[ri + 1] - raw_cum[ri], raw_scale);
                    }
                } else {
                    payload_bw.write_bits(value, block_bits);
                }
            }
            3 => {
                // ZERO-RLE: write run count
                payload_bw.write_uleb(value);
            }
            _ => {}
        }
    }

    let rc_tags_data = rc_tags.flush();
    let rc_raw_data = if n_raw_vals > 0 {
        rc_raw.flush()
    } else {
        Vec::new()
    };
    let payload_data = payload_bw.flush();

    // Pack: header + rc_tags + rc_raw + payload
    let mut header = Vec::new();
    header.push(n_tags as u8);
    for &t in &active_tags {
        header.push(t);
    }
    for &s in &scaled {
        header.extend_from_slice(&(s as u16).to_le_bytes());
    }
    header.extend_from_slice(&(n_raw_vals as u16).to_le_bytes());
    for &v in &raw_vals_sorted {
        header.extend_from_slice(&(v as u16).to_le_bytes());
    }
    for &s in &raw_scaled {
        header.extend_from_slice(&(s as u16).to_le_bytes());
    }
    header.extend_from_slice(&(rc_tags_data.len() as u32).to_le_bytes());
    header.extend_from_slice(&(rc_raw_data.len() as u32).to_le_bytes());
    header.extend_from_slice(&rc_tags_data);
    header.extend_from_slice(&rc_raw_data);
    header.extend_from_slice(&payload_data);

    (header, num_csymbols)
}

// ============================================================================
// HUFFMAN CODER — canonical Huffman for SSP5 symbol streams
// ============================================================================

use std::collections::BinaryHeap;
use std::cmp::Ordering;

/// Huffman tree node
#[derive(Clone)]
struct HuffmanNode {
    freq: u64,
    symbol: Option<usize>,
    left: Option<Box<HuffmanNode>>,
    right: Option<Box<HuffmanNode>>,
}

impl PartialEq for HuffmanNode {
    fn eq(&self, other: &Self) -> bool {
        self.freq == other.freq
    }
}
impl Eq for HuffmanNode {}
impl PartialOrd for HuffmanNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HuffmanNode {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse: highest freq = lowest priority (pop smallest first)
        other.freq.cmp(&self.freq)
    }
}

/// Build Huffman codes from symbol frequencies. Returns (symbol → (code, code_len)).
fn build_huffman_codes(freqs: &[(usize, u64)]) -> Vec<(u32, u8)> {
    if freqs.is_empty() {
        return Vec::new();
    }
    if freqs.len() == 1 {
        // Single symbol: code = 0, length = 1
        let mut codes = vec![(0u32, 1u8); freqs.iter().map(|(s, _)| s + 1).max().unwrap_or(1).max(256)];
        if let Some(&(sym, _)) = freqs.first() {
            codes[sym] = (0, 1);
        }
        return codes;
    }

    // Build priority queue
    let mut heap: BinaryHeap<Box<HuffmanNode>> = BinaryHeap::new();
    for &(sym, freq) in freqs {
        heap.push(Box::new(HuffmanNode {
            freq,
            symbol: Some(sym),
            left: None,
            right: None,
        }));
    }

    // Build tree
    while heap.len() > 1 {
        let left = heap.pop().unwrap();
        let right = heap.pop().unwrap();
        let parent = Box::new(HuffmanNode {
            freq: left.freq + right.freq,
            symbol: None,
            left: Some(left),
            right: Some(right),
        });
        heap.push(parent);
    }

    // Traverse tree to get code lengths
    let mut code_lens: Vec<(usize, u8)> = Vec::new();
    let max_sym = freqs.iter().map(|&(s, _)| s).max().unwrap_or(0);
    code_lens.resize(max_sym + 1, (0, 0));

    fn traverse(node: &HuffmanNode, depth: u8, code_lens: &mut Vec<(usize, u8)>) {
        if let Some(sym) = node.symbol {
            if sym < code_lens.len() {
                code_lens[sym].1 = depth;
            }
        }
        if let Some(ref left) = node.left {
            traverse(left, depth + 1, code_lens);
        }
        if let Some(ref right) = node.right {
            traverse(right, depth + 1, code_lens);
        }
    }
    if let Some(root) = heap.pop() {
        traverse(&root, 0, &mut code_lens);
    }

    // Canonical Huffman: sort by length, then by symbol value
    let mut sorted: Vec<(usize, u8)> = code_lens.iter().enumerate()
        .filter(|&(_, &(_, len))| len > 0)
        .map(|(s, &(_, len))| (s, len))
        .collect();
    sorted.sort_by_key(|&(s, len)| (len, s));

    // Assign canonical codes
    let mut codes = vec![(0u32, 0u8); code_lens.len()];
    let mut code = 0u32;
    let mut prev_len = 0u8;
    for &(sym, len) in &sorted {
        if len > prev_len {
            code <<= (len - prev_len) as u32;
        }
        codes[sym] = (code, len);
        code += 1;
        prev_len = len;
    }

    codes
}

/// Encode data with canonical Huffman coding.
/// Returns (encoded_bits, num_symbols) or (empty, 0) if not beneficial.
/// Header format: [n_syms(u16)][sym0(u8)][len0(u8)][sym1(u8)][len1(u8)]...[bitstream...]
fn huffman_encode_symbols(data: &[usize]) -> (Vec<u8>, u64) {
    if data.is_empty() || data.len() < 64 {
        return (Vec::new(), 0);
    }

    // Count frequencies
    let max_sym = data.iter().max().cloned().unwrap_or(0);
    let mut freqs: Vec<u64> = vec![0; (max_sym + 1).max(256)];
    for &sym in data {
        if sym < freqs.len() {
            freqs[sym] += 1;
        }
    }

    // Filter to only symbols that appear
    let active: Vec<(usize, u64)> = freqs.iter().enumerate()
        .filter(|&(_, &f)| f > 0)
        .map(|(s, &f)| (s, f))
        .collect();

    if active.is_empty() || active.len() == 1 {
        return (Vec::new(), 0);
    }

    // Build Huffman codes
    let codes = build_huffman_codes(&active);

    // Encode
    let mut bits: Vec<u8> = Vec::new();
    for &sym in data {
        if sym < codes.len() {
            let (code, len) = codes[sym];
            for i in (0..len as u32).rev() {
                bits.push(((code >> i) & 1) as u8);
            }
        }
    }

    // Build header: [n_syms][sym0][len0][sym1][len1]...
    let mut header = Vec::new();
    let n_syms = active.len() as u16;
    header.extend_from_slice(&n_syms.to_le_bytes());
    for &(sym, _) in &active {
        header.push(sym as u8);
        if sym < codes.len() {
            header.push(codes[sym].1);
        }
    }

    // Pack bits into bytes (MSB first)
    let mut out = Vec::new();
    for chunk in bits.chunks(8) {
        let mut byte = 0u8;
        for (_i, &bit) in chunk.iter().enumerate() {
            byte = (byte << 1) | bit;
        }
        byte <<= 8 - chunk.len();
        out.push(byte);
    }

    let mut result = header;
    result.extend_from_slice(&out);
    (result, data.len() as u64)
}

/// Decode Huffman-encoded data.
fn huffman_decode_symbols(data: &[u8], num_symbols: usize) -> Vec<usize> {
    if data.len() < 3 || num_symbols == 0 {
        return Vec::new();
    }

    let mut off = 0;
    let n_syms = u16::from_le_bytes([data[off], data[off + 1]]) as usize;
    off += 2;

    // Read (symbol, code_length) pairs
    let mut codebook: Vec<(usize, u8)> = Vec::new();
    let mut max_sym = 0;
    for _ in 0..n_syms {
        let sym = data[off] as usize;
        let len = data[off + 1];
        max_sym = max_sym.max(sym);
        codebook.push((sym, len));
        off += 2;
    }

    // Build reverse lookup: (code, len) → symbol
    // Canonical: sort by len, then sym, assign codes
    codebook.sort_by_key(|&(sym, len)| (len, sym));
    let mut code = 0u32;
    let mut prev_len = 0u8;
    let mut lookup: Vec<(u32, usize)> = vec![(0, 0); max_sym + 1]; // index = symbol, value = (code, len)
    let mut code_table: Vec<Option<(u32, u8)>> = vec![None; max_sym + 1];

    for &(sym, len) in &codebook {
        if len > prev_len {
            code <<= (len - prev_len) as u32;
        }
        code_table[sym] = Some((code, len));
        lookup[sym] = (code, len as usize);
        code += 1;
        prev_len = len;
    }

    // Build fast decode table: for codes up to 12 bits, store the symbol
    let max_bits = 12;
    let table_size = 1usize << max_bits;
    let mut decode_table: Vec<Option<usize>> = vec![None; table_size];
    for &(sym, len) in &codebook {
        if len as usize <= max_bits {
            if let Some((c, _)) = code_table[sym] {
                let c = c as usize;
                let start = c;
                let end = start + (1 << (max_bits - len as usize));
                // Only fill table entries that fit in the table
                let table_end = end.min(table_size);
                let table_start = start.min(table_size);
                for v in table_start..table_end {
                    decode_table[v] = Some(sym);
                }
            }
        }
    }

    // Decode bits
    let bits_data = &data[off..];
    let total_bits = bits_data.len() * 8;
    let mut result = Vec::with_capacity(num_symbols);
    let mut pos = 0usize;
    let mut cur = 0u32;
    let mut cur_bits = 0usize;

    while result.len() < num_symbols && pos < total_bits {
        let byte_idx = pos / 8;
        let bit_idx = 7 - (pos % 8);
        if byte_idx < bits_data.len() {
            let bit = (bits_data[byte_idx] >> bit_idx) & 1;
            cur = (cur << 1) | bit as u32;
            cur_bits += 1;

            // Fast path: table lookup for short codes
            if cur_bits <= max_bits && (cur as usize) < table_size {
                if let Some(sym) = decode_table[cur as usize] {
                    result.push(sym);
                    cur = 0;
                    cur_bits = 0;
                }
            }

            // Slow path: linear search for long codes
            if result.len() < num_symbols && cur_bits > max_bits {
                for &(sym, len) in &codebook {
                    if len as usize == cur_bits {
                        if let Some((c, _)) = code_table[sym] {
                            if c == cur {
                                result.push(sym);
                                cur = 0;
                                cur_bits = 0;
                                break;
                            }
                        }
                    }
                }
            }
        }
        pos += 1;
    }

    result
}

/// Decode SSP5 archive back to original bytes.
pub fn decode(archive: &[u8], s: &[u64]) -> Result<Vec<u8>, &'static str> {
    if archive.len() < 7 {
        return Err("Archive too short");
    }
    if &archive[0..4] != SSP5_MAGIC {
        return Err("Bad magic (expected SSP5)");
    }

    let mut off = 4;
    let version = archive[off];
    off += 1;
    if version != SSP5_VERSION {
        return Err("Unsupported SSP5 version");
    }

    let flags = archive[off];
    off += 1;
    let range_coded = (flags & 128) != 0;
    let huffman_coded = (flags & 64) != 0;
    let delta = (flags & 1) != 0;
    let _rle_enabled = (flags & 2) != 0;
    let mfs_enabled = (flags & 4) != 0;

    let use_odd_sep = (flags & 8) != 0;

    let block_bits = archive[off] as usize;
    off += 1;
    if block_bits < 8 || block_bits > 64 || block_bits % 8 != 0 {
        return Err("Invalid block_bits");
    }
    let block_bytes = block_bits / 8;
    let max_block_val = (1u64 << block_bits) - 1;

    let (_m, n) = uleb_decode_all(&archive[off..])?;
    off += n;
    let (_r, n2) = uleb_decode_all(&archive[off..])?;
    off += n2;
    let (data_len, n3) = uleb_decode_all(&archive[off..])?;
    off += n3;
    let (num_blocks, n4) = uleb_decode_all(&archive[off..])?;
    off += n4;

    // Read MFS value from header AFTER ULEB fields (matches Python order)
    let mfs_val = if mfs_enabled {
        if use_odd_sep {
            let mfs_odd = archive[off] as u64;
            off += 1;
            let (mfs_k, n) = uleb_decode_all(&archive[off..])?;
            off += n;
            let sym_val = if mfs_k > 0 {
                decode_symbol(mfs_k, s).unwrap_or(0)
            } else {
                0
            };
            sym_val + mfs_odd
        } else {
            let (packed_k, n) = uleb_decode_all(&archive[off..])?;
            off += n;
            let k = packed_k >> 1;
            let odd = packed_k & 1;
            let sym_val = if k > 0 {
                decode_symbol(k, s).unwrap_or(0)
            } else {
                0
            };
            sym_val + odd
        }
    } else {
        0
    };

    let mut body = &archive[off..];

    // Get compressed_symbols: either range-decoded, Huffman, or bit-packed
    let compressed_symbols: Vec<(u8, u64, u64)> = if huffman_coded {
        // Huffman-coded path: decode Huffman stream and expand to (tag, val, run)
        let mut off_body = 0;
        let (num_hsyms, n) = uleb_decode_all(body)?;
        off_body += n;
        body = &body[n..]; // advance body for next read
        let (num_zero_rle, n2) = uleb_decode_all(body)?;
        off_body += n2;
        body = &body[n2..]; // advance body for next read

        // Read num_zero_rle pairs of (gap, run)
        let mut zero_rle_gaps: Vec<u64> = Vec::with_capacity(num_zero_rle as usize);
        let mut zero_rle_runs: Vec<u64> = Vec::with_capacity(num_zero_rle as usize);
        for _ in 0..num_zero_rle {
            let (gap, n3) = uleb_decode_all(body)?;
            zero_rle_gaps.push(gap);
            let (run, n4) = uleb_decode_all(body)?;
            zero_rle_runs.push(run);
            off_body += n3 + n4;
        }

        let hf_data = &body[off_body..];

        let decoded = huffman_decode_symbols(hf_data, num_hsyms as usize);

        // Build compressed_symbols: interleave normal entries (decoded stream) with
        // ZERO-RLE entries (from zero_rle_gaps/runs). Gap = number of normal entries
        // to emit before the next ZERO-RLE.
        let mut result: Vec<(u8, u64, u64)> = Vec::new();
        let mut sym_idx = 0;
        let mut zero_rle_idx = 0usize;
        let mut remaining_gap = if num_zero_rle > 0 {
            zero_rle_gaps[0]
        } else {
            0
        };

        while sym_idx < decoded.len() || zero_rle_idx < num_zero_rle as usize {
            if zero_rle_idx < num_zero_rle as usize && remaining_gap == 0 {
                // Emit ZERO-RLE
                result.push((3, 0, zero_rle_runs[zero_rle_idx]));
                zero_rle_idx += 1;
                remaining_gap = if zero_rle_idx < num_zero_rle as usize {
                    zero_rle_gaps[zero_rle_idx]
                } else { 0 };
            } else if sym_idx < decoded.len() {
                // Emit normal entry
                let sym = decoded[sym_idx];
                let tag = (sym & 7) as u8;
                let val = (sym >> 3) as u64;
                result.push((tag, val, 0));
                sym_idx += 1;
                if remaining_gap > 0 {
                    remaining_gap -= 1;
                }
            } else {
                break;
            }
        }
        result
    } else if range_coded {
        // Range-coded path: read num_csymbols then parse RC model and decode
        let mut off_body = 0;

        // Read num_csymbols ULEB
        let (num_csymbols, n) = uleb_decode_all(body)?;
        off_body += n;

        let rc_data = &body[off_body..];
        let mut off = 0;

        // Parse RC model header
        if off + 1 > rc_data.len() {
            return Err("Range-coded data too short");
        }
        let n_tags = rc_data[off] as usize;
        off += 1;
        if n_tags == 0 || n_tags > 8 {
            return Err("Invalid n_tags");
        }

        let active_tags: Vec<u8> = rc_data[off..off + n_tags].to_vec();
        off += n_tags;

        // Read scaled frequencies (u16 LE each)
        if off + 2 * n_tags > rc_data.len() {
            return Err("Range-coded: truncated scaled freqs");
        }
        let scaled: Vec<u64> = (0..n_tags)
            .map(|i| u64::from(u16::from_le_bytes([rc_data[off + i * 2], rc_data[off + i * 2 + 1]])))
            .collect();
        off += 2 * n_tags;

        // Cumulative freq table
        let mut cum = vec![0u64; n_tags + 1];
        for i in 0..n_tags {
            cum[i + 1] = cum[i] + scaled[i];
        }

        // Read RAW value model
        if off + 2 > rc_data.len() {
            return Err("Range-coded: truncated n_raw_vals");
        }
        let n_raw_vals = u64::from(u16::from_le_bytes([rc_data[off], rc_data[off + 1]])) as usize;
        off += 2;

        let mut raw_vals_sorted: Vec<u64> = Vec::new();
        let mut raw_cum: Vec<u64> = vec![0u64];

        if n_raw_vals > 0 {
            if off + 2 * n_raw_vals > rc_data.len() {
                return Err("Range-coded: truncated raw_vals");
            }
            raw_vals_sorted = (0..n_raw_vals)
                .map(|i| u64::from(u16::from_le_bytes([rc_data[off + i * 2], rc_data[off + i * 2 + 1]])))
                .collect();
            off += 2 * n_raw_vals;

            if off + 2 * n_raw_vals > rc_data.len() {
                return Err("Range-coded: truncated raw_scaled");
            }
            let raw_scaled: Vec<u64> = (0..n_raw_vals)
                .map(|i| u64::from(u16::from_le_bytes([rc_data[off + i * 2], rc_data[off + i * 2 + 1]])))
                .collect();
            off += 2 * n_raw_vals;

            raw_cum = vec![0u64; n_raw_vals + 1];
            for i in 0..n_raw_vals {
                raw_cum[i + 1] = raw_cum[i] + raw_scaled[i];
            }
        }

        // Read rc_tags_len and rc_raw_len (at current `off` position after raw_scaled)
        if off + 8 > rc_data.len() {
            return Err("Range-coded: truncated lengths");
        }
        let rc_tags_len = u32::from_le_bytes([rc_data[off], rc_data[off + 1], rc_data[off + 2], rc_data[off + 3]]) as usize;
        let rc_raw_len = u32::from_le_bytes([rc_data[off + 4], rc_data[off + 5], rc_data[off + 6], rc_data[off + 7]]) as usize;
        off += 8;

        // Split data into rc_tags, rc_raw, payload
        if off + rc_tags_len + rc_raw_len > rc_data.len() {
            return Err("Range-coded: truncated RC data");
        }
        let rc_tags_data = &rc_data[off..off + rc_tags_len];
        let rc_raw_data = if rc_raw_len > 0 { &rc_data[off + rc_tags_len..off + rc_tags_len + rc_raw_len] } else { &[] };
        let payload_data = &rc_data[off + rc_tags_len + rc_raw_len..];

        // Range-decode tags
        let mut tags = Vec::with_capacity(num_csymbols as usize);
        {
            let mut rd_tags = RangeDecoder::new(rc_tags_data);
            let total: u64 = cum.last().copied().unwrap_or(1);
            for _ in 0..num_csymbols {
                let f = rd_tags.get_freq(total);
                let mut tag_idx = 0usize;
                for i in 0..n_tags {
                    if f < cum[i + 1] {
                        tag_idx = i;
                        break;
                    }
                }
                tags.push(active_tags[tag_idx]);
                rd_tags.decode(cum[tag_idx], cum[tag_idx + 1] - cum[tag_idx], total);
            }
        }

        // Range-decode RAW values: count how many RAW symbols, then decode them
        let num_raw = tags.iter().filter(|&&t| t == 1).count() as usize;
        let raw_vals_decoded: Vec<u64> = if n_raw_vals > 0 && !rc_raw_data.is_empty() && num_raw > 0 {
            let mut raw_decoded = Vec::with_capacity(num_raw);
            let mut rd_raw = RangeDecoder::new(rc_raw_data);
            let total_raw: u64 = raw_cum.last().copied().unwrap_or(1);
            for _ in 0..num_raw {
                let rf = rd_raw.get_freq(total_raw);
                // Find which raw_val corresponds to this rf
                let mut ri = 0usize;
                for i in 0..n_raw_vals {
                    if rf < raw_cum[i + 1] {
                        ri = i;
                        break;
                    }
                }
                let raw_val = raw_vals_sorted[ri];
                raw_decoded.push(raw_val);
                rd_raw.decode(raw_cum[ri], raw_cum[ri + 1] - raw_cum[ri], total_raw);
            }
            raw_decoded
        } else {
            Vec::new()
        };

        // Use BitReader for payload (SYM k-values and ZERO-RLE counts)
        let mut br = BitReader::new(payload_data);
        let mut raw_idx = 0usize;

        // Reconstruct compressed_symbols from decoded tags
        let mut result: Vec<(u8, u64, u64)> = Vec::with_capacity(num_csymbols as usize);
        for &tag in &tags {
            match tag {
                0 => {
                    // SYM
                    if use_odd_sep {
                        let odd = br.read_bit().unwrap_or(0) as u64;
                        let k = br.read_uleb().unwrap_or(0);
                        result.push((tag, (odd << 32) | k, 0));
                    } else {
                        let pk = br.read_uleb().unwrap_or(0);
                        result.push((tag, pk, 0));
                    }
                }
                1 => {
                    // RAW — use raw_vals_decoded or read from payload
                    if n_raw_vals > 0 && raw_idx < raw_vals_decoded.len() {
                        result.push((tag, raw_vals_decoded[raw_idx], 0));
                        raw_idx += 1;
                    } else {
                        let val = br.read_bits(block_bits).unwrap_or(0);
                        result.push((tag, val, 0));
                    }
                }
                2 => {
                    // ZERO
                    result.push((tag, 0, 0));
                }
                3 => {
                    // ZERO-RLE
                    let count = br.read_uleb().unwrap_or(0);
                    result.push((tag, 0, count));
                }
                _ => {
                    result.push((tag, 0, 0));
                }
            }
        }
        result
    } else {
        // Bit-packed path: parse directly with BitReader
        // Tags 0/1/2 are 2-bit; tags 3/4/5 are 4-bit (prefix 11 + 2-bit subtype).
        let mut br = BitReader::new(body);
        let mut result: Vec<(u8, u64, u64)> = Vec::new();

        while !br.is_exhausted() {
            let tag_bits = br.read_bits(2).unwrap_or(0);
            let tag = match tag_bits {
                0 => 0u8,  // SYM (00)
                1 => 1u8,  // RAW (01)
                2 => 2u8,  // ZERO (10)
                3 => {
                    // 3-bit variant tags: read 2 more bits
                    let subtype = br.read_bits(2).unwrap_or(0);
                    match subtype {
                        0b00 => 3u8,  // ZERO-RLE (1100)
                        0b01 => 4u8,  // MFS (1101)
                        0b10 => 5u8,  // RAW-RLE (1110)
                        _ => break,
                    }
                }
                _ => break,
            };

            if tag == 0 || tag == 3 {
                br.skip_to_byte_boundary();
            }

            match tag {
                0 => {
                    if use_odd_sep {
                        let odd = br.read_bit().unwrap_or(0) as u64;
                        let k = br.read_uleb().unwrap_or(0);
                        result.push((tag, (odd << 32) | k, 0));
                    } else {
                        let pk = br.read_uleb().unwrap_or(0);
                        result.push((tag, pk, 0));
                    }
                }
                1 => {
                    let val = br.read_bits(block_bits).unwrap_or(0);
                    result.push((tag, val, 0));
                }
                2 => {
                    result.push((tag, 0, 0));
                }
                3 => {
                    let count = br.read_uleb().unwrap_or(0);
                    result.push((tag, 0, count));
                }
                4 => {
                    result.push((tag, 0, 0));
                }
                5 => {
                    let raw_val = br.read_bits(block_bits).unwrap_or(0);
                    let run = br.read_uleb().unwrap_or(1);
                    for _ in 0..run {
                        result.push((1, raw_val, 0));
                    }
                }
                _ => break,
            }
        }
        result
    };

    // Process compressed_symbols and emit blocks
    // Each encode path writes a different number of entries:
    // - Bit-packed: num_blocks entries (RAW-RLE encoded inline per-block)
    // - Range-coded: clean.len() entries (RAW-RLE expanded to per-block entries)
    // - Huffman: compressed.len() entries (RLE collapsed, but num_blocks symbols in stream)
    // Use the correct limit based on which path was used.
    let sym_limit = compressed_symbols.len(); // All paths: iterate all entries.
    // ZERO-RLE (tag=3) expands internally in the loop.
    // sym_idx may not reach num_blocks — that's fine, all data was written.

    let mut output = Vec::with_capacity(num_blocks as usize * block_bytes);
    let mut prev_n: u64 = 0;
    let mut _sym_idx = 0usize;

    for &(tag, val, extra) in compressed_symbols.iter().take(sym_limit) {

        if tag == 4 {
            // MFS: value is mfs_val
            let n = if delta {
                prev_n.wrapping_add(mfs_val) & max_block_val
            } else {
                mfs_val
            };
            prev_n = n;
            let needed = (block_bits + 7) / 8;
            let be_bytes = n.to_be_bytes();
            output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
            _sym_idx += 1;
            continue;
        }

        if tag == 0 || tag == 3 {
            // skip_to_byte_boundary
        }

        match tag {
            0 => {
                // SYM
                if use_odd_sep {
                    let odd = (val >> 32) & 1;
                    let k = val & 0xFFFF_FFFF;
                    let sym_val = if k > 0 {
                        decode_symbol(k, s).unwrap_or(0)
                    } else { 0 };
                    let val2 = sym_val + odd;
                    let n = if delta {
                        prev_n.wrapping_add(val2) & max_block_val
                    } else { val2 };
                    prev_n = n;
                    let needed = (block_bits + 7) / 8;
                    let be_bytes = n.to_be_bytes();
                    output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                } else {
                    let k = val >> 1;
                    let odd = val & 1;
                    let sym_val = if k > 0 {
                        decode_symbol(k, s).unwrap_or(0)
                    } else { 0 };
                    let val2 = sym_val + odd;
                    let n = if delta {
                        prev_n.wrapping_add(val2) & max_block_val
                    } else { val2 };
                    prev_n = n;
                    let needed = (block_bits + 7) / 8;
                    let be_bytes = n.to_be_bytes();
                    output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                }
                _sym_idx += 1;
            }
            1 => {
                // RAW
                let n = if delta {
                    prev_n.wrapping_add(val) & max_block_val
                } else { val };
                prev_n = n;
                let needed = (block_bits + 7) / 8;
                let be_bytes = n.to_be_bytes();
                output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                _sym_idx += 1;
            }
            2 => {
                // ZERO
                let n = if delta { prev_n } else { 0 };
                prev_n = n;
                let needed = (block_bits + 7) / 8;
                let be_bytes = n.to_be_bytes();
                output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                _sym_idx += 1;
            }
            3 => {
                // ZERO-RLE: extra = number of zero blocks
                _sym_idx += extra as usize;
                for _ in 0..extra {
                    let n = if delta { prev_n } else { 0 };
                    prev_n = n;
                    let needed = (block_bits + 7) / 8;
                    let be_bytes = n.to_be_bytes();
                    output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                }
            }
            _ => {}
        }
    }

    // Truncate to original data length (not padded).
    // Keep first data_len bytes (big-endian: original bytes are the FIRST bytes).
    if output.len() > data_len as usize {
        output.drain(data_len as usize..);
    }

    Ok(output)
}

fn encode_empty(block_bits: usize) -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(SSP5_MAGIC);
    header.push(SSP5_VERSION);
    header.push(0); // flags
    header.push(block_bits as u8);
    header.extend_from_slice(&uleb_vec(1)); // m=1
    header.extend_from_slice(&uleb_vec(1)); // r=1
    header.extend_from_slice(&uleb_vec(0)); // data_len=0
    header.extend_from_slice(&uleb_vec(0)); // num_blocks=0
    header
}

fn uleb_size(mut x: u64) -> u64 {
    let mut len = 1u64;
    while x >= 0x80 {
        x >>= 7;
        len += 1;
    }
    len
}

fn uleb_vec(x: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = x;
    while v >= 0x80 {
        out.push(((v & 0x7F) | 0x80) as u8);
        v >>= 7;
    }
    out.push((v & 0x7F) as u8);
    out
}

fn uleb_decode_all(data: &[u8]) -> Result<(u64, usize), &'static str> {
    let mut result = 0u64;
    let mut shift = 0;
    let mut pos = 0;
    while pos < data.len() {
        let b = data[pos];
        pos += 1;
        result |= ((b & 0x7F) as u64) << shift;
        if (b & 0x80) == 0 {
            return Ok((result, pos));
        }
        shift += 7;
    }
    Err("ULEB: incomplete")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a simple test S sequence: first N primes starting from 2
    fn test_s() -> Vec<u64> {
        let mut primes = Vec::new();
        let mut n = 2u64;
        while primes.len() < 32 {
            if is_prime(n) {
                primes.push(n);
            }
            n += 1;
        }
        primes
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

    #[test]
    fn test_encode_decode_roundtrip() {
        let s = test_s();
        let data = b"Hello, SSP4 world! This is a test of the SSP codec.";

        for bb in [8, 16, 32] {
            let enc = encode(data, &s, bb, false);
            let dec = decode(&enc, &s).expect("decode should succeed");
            assert_eq!(&dec[..], data, "decode should match original (bb={})", bb);
        }
    }

    #[test]
    fn test_encode_decode_simple() {
        let s = test_s();
        let data = [12u8];
        let enc = encode(&data, &s, 16, false);
        let dec = decode(&enc, &s).expect("decode should succeed");
        assert_eq!(dec.len(), 1, "decoded length should be 1");
        assert_eq!(dec[0], 12, "decoded byte should be 12");
    }

    #[test]
    fn test_encode_decode_with_delta() {
        let s = test_s();
        // Data with ascending values: good for delta
        let data: Vec<u8> = (0u8..64).map(|x| x.wrapping_mul(3)).collect();

        for bb in [8, 16] {
            let enc = encode(&data, &s, bb, true);
            let dec = decode(&enc, &s).expect("decode should succeed");
            assert_eq!(&dec[..], &data[..], "delta decode should match (bb={})", bb);
        }
    }

    #[test]
    fn test_raw_blocks_minimal() {
        // Minimal test: 2 RAW blocks with bb=8
        // Block 0: 'H'=72. Block 1: 'e'=101.
        let s = test_s();
        let data = b"He"; // 2 bytes, 2 blocks
        
        let enc = encode(data, &s, 8, false);
        
        // Use the decode function to extract body (which has correct header parsing)
        let dec = decode(&enc, &s).expect("decode should succeed");
        
        assert_eq!(&dec[..], data, "minimal decode should match");
    }

    #[test]
    fn test_encode_decode_empty() {
        let s = test_s();
        let data = b"";

        for bb in [8, 16, 32] {
            let enc = encode(data, &s, bb, false);
            let dec = decode(&enc, &s).expect("decode empty should succeed");
            assert_eq!(&dec[..], data);
        }
    }

    #[test]
    fn test_encode_decode_random() {
        let s = test_s();
        let data: Vec<u8> = (0..1000).map(|_| rand_u8()).collect();

        for bb in [8, 16] {
            let enc = encode(&data, &s, bb, false);
            let dec = decode(&enc, &s).expect("decode should succeed");
            assert_eq!(&dec[..], &data[..], "random data decode (bb={})", bb);
        }
    }

    #[test]
    fn test_range_coder_roundtrip() {
        let s = test_s();
        // Test with data that triggers range coder (>256 bytes)
        // Use text-like data with skewed distribution
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".repeat(8); // 312 bytes
        assert!(data.len() >= 256);

        let enc = encode(&data, &s, 8, false);
        let dec = decode(&enc, &s).expect("decode should succeed");
        assert_eq!(&dec[..], &data[..], "range coder roundtrip");
    }

    fn rand_u8() -> u8 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        ((nanos as u64 * 1103515245 + 12345) >> 16) as u8
    }
}
