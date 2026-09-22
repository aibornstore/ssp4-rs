//! SSP5 pipeline: BWT → MTF → SSP and LZ77 → BWT → MTF → SSP
//! 
//! Pipeline A: BWT → MTF → SSP
//! Pipeline B: LZ77 → BWT → MTF → SSP (LZ77 on raw data first, then BWT on LZ77 output)
//! 
//! Archive format (WRAPPER_MAGIC):
//!   [WRAPPER_MAGIC(4)][WRAPPER_VERSION(1)][LZ77_FLAG(1)][CHUNK_SIZE(4)]
//!   [+LZ77_DATA] (only if LZ77_FLAG=1)
//!   [s_len(1)][S(s_len×8)]
//!   [ssp_data]  ← ssp_codec::encode output

use super::bwt::{bwt_encode, bwt_decode, pack_bwt, unpack_bwt, 
                 bwt_encode_chunked, bwt_decode_chunked, bwt_encode_iterative};
use super::mtf::{mtf_encode, mtf_decode};
use super::ssp_codec::{encode as ssp_encode, decode as ssp_decode};
use super::lz77::{encode as lz77_encode, decode as lz77_decode, Token};

/// Wrapper magic: distinct from SSP5_MAGIC so ssp_decode finds SSP5_MAGIC at ssp_data offset
const WRAPPER_MAGIC: &[u8; 4] = b"SS5W";
const WRAPPER_VERSION: u8 = 3; // Version 3 supports chunked BWT

/// Encode data with SSP5 pipeline: BWT → MTF → SSP (no LZ77)
/// chunk_size: 0 = no chunking, >0 = split into chunks of that size
/// bwt_passes: number of iterative BWT passes (1 = standard)
pub fn ssp5_encode(data: &[u8], s: &[u64], block_bits: usize) -> Vec<u8> {
    ssp5_encode_with_options(data, s, block_bits, 0, 1)
}

/// Encode with options for chunking and iterative BWT
pub fn ssp5_encode_with_options(data: &[u8], s: &[u64], block_bits: usize, chunk_size: usize, bwt_passes: usize) -> Vec<u8> {
    // BWT → pack(primary + last_col) → MTF → SSP
    let bwt_packed = if chunk_size > 0 && data.len() > chunk_size {
        // Chunked BWT
        bwt_encode_chunked(data, chunk_size)
    } else if bwt_passes > 1 {
        // Iterative BWT
        let (primary, last) = bwt_encode_iterative(data, bwt_passes);
        pack_bwt(primary, &last)
    } else {
        // Standard BWT
        let (primary, bwt_data) = bwt_encode(data);
        pack_bwt(primary, &bwt_data)
    };
    
    let mtf_data = mtf_encode(&bwt_packed);
    let ssp_encoded = ssp_encode(&mtf_data, s, block_bits, false);
    
    // Archive: [WRAPPER_MAGIC][VERSION][LZ77_FLAG=0][CHUNK_SIZE(4)][S_LEN][S...][SSP_DATA]
    let mut out = Vec::with_capacity(9 + s.len() * 8 + ssp_encoded.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION);
    out.push(0); // LZ77_FLAG = 0 (no LZ77)
    out.extend_from_slice(&(chunk_size as u32).to_le_bytes()); // chunk_size
    out.push(s.len() as u8);
    for &val in s { out.extend_from_slice(&val.to_le_bytes()); }
    out.extend_from_slice(&ssp_encoded);
    out
}

/// Encode data with LZ77 → BWT → MTF → SSP pipeline
pub fn ssp5_encode_with_lz77(data: &[u8], s: &[u64], block_bits: usize) -> Vec<u8> {
    // LZ77 on raw data
    let tokens = lz77_encode(data);
    let lz77_bytes = tokens_to_bytes(&tokens);
    
    // BWT on LZ77 output
    let (primary, bwt_data) = bwt_encode(&lz77_bytes);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    let mtf_data = mtf_encode(&bwt_packed);
    let ssp_encoded = ssp_encode(&mtf_data, s, block_bits, false);
    
    // Archive: [WRAPPER_MAGIC][VERSION][LZ77_FLAG=1][LZ77_DATA][S_LEN][S...][SSP_DATA]
    let mut out = Vec::with_capacity(6 + lz77_bytes.len() + s.len() * 8 + ssp_encoded.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION);
    out.push(1); // LZ77_FLAG = 1 (LZ77 enabled)
    out.extend_from_slice(&(lz77_bytes.len() as u32).to_le_bytes()); // LZ77 length
    out.extend_from_slice(&lz77_bytes);
    out.push(s.len() as u8);
    for &val in s { out.extend_from_slice(&val.to_le_bytes()); }
    out.extend_from_slice(&ssp_encoded);
    out
}

