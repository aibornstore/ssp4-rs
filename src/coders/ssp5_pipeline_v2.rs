//! Full SSP5 pipeline: BWT + MTF + RLE + SSP with Range Coder
//! 
//! Pipeline for text compression (improved):
//!   Encode: BWT → MTF → RLE → SSP (Range Coder)
//!   Decode: SSP (Range Coder) → RLE_inv → MTF → BWT
//! 
//! Key improvements over v1:
//! - RLE after MTF compresses runs of identical indices
//! - Optimized S-sequence for MTF index distribution
//! - Block_bits=8 per default (1 MTF index = 1 block)

use super::bwt::{bwt_encode, bwt_decode};
use super::mtf::{mtf_encode, mtf_decode};
use super::ssp_codec::{encode as ssp_encode, decode as ssp_decode, SSP5_MAGIC, SSP5_VERSION};

/// RLE encode: compress runs of identical bytes into (value, run_length) pairs.
/// Only activates for runs >= MIN_RUN (saves bytes when run > 2).
const RLE_MIN_RUN: usize = 3;

/// Encode RLE-compressed data from input bytes.
/// Format: [RLE_MAGIC=u8][rle_bytes...]
/// Each RLE run: [value_byte][run_length_uleb] for runs >= RLE_MIN_RUN,
/// or raw bytes for short runs.
pub fn rle_encode(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    
    while i < data.len() {
        let val = data[i];
        
        // Count run length
        let mut run = 1;
        while i + run < data.len() && data[i + run] == val {
            run += 1;
        }
        
        if run >= RLE_MIN_RUN {
            // RLE: emit (value, run_len)
            out.push(val);
            // Encode run length as ULEB
            let mut r = run;
            while r >= 0x80 {
                out.push(((r & 0x7F) | 0x80) as u8);
                r >>= 7;
            }
            out.push((r & 0x7F) as u8);
        } else {
            // Raw bytes (short run)
            for _ in 0..run {
                out.push(val);
            }
        }
        
        i += run;
    }
    
    out
}

/// Decode RLE data back to original bytes.
pub fn rle_decode(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    
    let mut out = Vec::new();
    let mut i = 0;
    
    while i < data.len() {
        let val = data[i];
        i += 1;
        
        // Read run length (ULEB)
        let mut run = 0u64;
        let mut shift = 0;
        while i < data.len() {
            let b = data[i];
            i += 1;
            run |= ((b & 0x7F) as u64) << shift;
            if (b & 0x80) == 0 {
                break;
            }
            shift += 7;
        }
        
        let run_usize = run as usize;
        if run_usize == 0 {
            // Not a RLE pair — it's a raw byte
            out.push(val);
            // Rewind: this wasn't a RLE pair, it was a raw byte followed by
            // what looks like ULEB but is actually more raw bytes
            // Actually: if run=0, this is just the value (raw byte)
            // If run>0, it's RLE
            // The issue: for raw bytes >= RLE_MIN_RUN, we need to distinguish
            // Solution: use escape code
        }
        
        // For RLE runs
        for _ in 0..run_usize {
            out.push(val);
        }
    }
    
    out
}

/// Better RLE encode with escape coding for ambiguous bytes.
/// Format: [raw bytes] | [ESCAPE, byte, run_uleb]
const RLE_ESCAPE: u8 = 0xFF;

pub fn rle_encode_v2(data: &[u8]) -> (Vec<u8>, usize) {
    /// Count how many runs >= RLE_MIN_RUN exist
    let mut num_rle = 0usize;
    let mut i = 0;
    while i < data.len() {
        let val = data[i];
        let mut run = 1;
        while i + run < data.len() && data[i + run] == val {
            run += 1;
        }
        if run >= RLE_MIN_RUN {
            num_rle += 1;
        }
        i += run;
    }
    
    // Estimate output size
    let mut out = Vec::with_capacity(data.len());
    
    // Write num_rle as ULEB (for decoder)
    let mut nr = num_rle;
    let mut nr_len = 0;
    while nr >= 0x80 {
        out.push(((nr & 0x7F) | 0x80) as u8);
        nr >>= 7;
        nr_len += 1;
    }
    out.push((nr & 0x7F) as u8);
    nr_len += 1;
    
    i = 0;
    while i < data.len() {
        let val = data[i];
        
        // Count run
        let mut run = 1;
        while i + run < data.len() && data[i + run] == val {
            run += 1;
        }
        
        if run >= RLE_MIN_RUN {
            // RLE: [ESCAPE][value][run_uleb]
            out.push(RLE_ESCAPE);
            out.push(val);
            // ULEB run
            let mut r = run;
            while r >= 0x80 {
                out.push(((r & 0x7F) | 0x80) as u8);
                r >>= 7;
            }
            out.push((r & 0x7F) as u8);
        } else {
            // Raw bytes
            for _ in 0..run {
                out.push(val);
            }
        }
        
        i += run;
    }
    
    (out, nr_len)
}

