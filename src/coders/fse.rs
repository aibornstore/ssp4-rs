//! FSE-style entropy coder: Huffman for BWT+MTF data
//!
//! Format: [num_syms(1)][codebook(num_syms*6)][bitstream_bytes(4)][num_symbols(4)][bitstream]
//! Codebook: [sym(1)][code(4)][depth(1)] per entry (tree codes, prefix-free by construction)
//!
//! Encode: Huffman tree → canonical codes → pack bits MSB-first into bytes
//!   Code bits go to TOP of 64-bit buffer: shift = 64 - nbits - len
//!   Flush MSB-first: (bitbuf >> 56) as u8
//!
//! Decode: read bits MSB-first → accumulate at MSB side
//!   buf = (buf << 1) | bit, match top bits: buf & (!0u64 << (64 - cl))
//!   Consume from MSB: buf &= !0u64 >> cl

use std::cmp::Ordering;

// ──────────────────────────────────────────────────────────────────────────────
// Huffman tree construction (min-heap + DFS)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct HuffNode {
    freq: u64,
    sym: Option<u8>,
    idx: usize, // stable tiebreaker for heap ordering
    left: Option<Box<HuffNode>>,
    right: Option<Box<HuffNode>>,
}

impl HuffNode {
    fn leaf(sym: u8, freq: u64, idx: usize) -> Self {
        Self { freq, sym: Some(sym), idx, left: None, right: None }
    }
    fn internal(left: Box<HuffNode>, right: Box<HuffNode>, idx: usize) -> Self {
        Self { freq: left.freq + right.freq, sym: None, idx, left: Some(left), right: Some(right) }
    }
}

impl Ord for HuffNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other.freq.cmp(&self.freq)
            .then_with(|| other.sym.cmp(&self.sym))
            .then_with(|| other.idx.cmp(&self.idx))
    }
}
impl PartialOrd for HuffNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl Eq for HuffNode {}
impl PartialEq for HuffNode {
    fn eq(&self, other: &Self) -> bool {
        self.freq == other.freq && self.sym == other.sym && self.idx == other.idx
    }
}

fn sift_down(heap: &mut Vec<HuffNode>, mut i: usize) {
    let len = heap.len();
    loop {
        let left = 2 * i + 1;
        let right = 2 * i + 2;
        let mut smallest = i;
        let cur_freq = heap[i].freq;
        let cur_sym = heap[i].sym;
        let cur_idx = heap[i].idx;
        if left < len {
            let l_freq = heap[left].freq;
            let l_sym = heap[left].sym;
            let l_idx = heap[left].idx;
            if l_freq < cur_freq
                || (l_freq == cur_freq && l_sym.cmp(&cur_sym) == Ordering::Less)
                || (l_freq == cur_freq && l_sym == cur_sym && l_idx < cur_idx)
            {
                smallest = left;
            }
        }
        if right < len {
            let s_freq = heap[smallest].freq;
            let s_sym = heap[smallest].sym;
            let s_idx = heap[smallest].idx;
            let r_freq = heap[right].freq;
            let r_sym = heap[right].sym;
            let r_idx = heap[right].idx;
            if r_freq < s_freq
                || (r_freq == s_freq && r_sym.cmp(&s_sym) == Ordering::Less)
                || (r_freq == s_freq && r_sym == s_sym && r_idx < s_idx)
            {
                smallest = right;
            }
        }
        if smallest != i {
            heap.swap(i, smallest);
            i = smallest;
        } else {
            break;
        }
    }
}

fn sift_up(heap: &mut Vec<HuffNode>, mut i: usize) {
    while i > 0 {
        let parent = (i - 1) / 2;
        let cur_freq = heap[i].freq;
        let cur_sym = heap[i].sym;
        let cur_idx = heap[i].idx;
        let p_freq = heap[parent].freq;
        let p_sym = heap[parent].sym;
        let p_idx = heap[parent].idx;
        if cur_freq < p_freq
            || (cur_freq == p_freq && cur_sym.cmp(&p_sym) == Ordering::Less)
            || (cur_freq == p_freq && cur_sym == p_sym && cur_idx < p_idx)
        {
            heap.swap(i, parent);
            i = parent;
        } else {
            break;
        }
    }
}