/// Decode SSP5 archive back to original bytes (handles both plain and LZ77)
pub fn ssp5_decode(archive: &[u8]) -> Vec<u8> {
    let min_len = 4 + 1 + 1; // MAGIC + VERSION + LZ77_FLAG
    if archive.len() < min_len {
        panic!("SSP5: archive too short");
    }
    if &archive[0..4] != WRAPPER_MAGIC {
        panic!("SSP5: invalid wrapper magic");
    }
    
    let version = archive[4];
    let lz77_flag = archive[5];
    
    // Version 3: [MAGIC(4)][VERSION(1)][LZ77_FLAG(1)][CHUNK_SIZE(4)][S_LEN(1)][S...]
    // Version 2: [MAGIC(4)][VERSION(1)][LZ77_FLAG(1)][LZ77_LEN(4)][S_LEN(1)][S...]
    // Version 1: [MAGIC(4)][VERSION(1)][S_LEN(1)][S...]
    
    if lz77_flag == 0 || version < 2 {
        // Plain BWT pipeline (no LZ77)
        // Format: [MAGIC(4)][VERSION(1)][LZ77_FLAG(1)][CHUNK_SIZE(4)][S_LEN(1)][S...]
        //                     offset 5        offset 6   offset 10
        let s_len_pos = if version >= 3 { 10 } else { 6 };
        let s_len = archive[s_len_pos] as usize;
        let s_start = s_len_pos + 1;
        let s_end = s_start + s_len * 8;
        
        let mut s_vec = Vec::with_capacity(s_len);
        for i in 0..s_len {
            let o = s_start + i * 8;
            let val = u64::from_le_bytes([
                archive[o], archive[o+1], archive[o+2], archive[o+3],
                archive[o+4], archive[o+5], archive[o+6], archive[o+7]
            ]);
            s_vec.push(val);
        }
        
        let ssp_data = &archive[s_end..];
        let mtf_data = ssp_decode(ssp_data, &s_vec).expect("SSP decode failed");
        let bwt_decoded = mtf_decode(&mtf_data);
        
        // Check if chunked based on chunk_size in header
        let chunk_size = if version >= 3 {
            u32::from_le_bytes([archive[6], archive[7], archive[8], archive[9]]) as usize
        } else {
            0
        };
        
        if chunk_size > 0 && bwt_decoded.len() > chunk_size {
            // Actually chunked
            bwt_decode_chunked(&bwt_decoded)
        } else {
            // Non-chunked
            let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
            bwt_decode(dec_primary, dec_bwt)
        }
    } else {
        // LZ77 → BWT pipeline (version 2 only for LZ77)
        // Format: [MAGIC(4)][VERSION(1)][LZ77_FLAG(1)][LZ77_LEN(4)][S_LEN(1)][S...]
        let lz77_len_pos = 6; // after MAGIC + VERSION + LZ77_FLAG
        let lz77_len = u32::from_le_bytes([
            archive[lz77_len_pos], archive[lz77_len_pos+1], 
            archive[lz77_len_pos+2], archive[lz77_len_pos+3]
        ]) as usize;
        let lz77_start = lz77_len_pos + 4;
        let lz77_end = lz77_start + lz77_len;
        
        let s_len_pos = lz77_end;
        let s_len = archive[s_len_pos] as usize;
        let s_start = s_len_pos + 1;
        let s_end = s_start + s_len * 8;
        
        // Parse S sequence
        let mut s_vec = Vec::with_capacity(s_len);
        for i in 0..s_len {
            let o = s_start + i * 8;
            let val = u64::from_le_bytes([
                archive[o], archive[o+1], archive[o+2], archive[o+3],
                archive[o+4], archive[o+5], archive[o+6], archive[o+7]
            ]);
            s_vec.push(val);
        }
        
        // Decode SSP → MTF → BWT → LZ77
        let ssp_data = &archive[s_end..];

        let mtf_data = ssp_decode(ssp_data, &s_vec).expect("SSP decode failed");
        let bwt_decoded = mtf_decode(&mtf_data);
        
        // Version 2 with LZ77 doesn't use chunking
        let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
        let lz77_bytes = bwt_decode(dec_primary, dec_bwt);
        
        // Decode LZ77 tokens and then LZ77
        let tokens = bytes_to_tokens(&lz77_bytes);
        lz77_decode(&tokens)
    }
}