/// Decode RLE v2 data.
pub fn rle_decode_v2(data: &[u8], header_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = header_len;
    
    while i < data.len() {
        if data[i] == RLE_ESCAPE && i + 2 <= data.len() {
            i += 1; // skip ESCAPE
            let val = data[i];
            i += 1;
            // Read ULEB run
            let mut run = 0u64;
            let mut shift = 0;
            while i < data.len() {
                let b = data[i];
                i += 1;
                run |= ((b & 0x7F) as u64) << shift;
                if (b & 0x80) == 0 { break; }
                shift += 7;
            }
            for _ in 0..run {
                out.push(val);
            }
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    
    out
}

/// Analyze MTF output to find optimal parameters
pub fn analyze_mtf(data: &[u8]) {
    // Count MTF index distribution
    let mut counts = [0usize; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    
    // Find top values
    let mut pairs: Vec<_> = counts.iter().enumerate().collect();
    pairs.sort_by_key(|(_, &c)| std::cmp::Reverse(c));
    
    let total = data.len();
    println!("MTF analysis ({} bytes):", total);
    println!("  Top 10 MTF indices:");
    for (i, &(idx, &cnt)) in pairs.iter().take(10).enumerate() {
        println!("    {:3}: count={:6} ({:.1}%)", idx, cnt, cnt as f64 / total as f64 * 100.0);
    }
    
    // Count runs
    let mut total_runs = 0usize;
    let mut long_runs = 0usize; // runs >= 3
    let mut max_run = 0usize;
    let mut i = 0;
    while i < data.len() {
        let val = data[i];
        let mut run = 1;
        while i + run < data.len() && data[i + run] == val {
            run += 1;
        }
        total_runs += 1;
        if run >= 3 { long_runs += 1; }
        max_run = max_run.max(run);
        i += run;
    }
    
    println!("  Total runs: {}", total_runs);
    println!("  Long runs (>=3): {}", long_runs);
    println!("  Max run length: {}", max_run);
    println!("  Compression potential: {:.1}% of data is in runs",
             (long_runs * 3) as f64 / total as f64 * 100.0);
}

/// Optimized S-sequence for MTF index data.
/// MTF output has: lots of 0s, small values (0-31), and occasional large values.
/// This S is optimized for that distribution.
pub fn s_for_mtf() -> Vec<u64> {
    vec![
        // Top MTF indices: dense first
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
        // Small primes for gaps
        17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79,
        // Medium primes
        83, 89, 97, 101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157, 163,
    ]
}

/// Full SSP5 encode with BWT + MTF + RLE + SSP (Range Coder)
pub fn ssp5_encode(data: &[u8], s: &[u64], block_bits: usize) -> Vec<u8> {
    // Step 1: BWT
    let (primary, last) = bwt_encode(data);
    
    // Step 2: MTF
    let mtf_indices = mtf_encode(&last);
    
    // Step 3: RLE — compress runs of identical MTF indices
    let (rle_data, _header_len) = rle_encode_v2(&mtf_indices);
    
    // Decide: use RLE only if it actually compresses
    let use_rle = rle_data.len() < mtf_indices.len() && data.len() >= 64;
    
    let encode_data = if use_rle { &rle_data } else { &mtf_indices };
    
    // Step 4: SSP with Range Coder
    let ssp_encoded = ssp_encode(encode_data, s, block_bits, false);
    
    // Build archive
    let s_len = s.len() as u8;
    let mut out = Vec::with_capacity(
        4 + 1 + 1 + s_len as usize * 8 + 1 + 4 + ssp_encoded.len() + 4
    );
    out.extend_from_slice(SSP5_MAGIC);
    out.push(SSP5_VERSION);
    out.push(s_len);
    for &val in s.iter().take(s_len as usize) {
        out.extend_from_slice(&val.to_le_bytes());
    }
    out.push(block_bits as u8);
    out.extend_from_slice(&primary.to_le_bytes());
    
    // Write compression metadata: use_rle flag (1 byte)
    out.push(if use_rle { 1 } else { 0 });
    
    // Write length of encoded data (ULEB)
    let mut dlen = encode_data.len() as u64;
    while dlen >= 0x80 {
        out.push(((dlen & 0x7F) | 0x80) as u8);
        dlen >>= 7;
    }
    out.push((dlen & 0x7F) as u8);
    
    out.extend_from_slice(&ssp_encoded);
    
    out
}

/// Full SSP5 decode with SSP (Range Coder) + RLE + MTF + BWT
pub fn ssp5_decode(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.len() < 4 + 1 + 1 + 1 + 4 {
        return Err("Archive too short for SSP5 header");
    }

    if &archive[0..4] != SSP5_MAGIC {
        return Err("Bad magic (expected SSP5)");
    }
    
    let version = archive[4];
    if version != SSP5_VERSION {
        return Err("Unsupported SSP5 version");
    }
    
    let mut pos = 5;
    let s_len = archive[pos] as usize;
    pos += 1;
    
    let s: Vec<u64> = (0..s_len).map(|_| {
        let val = u64::from_le_bytes([
            archive[pos], archive[pos+1], archive[pos+2], archive[pos+3],
            archive[pos+4], archive[pos+5], archive[pos+6], archive[pos+7]
        ]);
        pos += 8;
        val
    }).collect();
    
    if pos + 1 + 4 > archive.len() {
        return Err("Archive too short");
    }
    
    let block_bits = archive[pos] as usize;
    pos += 1;
    
    let primary = u32::from_le_bytes([
        archive[pos], archive[pos+1], archive[pos+2], archive[pos+3]
    ]);
    pos += 4;
    
    if pos >= archive.len() {
        return Ok(Vec::new());
    }
    
    // Read use_rle flag
    let use_rle = archive[pos] == 1;
    pos += 1;
    
    // Read encoded data length (ULEB)
    let mut dlen = 0u64;
    let mut shift = 0;
    while pos < archive.len() {
        let b = archive[pos];
        pos += 1;
        dlen |= ((b & 0x7F) as u64) << shift;
        if (b & 0x80) == 0 { break; }
        shift += 7;
    }
    let dlen_usize = dlen as usize;
    
    if pos + dlen_usize > archive.len() {
        return Err("Archive truncated");
    }
    
    let encoded_data = &archive[pos..pos + dlen_usize];
    pos += dlen_usize;
    
    // Decode SSP
    let ssp_decoded = ssp_decode(encoded_data, &s)?;
    
    // Decode RLE if used
    let mtf_indices = if use_rle {
        rle_decode_v2(ssp_decoded, 0) // 0 = no header, decode entire thing
    } else {
        ssp_decoded
    };
    
    // Decode MTF
    let mtf_decoded = mtf_decode(&mtf_indices);
    
    // Decode BWT
    let original = bwt_decode(primary, &mtf_decoded);
    
    Ok(original)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_s() -> Vec<u64> {
        vec![
            2, 3, 5, 7, 11, 13, 17, 19, 23, 29,
            31, 37, 41, 43, 47, 53, 59, 61, 67, 71,
            73, 79, 83, 89, 97, 101, 103, 107, 109, 113,
            127, 131, 137, 139, 149, 151, 157, 163, 167, 173,
            179, 181, 191, 193, 197, 199, 211, 223, 227, 229,
            233, 239, 241, 251, 257, 263, 269, 271, 277, 281,
            283, 293, 307, 311
        ]
    }

    #[test]
    fn test_rle_roundtrip() {
        let cases = vec![
            b"aaaaabbbbbccccc",
            b"hello world",
            b"\x00\x00\x00\x01\x01\x02\x02\x02\x02\x03",
            b"ababababab",
        ];
        
        for data in cases {
            let (encoded, _) = rle_encode_v2(data);
            let decoded = rle_decode_v2(&encoded, 0);
            assert_eq!(decoded, data, "RLE roundtrip failed");
        }
    }

    #[test]
    fn test_rle_compression() {
        let data = b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        let (encoded, _) = rle_encode_v2(data);
        println!("RLE: {} -> {} ({:.1}%)", data.len(), encoded.len(), 
                 encoded.len() as f64 / data.len() as f64 * 100.0);
        assert!(encoded.len() < data.len(), "RLE should compress runs");
    }

    #[test]
    fn test_ssp5_rle() {
        let s = default_s();
        let data = b"banana banana banana";
        
        let encoded = ssp5_encode(data, &s, 16);
        let decoded = ssp5_decode(&encoded).unwrap();
        
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_mtf_analysis() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let mtf = mtf_encode(data);
        analyze_mtf(&mtf);
    }
}