/// Build Huffman tree from (sym, freq) pairs sorted by (freq DESC, sym ASC).
/// Returns code lengths per symbol.
fn huffman_lengths(sorted_active: &[(u8, u32)]) -> Vec<(u8, u32, u8)> {
    if sorted_active.is_empty() { return Vec::new(); }
    if sorted_active.len() == 1 {
        return vec![(sorted_active[0].0, 0u32, 1u8)];
    }

    let mut heap: Vec<HuffNode> = sorted_active.iter()
        .enumerate()
        .map(|(i, (s, f))| HuffNode::leaf(*s, *f as u64, i))
        .collect();

    let len = heap.len();
    for i in (1..len).rev() {
        sift_down(&mut heap, i);
    }

    let mut next_idx = sorted_active.len();
    while heap.len() > 1 {
        let mut left = heap.remove(0);
        if !heap.is_empty() { sift_down(&mut heap, 0); }
        let mut right = heap.remove(0);
        // For equal-weight ties: put node with larger symbol on LEFT (DFS visits left first)
        // This ensures larger symbols get deeper positions, giving shorter codes to smaller symbols
        if right.sym > left.sym {
            std::mem::swap(&mut left, &mut right);
        }
        heap.push(HuffNode::internal(Box::new(left), Box::new(right), next_idx));
        next_idx += 1;
        let last_idx = heap.len() - 1;
        sift_up(&mut heap, last_idx);
    }

    let mut result = Vec::new();
    fn collect(node: &HuffNode, code: u32, depth: u8, out: &mut Vec<(u8, u32, u8)>) {
        if node.left.is_none() && node.right.is_none() {
            if let Some(sym) = node.sym { out.push((sym, code, depth)); }
        } else {
            if let Some(ref l) = node.left { collect(l, code << 1, depth + 1, out); }
            if let Some(ref r) = node.right { collect(r, (code << 1) | 1, depth + 1, out); }
        }
    }
    collect(&heap.pop().unwrap(), 0, 0, &mut result);
    result
}

// ──────────────────────────────────────────────────────────────────────────────
// Public API
// ──────────────────────────────────────────────────────────────────────────────

/// Encode data using canonical Huffman coding.
/// MSB-first packing: code bits go to TOP of 64-bit buffer, flush via (>>56).
pub fn huffman_encode(data: &[u8]) -> Vec<u8> {
    if data.is_empty() { return Vec::new(); }

    let mut freq = [0u32; 256];
    for &b in data { freq[b as usize] += 1; }

    let mut active: Vec<(u8, u32)> = (0u8..=255u8)
        .filter(|&i| freq[i as usize] > 0)
        .map(|i| (i, freq[i as usize]))
        .collect();
    active.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Single symbol special case: format [1][sym(1)][code(4)][depth(1)][count(4)]
    if active.len() == 1 {
        let sym = active[0].0;
        let count = active[0].1;
        let mut out = Vec::with_capacity(11);
        out.push(1u8); // num_syms=1 (flag for single symbol)
        out.push(sym);
        out.extend_from_slice(&0u32.to_le_bytes()); // code (unused)
        out.push(1u8); // depth=1
        out.extend_from_slice(&(count as u32).to_le_bytes()); // count as u32 (4 bytes!)
        return out;
    }

    // Build Huffman tree → DFS gives prefix-free (code, len) per symbol
    // Tree codes are prefix-free by construction (left=0, right=1 in binary)
    let tree_codes: Vec<(u8, u32, u8)> = huffman_lengths(&active);

    // Build lookup: sym → (code, len) for encoding
    let mut lookup = vec![(0u32, 0u8); 256];
    for &(sym, code, len) in &tree_codes {
        lookup[sym as usize] = (code, len);
    }

    // For format: write codebook in symbol order
    let mut codes_by_sym: Vec<(u8, u32, u8)> = tree_codes.clone();
    codes_by_sym.sort_by_key(|&(sym, _, _)| sym);

    // MSB-first packing: bit 7 of code → first bit in stream, bit 6 → second, etc.
    // Accumulate into a Vec<u8>, flushing each full byte.
    let mut bitstream = Vec::new();
    let mut cur_byte: u8 = 0;
    let mut bit_pos: u8 = 0; // next bit position in cur_byte (0=MSB, 7=LSB)

    for &b in data {
        let (code, len) = lookup[b as usize];
        // Write code bits MSB-first: start from bit (len-1) down to 0
        for i in (0..len).rev() {
            let bit = ((code >> i) & 1) as u8;
            cur_byte |= bit << (7 - bit_pos);
            bit_pos += 1;
            if bit_pos == 8 {
                bitstream.push(cur_byte);
                cur_byte = 0;
                bit_pos = 0;
            }
        }
    }
    if bit_pos > 0 {
        bitstream.push(cur_byte);
    }

    // Output: [num_syms(1)][codebook(N*6)][bitstream_bytes(4)][num_symbols(4)][bitstream]
    // Codebook: [sym(1)][code(4)][depth(1)] — tree codes in symbol order
    let num_syms = codes_by_sym.len() as u8;
    let bitstream_bytes = bitstream.len() as u32;
    let num_symbols = data.len() as u32;

    let mut out = Vec::with_capacity(1 + (num_syms as usize) * 6 + 4 + 4 + bitstream.len());
    out.push(num_syms);
    for &(sym, code, len) in &codes_by_sym {
        out.push(sym);
        out.extend_from_slice(&code.to_le_bytes());
        out.push(len);
    }
    out.extend_from_slice(&bitstream_bytes.to_le_bytes());
    out.extend_from_slice(&num_symbols.to_le_bytes());
    out.extend_from_slice(&bitstream);

    out
}