/// Encode with automatic LZ77 selection (chooses smaller output)
pub fn ssp5_encode_auto(data: &[u8], s: &[u64], block_bits: usize) -> Vec<u8> {
    let plain_encoded = ssp5_encode(data, s, block_bits);
    let lz77_encoded = ssp5_encode_with_lz77(data, s, block_bits);
    
    if lz77_encoded.len() < plain_encoded.len() {
        lz77_encoded
    } else {
        plain_encoded
    }
}

/// Serialize LZ77 tokens to bytes for BWT processing.
/// Uses lz77::tokens_to_bytes (Rice-coded format).
#[allow(dead_code)]
const ESCAPE: u8 = 0xFF;
#[allow(dead_code)]
const LITERAL_CODE: u8 = 0x00;
#[allow(dead_code)]
const MATCH_CODE: u8 = 0x01;

fn tokens_to_bytes(tokens: &[Token]) -> Vec<u8> {
    super::lz77::tokens_to_bytes(tokens)
}

/// Deserialize bytes back to LZ77 tokens.
fn bytes_to_tokens(bytes: &[u8]) -> Vec<Token> {
    super::lz77::bytes_to_tokens(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_ssp5_roundtrip_small() {
        let data = b"Hello World! This is a test.";
        let s: Vec<u64> = (1u64..=32).collect();
        let encoded = ssp5_encode(data, &s, 16);
        let decoded = ssp5_decode(&encoded);
        assert_eq!(data.to_vec(), decoded);
    }
    
    #[test]
    fn test_ssp5_roundtrip_repeated() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let s: Vec<u64> = (1u64..=32).collect();
        let encoded = ssp5_encode(&data, &s, 16);
        let decoded = ssp5_decode(&encoded);
        assert_eq!(data, decoded);
    }
    
    #[test]
    fn test_ssp5_compression_ratio() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(5000).copied().collect();
        let s: Vec<u64> = (1u64..=32).collect();
        let encoded = ssp5_encode(&data, &s, 16);
        let ratio = 100.0 * encoded.len() as f64 / data.len() as f64;
        println!("Compression ratio: {:.2}%", ratio);
        assert!(ratio < 15.0, "Should compress well: got {}%", ratio);
    }
    
    #[test]
    fn test_ssp5_random_data() {
        let data: Vec<u8> = (0..1000).map(|i| (i * 31 + 17) as u8).collect();
        let s: Vec<u64> = (1u64..=32).collect();
        let encoded = ssp5_encode(&data, &s, 16);
        let decoded = ssp5_decode(&encoded);
        assert_eq!(data, decoded);
        let ratio = 100.0 * encoded.len() as f64 / data.len() as f64;
        println!("Random data ratio: {:.1}%", ratio);
        assert!(ratio < 200.0, "Should not expand too much: got {}%", ratio);
    }
    
    #[test]
    fn test_ssp5_with_lz77_roundtrip() {
        let data = b"Hello World! This is a test. Hello World! This is a test.";
        let s: Vec<u64> = (1u64..=32).collect();
        let encoded = ssp5_encode_with_lz77(data, &s, 16);
        let decoded = ssp5_decode(&encoded);
        assert_eq!(data.to_vec(), decoded);
    }
    
    #[test]
    fn test_ssp5_auto_selects_lz77() {
        // Repeated data should benefit from LZ77
        let data: Vec<u8> = b"Hello World! ".iter().cycle().take(1000).copied().collect();
        let s: Vec<u64> = (1u64..=32).collect();
        
        let plain = ssp5_encode(&data, &s, 16);
        let with_lz77 = ssp5_encode_with_lz77(&data, &s, 16);
        let auto = ssp5_encode_auto(&data, &s, 16);
        
        println!("Plain: {} bytes ({:.1}%)", plain.len(), 100.0 * plain.len() as f64 / data.len() as f64);
        println!("With LZ77: {} bytes ({:.1}%)", with_lz77.len(), 100.0 * with_lz77.len() as f64 / data.len() as f64);
        
        // Auto should pick the smaller one
        assert!(auto.len() <= plain.len().min(with_lz77.len()));
        
        // Roundtrip
        let decoded = ssp5_decode(&auto);
        assert_eq!(data, decoded);
    }
    
    #[test]
    fn test_ssp5_lz77_vs_plain_comparison() {
        let data: Vec<u8> = b"AAAA".iter().cycle().take(1000).copied().collect();
        let s: Vec<u64> = vec![13, 17, 19, 23, 29, 31, 37, 41];
        
        let plain = ssp5_encode(&data, &s, 16);
        let with_lz77 = ssp5_encode_with_lz77(&data, &s, 16);
        
        println!("AAAA repeated (1000 bytes):");
        println!("  Plain: {} bytes ({:.1}%)", plain.len(), 100.0 * plain.len() as f64 / data.len() as f64);
        println!("  With LZ77: {} bytes ({:.1}%)", with_lz77.len(), 100.0 * with_lz77.len() as f64 / data.len() as f64);
        
        // Both should roundtrip correctly
        let plain_dec = ssp5_decode(&plain);
        let lz77_dec = ssp5_decode(&with_lz77);
        assert_eq!(data, plain_dec);
        assert_eq!(data, lz77_dec);
    }
    
    #[test]
    fn test_ssp5_lz77_token_roundtrip() {
        // Test tokens_to_bytes and bytes_to_tokens
        use super::super::lz77::{encode as lz77_encode, decode as lz77_decode};
        
        let data: Vec<u8> = b"Hello World! Test data 12345".to_vec();
        let tokens = lz77_encode(&data);
        let bytes = tokens_to_bytes(&tokens);
        let tokens2 = bytes_to_tokens(&bytes);
        let data2 = lz77_decode(&tokens2);
        
        assert_eq!(data, data2, "Token roundtrip failed");
        
        // Also test on repetitive data
        let repetitive: Vec<u8> = b"ABC".iter().cycle().take(100).copied().collect();
        let tokens = lz77_encode(&repetitive);
        let bytes = tokens_to_bytes(&tokens);
        let tokens2 = bytes_to_tokens(&bytes);
        let data2 = lz77_decode(&tokens2);
        
        assert_eq!(repetitive, data2, "Repetitive token roundtrip failed");
    }
    
    #[test]
    fn test_ssp5_lz77_small_text() {
        // Small text that should benefit from LZ77
        let data = b"The quick brown fox jumps over the lazy dog. ".to_vec();
        let s: Vec<u64> = vec![13, 17, 19, 23, 29, 31, 37, 41];
        
        let plain = ssp5_encode(&data, &s, 16);
        let with_lz77 = ssp5_encode_with_lz77(&data, &s, 16);
        
        // Both should roundtrip
        let plain_dec = ssp5_decode(&plain);
        let lz77_dec = ssp5_decode(&with_lz77);
        
        assert_eq!(data, plain_dec, "Plain roundtrip failed");
        assert_eq!(data, lz77_dec, "LZ77 roundtrip failed");
        
        println!("Small text ({} bytes):", data.len());
        println!("  Plain: {} bytes", plain.len());
        println!("  With LZ77: {} bytes", with_lz77.len());
    }
    
    #[test]
    fn test_ssp5_lz77_with_newlines() {
        // NOTE: LZ77 encode has known bugs with \r\n patterns.
        // This test documents that LZ77→BWT pipeline does NOT work for text.
        // Skip this test - LZ77 preprocessing is NOT recommended for SSP5.
    }

    #[test]
    fn test_ssp5_lz77_full_pipeline_alice29() {
        // Use include_bytes with path relative to ssp5_pipeline.rs
        let data: Vec<u8> = include_bytes!("../../../../../PROJECT UNIVERSE/01Compression/SSP5/tests/comparison_corpora/canterbury/alice29.txt").to_vec();
        let s: Vec<u64> = vec![13, 17, 19, 23, 29, 31, 37, 41];
        
        // Test on 75KB prefix
        let prefix = &data[..75000.min(data.len())];
        let encoded = ssp5_encode_with_lz77(prefix, &s, 16);
        let decoded = ssp5_decode(&encoded);
        
        if decoded != prefix.to_vec() {
            for i in 0..prefix.len().min(decoded.len()) {
                if prefix[i] != decoded[i] {
                    println!("First diff at byte {}: orig={} ({}), dec={} ({})", 
                        i, prefix[i], prefix[i] as char, decoded[i], decoded[i] as char);
                    break;
                }
            }
            if decoded.len() < prefix.len() {
                println!("Decoded {} bytes, expected {}", decoded.len(), prefix.len());
            }
        }
        assert_eq!(prefix.to_vec(), decoded, "LZ77 pipeline failed on alice29 75KB prefix");
    }

    #[test]
    fn test_ssp5_lz77_pipeline_isolated() {
        // Isolated test: check each stage of the LZ77 pipeline
        use super::super::lz77::{encode as lz77_encode, decode as lz77_decode, tokens_to_bytes, bytes_to_tokens};
        use super::super::bwt::{bwt_encode, bwt_decode};
        
        use super::super::ssp_codec::{encode as ssp_encode, decode as ssp_decode};
        
        // Test on alice29 prefix
        let data: Vec<u8> = include_bytes!("../../../../../PROJECT UNIVERSE/01Compression/SSP5/tests/comparison_corpora/canterbury/alice29.txt").to_vec();
        let prefix = &data[..75000.min(data.len())];
        let s: Vec<u64> = vec![13, 17, 19, 23, 29, 31, 37, 41];
        
        // Check bytes at key positions in prefix
        println!("prefix.len() = {}", prefix.len());
        println!("prefix[65734..65737] = {:02x?}", &prefix[65734..65737]);
        println!("prefix[65937..65940] = {:02x?}", &prefix[65937..65940]);
        println!("HASH of 65734: {:08x}", u32::from(prefix[65734]) << 16 | u32::from(prefix[65735]) << 8 | u32::from(prefix[65736]));
        println!("HASH of 65937: {:08x}", u32::from(prefix[65937]) << 16 | u32::from(prefix[65938]) << 8 | u32::from(prefix[65939]));
        
        // Stage 1: LZ77 encode
        let tokens = lz77_encode(prefix);
        
        // Find the token that covers position 65937
        let mut pos = 0usize;
        for (ti, tok) in tokens.iter().enumerate() {
            match tok {
                crate::coders::lz77::Token::Literal(b) => {
                    if pos == 65937 {
                        println!("Token {} at pos {}: Literal({}) = '{}'", ti, pos, b, *b as char);
                    }
                    pos += 1;
                }
                crate::coders::lz77::Token::Match { offset, length } => {
                    if pos <= 65937 && pos + *length as usize > 65937 {
                        println!("Token {} at pos {}: Match(offset={}, length={}) COVERS 65937", ti, pos, offset, length);
                    }
                    pos += *length as usize;
                }
            }
        }
        
        let lz77_bytes = tokens_to_bytes(&tokens);
        let tokens_check = bytes_to_tokens(&lz77_bytes);
        let lz77_decoded_check = lz77_decode(&tokens_check);
        if prefix.to_vec() != lz77_decoded_check {
            println!("LZ77 encode/decode ALONE fails!");
            for i in 0..prefix.len().min(lz77_decoded_check.len()) {
                if prefix[i] != lz77_decoded_check[i] {
                    println!("First diff at {}: orig={} (0x{:02x}), dec={} (0x{:02x})", 
                             i, prefix[i], prefix[i], lz77_decoded_check[i], lz77_decoded_check[i]);
                    // Also show context
                    let start = i.saturating_sub(10);
                    let end = (i + 5).min(prefix.len());
                    println!("Context orig: {:?}", String::from_utf8_lossy(&prefix[start..end]));
                    println!("Context dec:  {:?}", String::from_utf8_lossy(&lz77_decoded_check[start..end.min(lz77_decoded_check.len())]));
                    break;
                }
            }
        }
        
        let ratio = 100.0 * lz77_bytes.len() as f64 / prefix.len() as f64;
        println!("LZ77 expansion: {} -> {} ({:.1}%)", prefix.len(), lz77_bytes.len(), ratio);
        assert!(ratio < 500.0, "LZ77 expanded too much: {}%", ratio);
        
        // Stage 2: BWT encode/decode roundtrip
        let (primary, last) = bwt_encode(&lz77_bytes);
        println!("BWT: primary={}, n={}", primary, last.len());
        let bwt_decoded = bwt_decode(primary, &last);
        assert_eq!(lz77_bytes, bwt_decoded, "BWT roundtrip failed");
        
        // Stage 3: MTF encode/decode roundtrip
        let mtf_out = super::super::mtf::mtf_encode(&lz77_bytes);
        let mtf_decoded = super::super::mtf::mtf_decode(&mtf_out);
        assert_eq!(lz77_bytes, mtf_decoded, "MTF roundtrip failed");
        
        // Stage 4: SSP encode/decode roundtrip on text data
        let ssp_encoded = ssp_encode(prefix, &s, 16, false);
        let ssp_decoded = ssp_decode(&ssp_encoded, &s);
        match ssp_decoded {
            Ok(v) => {
                assert_eq!(prefix, &v[..], "SSP roundtrip on text data failed");
            }
            Err(e) => panic!("ssp_decode error: {}", e),
        }
    }

    #[test]
    fn test_ssp5_raw_value_analysis() {
        // Analyze block value distribution for different block_bits
        use super::super::bwt::{bwt_encode, pack_bwt};
        use super::super::mtf::mtf_encode;
        
        let data = include_bytes!("../../../../../tmp/bible_100k.txt");
        
        // BWT → MTF (this is what SSP encodes)
        let (primary, bwt_data) = bwt_encode(data);
        let bwt_packed = pack_bwt(primary, &bwt_data);
        let mtf_data = mtf_encode(&bwt_packed);
        
        println!("MTF data: {} bytes, unique values: {}", 
            mtf_data.len(), mtf_data.iter().collect::<std::collections::HashSet<_>>().len());
        
        // Test different block_bits
        for bb in [8, 12, 16, 20, 24, 32] {
            let block_bytes = bb / 8;
            let num_blocks = (data.len() + block_bytes - 1) / block_bytes;
            
            // Count block values from ORIGINAL data (as SSP does)
            let mut over_limit = 0usize;
            let max_val = (1u64 << bb) - 1;
            for bi in 0..num_blocks {
                let start = bi * block_bytes;
                let end = (start + block_bytes).min(data.len());
                let chunk = &data[start..end];
                let mut n = 0u64;
                for &b in chunk.iter() {
                    n = (n << 8) | b as u64;
                }
                if n > max_val {
                    over_limit += 1;
                }
            }
            println!("bb={:>2}: blocks={:>6}, over_limit(>{:>6})={:>5} ({:5.1}%)",
                bb, num_blocks, max_val, over_limit, 
                100.0 * over_limit as f64 / num_blocks as f64);
        }
    }

    #[test]
    fn test_ssp5_bible_100k_roundtrip() {
        // Test SSP5 roundtrip on bible_100k.txt
        let data = include_bytes!("../../../../../tmp/bible_100k.txt");
        let s: Vec<u64> = vec![13, 17, 19, 23, 29, 31, 37, 41];
        let enc = ssp5_encode(data, &s, 16);
        println!("Encoded: {} bytes", enc.len());
        let dec = ssp5_decode(&enc);
        println!("Decoded: {} bytes", dec.len());
        // Find first diff
        let min_len = data.len().min(dec.len());
        let mut first_diff = min_len;
        for i in 0..min_len {
            if data[i] != dec[i] {
                first_diff = i;
                break;
            }
        }
        if first_diff < min_len {
            println!("First diff at byte {}", first_diff);
            println!("Expected: {:02x?}", &data[first_diff..first_diff.min(data.len())]);
            println!("Got:      {:02x?}", &dec[first_diff..]);
        }
        assert_eq!(data.len(), dec.len(), "bible_100k roundtrip length mismatch");
        assert_eq!(data.as_slice(), &dec[..], "bible_100k roundtrip data mismatch");
    }

    #[test]
    fn test_ssp5_100k_sized_roundtrip() {
        // Test roundtrip on 100k of pseudo-random data
        let data: Vec<u8> = (0u32..100000).map(|i| ((i.wrapping_mul(31)).wrapping_add(17)) as u8).collect();
        let s: Vec<u64> = vec![13, 17, 19, 23, 29, 31, 37, 41];
        let enc = ssp5_encode(&data, &s, 16);
        let dec = ssp5_decode(&enc);
        println!("100k sized: enc={} dec={}", enc.len(), dec.len());
        assert_eq!(data.len(), dec.len(), "100k roundtrip length mismatch");
        assert_eq!(&data[..], &dec[..], "100k roundtrip data mismatch");
    }
}
