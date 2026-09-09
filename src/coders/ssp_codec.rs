//! Core SSP codec — encodes byte sequences using special prime sequences
//! Corresponds to ssp4_local_v44.py encode_data/decode_data core SSP path (lines ~14043-14582)
//!
//! Format: SSP5 magic + flags + block_bits + ULEB(m,r,len,nblocks) + body
//! Body: variable-length tag encoding with ULEB k-values for symbols

use std::collections::HashMap;

use super::bit_io::{BitReader, BitWriter};
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

    // flags: bit0=delta, bit1=RLE, bit2=MFS, bit3=odd_sep
    let flags = (if delta { 1u8 } else { 0 })
        | (if rle_used { 2u8 } else { 0 })
        | (if mfs_used { 4u8 } else { 0 })
        | (if use_odd_sep { 8u8 } else { 0 });
    header.push(flags);
    header.push(block_bits as u8);
    // ULEB(m), ULEB(r), ULEB(data_len), ULEB(num_blocks) — matches Python format
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

    // Encode body
    let mut bw = BitWriter::new();
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
                // ZERO-RLE: 3-bit tag 110
                bw.write_bits(0b110, 3);
                bw.write_uleb(*value);
            }
            4 => {
                // MFS: 3-bit tag 111, no payload
                bw.write_bits(0b111, 3);
            }
            5 => {
                // RAW-RLE: 3-bit tag 111, value bits + run ULEB
                bw.write_bits(0b111, 3);
                bw.write_bits(*value, block_bits);
                bw.write_uleb(run);
            }
            _ => {}
        }
    }

    let body = bw.flush();
    header.extend_from_slice(&body);
    header
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
    let delta = (flags & 1) != 0;
    let rle_enabled = (flags & 2) != 0;
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

    let body = &archive[off..];

    // Decode body
    let mut br = BitReader::new(body);
    let mut output = Vec::with_capacity(num_blocks as usize * block_bytes);
    let mut prev_n: u64 = 0;
    let mut blocks_decoded: u64 = 0;

    while blocks_decoded < num_blocks {
        // Read tag (2 bits for Phase A, 3 bits for Phase B extended)
        if br.is_exhausted() {
            break;
        }
        let tag_bits = br.read_bits(2).unwrap_or(0);

        // Phase B extended: 3-bit tags when first 2 bits are 11
        // 110 = ZERO-RLE (tag=3), 111 = MFS (tag=4) or RAW-RLE (tag=5)
        let (tag, _count) = if tag_bits == 3 {
            let third = br.read_bit().unwrap_or(0);
            if third == 0 && rle_enabled {
                // 110 = ZERO-RLE
                (3u8, 1u64)
            } else if third == 1 && mfs_enabled {
                // 111 = MFS (tag=4)
                (4u8, 1u64)
            } else {
                // RAW-RLE: 111 with mfs disabled — treat as RAW-RLE
                // Read RAW-RLE payload: value bits + run ULEB
                let raw_val = br.read_bits(block_bits).unwrap_or(0);
                let run = br.read_uleb().unwrap_or(1);
                // Emit RAW blocks
                for _ in 0..run {
                    let n = if delta {
                        prev_n.wrapping_add(raw_val) & max_block_val
                    } else {
                        raw_val
                    };
                    prev_n = n;
                    let needed = (block_bits + 7) / 8;
                    let be_bytes = n.to_be_bytes();
                    output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                }
                blocks_decoded += run;
                continue;
            }
        } else {
            (tag_bits as u8, 1u64)
        };

        // skip_to_byte_boundary: for SYM (tag=0) and ZERO-RLE (tag=3),
        // the next payload (ULEB) starts at the next byte boundary.
        // For RAW (tag=1) and ZERO (tag=2): payload starts immediately.
        if tag == 0 || tag == 3 {
            br.skip_to_byte_boundary();
        }

        match tag {
            0 => {
                // SYM
                let count = 1u64;
                if use_odd_sep {
                    let odd = br.read_bit().unwrap_or(0) as u64;
                    let k = br.read_uleb().unwrap_or(0);
                    let sym_val = if k > 0 {
                        decode_symbol(k, s).unwrap_or(0)
                    } else {
                        0
                    };
                    let val = sym_val + odd;
                    let n = if delta {
                        prev_n.wrapping_add(val) & max_block_val
                    } else {
                        val
                    };
                    prev_n = n;
                    for _ in 0..count {
                        let needed = (block_bits + 7) / 8;
                        let be_bytes = n.to_be_bytes();
                        output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                    }
                } else {
                    let pk = br.read_uleb().unwrap_or(0);
                    let k = pk >> 1;
                    let odd = pk & 1;
                    let sym_val = if k > 0 {
                        decode_symbol(k, s).unwrap_or(0)
                    } else {
                        0
                    };
                    let val = sym_val + odd;
                    let n = if delta {
                        prev_n.wrapping_add(val) & max_block_val
                    } else {
                        val
                    };
                    prev_n = n;
                    for _ in 0..count {
                        let needed = (block_bits + 7) / 8;
                        let be_bytes = n.to_be_bytes();
                        output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                    }
                }
                blocks_decoded += count;
            }
            1 => {
                // RAW
                let count = 1u64;
                let val = br.read_bits(block_bits as usize).unwrap_or(0);
                let n = if delta {
                    prev_n.wrapping_add(val) & max_block_val
                } else {
                    val
                };
                prev_n = n;
                for _ in 0..count {
                    // Convert n to bytes (big-endian within block)
                    let needed = (block_bits + 7) / 8;
                    let be_bytes = n.to_be_bytes();
                    output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                }
                blocks_decoded += count;
            }
            2 => {
                // ZERO
                let count = 1u64;
                let n = if delta { prev_n } else { 0 };
                prev_n = n;
                for _ in 0..count {
                    let needed = (block_bits + 7) / 8;
                    let be_bytes = n.to_be_bytes();
                    output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                }
                blocks_decoded += count;
            }
            3 => {
                // ZERO-RLE (3 bits: 110)
                let count = br.read_uleb().unwrap_or(0);
                for _ in 0..count {
                    let n = if delta { prev_n } else { 0 };
                    prev_n = n;
                    let needed = (block_bits + 7) / 8;
                    let be_bytes = n.to_be_bytes();
                    output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                }
                blocks_decoded += count;
            }
            4 => {
                // MFS: 3 bits 111, no payload — value is mfs_val
                // (count comes from the 3-bit tag dispatch above)
                let n = if delta {
                    prev_n.wrapping_add(mfs_val) & max_block_val
                } else {
                    mfs_val
                };
                prev_n = n;
                let needed = (block_bits + 7) / 8;
                let be_bytes = n.to_be_bytes();
                output.extend_from_slice(&be_bytes[be_bytes.len() - needed..]);
                blocks_decoded += 1;
            }
            _ => {
                break;
            }
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

    fn rand_u8() -> u8 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        ((nanos as u64 * 1103515245 + 12345) >> 16) as u8
    }
}