/// Decode Huffman-encoded data using canonical matching.
/// MSB-first reading: read bit 7 first, accumulate at MSB side.
pub fn huffman_decode(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.is_empty() { return Ok(Vec::new()); }

    let mut pos = 0usize;
    let num_syms = data[pos]; pos += 1;

    if num_syms == 0 { return Err("Huffman: no symbols"); }

    // Single symbol: [1][sym(1)][code(4)][depth(1)][count(4)]
    if num_syms == 1 {
        let sym = data[pos]; pos += 1;
        pos += 4 + 1; // skip code + depth
        let count = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]) as usize;
        return Ok(vec![sym; count]);
    }

    // Read codebook: [(sym, code, len)]
    let mut entries: Vec<(u8, u32, u8)> = Vec::with_capacity(num_syms as usize);
    for _ in 0..num_syms as usize {
        let sym = data[pos]; pos += 1;
        let code = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]); pos += 4;
        let len = data[pos]; pos += 1;
        entries.push((sym, code, len));
    }

    let bitstream_bytes = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]) as usize;
    pos += 4;
    let num_symbols = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]) as usize;
    pos += 4;
    let bitstream = &data[pos..pos + bitstream_bytes];

    if num_symbols == 0 { return Ok(Vec::new()); }

    // Build lookup: (code, len, sym) sorted by len DESC (longest first)
    let mut codes: Vec<(u32, u8, u8)> = entries.iter().map(|&(s, c, l)| (c, l, s)).collect();
    codes.sort_by(|a, b| b.1.cmp(&a.1)); // len DESC
    let max_depth = codes.first().map(|c| c.1 as usize).unwrap_or(0);

    let mut result = Vec::with_capacity(num_symbols);
    let mut buf = 0u64;
    let mut loaded_bits = 0usize;  // bits currently in buf
    let mut stream_pos = 0usize;   // next stream bit to load
    let total_bits = bitstream_bytes * 8; // total bits in bitstream
    let mut remaining_bits = total_bits; // real bits remaining (decrements on load)

    while result.len() < num_symbols {
        // Load more bits from bitstream MSB-first: accumulate at MSB side of buf
        // bit 0 goes to position 63, bit 1 to 62, etc.
        while loaded_bits < max_depth && remaining_bits > 0 {
            let byte_idx = stream_pos / 8;
            if byte_idx >= bitstream.len() { break; }
            let bit_in_byte = 7 - (stream_pos % 8); // MSB-first: bit 7 first
            let byte = bitstream[byte_idx];
            let bit_val = (byte >> bit_in_byte) & 1;
            buf |= (bit_val as u64) << (63 - loaded_bits); // MSB-first accumulation
            loaded_bits += 1;
            stream_pos += 1;
            remaining_bits -= 1;
        }

        if loaded_bits == 0 { break; }

        // Match top bits of buffer against tree codes (sorted by len DESC)
        // MSB-first accumulation: top bits are at positions 63, 62, ..., 64-loaded_bits
        // Extract top cl bits: shift RIGHT to bring MSB side to LSB, mask cl bits
        let mut matched_sym = None;
        let mut matched_len = 0usize;
        for &(code_val, code_len, sym) in &codes {
            let cl = code_len as usize;
            if cl <= loaded_bits {
                let shift = 64 - cl;
                let mask = (1u64 << cl) - 1;
                let top_bits = (buf >> shift) & mask;
                if top_bits == code_val as u64 {
                    matched_sym = Some(sym);
                    matched_len = cl;
                    break;
                }
            }
        }

        if let Some(sym) = matched_sym {
            result.push(sym);
            // Consume: shift buffer LEFT by matched_len (remaining bits move toward MSB)
            buf <<= matched_len;
            loaded_bits -= matched_len;
            // stream_pos already advanced during load
        } else {
            break;
        }
    }

    Ok(result)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_huffman_simple() {
        let data = b"abacabadabacaba";
        let encoded = huffman_encode(data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data, "roundtrip should match");
    }

    #[test]
    fn test_huffman_all_same() {
        let data = vec![42u8; 1000];
        let encoded = huffman_encode(&data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_huffman_known_vector() {
        // "aaaaabbbccd" — valid Huffman: a=1, b=2, c=3, d=3
        let data = b"aaaaabbbccd";
        let encoded = huffman_encode(data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_huffman_sizes_debug_012() {
        // Test minimal [0,1,2] through actual encode/decode
        let data = [0u8, 1, 2];
        let encoded = huffman_encode(&data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data, "minimal [0,1,2]");
    }

    #[test]
    fn test_huffman_sizes() {
        let data2 = [0u8, 1];
        let enc2 = huffman_encode(&data2);
        let dec2 = huffman_decode(&enc2).unwrap();
        assert_eq!(dec2, data2, "size 2");

        let data3 = [0u8, 1, 2];
        let enc3 = huffman_encode(&data3);
        let dec3 = huffman_decode(&enc3).unwrap();
        assert_eq!(dec3, data3, "size 3");

        let data4 = [0u8, 1, 2, 3];
        let enc4 = huffman_encode(&data4);
        let dec4 = huffman_decode(&enc4).unwrap();
        assert_eq!(dec4, data4, "size 4");

        let data10: Vec<u8> = (0..10).collect();
        let enc10 = huffman_encode(&data10);
        let dec10 = huffman_decode(&enc10).unwrap();
        assert_eq!(dec10, data10, "size 10");
    }

    #[test]
    fn test_huffman_skewed() {
        let mut data = vec![0u8; 500];
        data.push(1);
        data.push(2);
        let encoded = huffman_encode(&data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data, "skewed data should roundtrip");
    }

    #[test]
    fn test_huffman_repetitive() {
        let data: Vec<u8> = (0..10).flat_map(|i| vec![i; 100]).collect();
        let encoded = huffman_encode(&data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data, "repetitive data should roundtrip");
    }

    #[test]
    fn test_huffman_mtf_like() {
        let mut data = Vec::new();
        for _ in 0..200 {
            data.extend_from_slice(&[0, 0, 0, 1, 2, 3]);
        }
        let encoded = huffman_encode(&data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data, "mtf-like data should roundtrip");
    }

    #[test]
    fn test_huffman_alice29_like() {
        let mut data = Vec::new();
        for _ in 0..100 {
            data.extend_from_slice(&[0, 0, 0, 0, 0, 1, 2, 3, 10, 20, 30]);
        }
        let encoded = huffman_encode(&data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data, "alice29-like data should roundtrip");
    }

    #[test]
    fn test_huffman_random() {
        let data: Vec<u8> = (0u8..100).cycle().take(1000).collect();
        let encoded = huffman_encode(&data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data, "random-like data should roundtrip");
    }

    #[test]
    fn test_huffman_debug_012() {
        // Minimal [0,1,2] — all equal frequency
        let data = [0u8, 1, 2];
        let encoded = huffman_encode(&data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data.as_slice(), "[0,1,2] should roundtrip");
    }

    #[test]
    fn test_huffman_debug_sizes() {
        // [0,1] — basic two-symbol case
        let data = [0u8, 1];
        let encoded = huffman_encode(&data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data.as_slice(), "size 2");
    }
}
