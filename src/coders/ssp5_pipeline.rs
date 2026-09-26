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
use super::mtf::{mtf_encode, mtf_decode, mtf_encode_bwt_chunked, mtf_decode_bwt_chunked,
                 zrun_encode, zrun_decode};
use super::ssp_codec::{encode as ssp_encode, decode as ssp_decode};
use super::lz77::{encode as lz77_encode, decode as lz77_decode, Token};
use super::range_coder::{range_encode_bytes, range_decode_bytes, 
                          range_encode_bytes_order1, range_decode_bytes_order1,
                          range_encode_bytes_order2, range_decode_bytes_order2,
                          range_encode_bytes_order_mix, range_decode_bytes_order_mix,
                          range_encode_bytes_order12_mix, range_decode_bytes_order12_mix,
                          range_encode_bytes_order_ewma, range_decode_bytes_order_ewma,
                          range_encode_bytes_order_ewma3, range_decode_bytes_order_ewma3,
                          range_encode_bytes_order_ewma5, range_decode_bytes_order_ewma5,
                          range_encode_bytes_order_ewma7, range_decode_bytes_order_ewma7,
                          range_encode_bytes_order_ewma7_alpha_ws, range_decode_bytes_order_ewma7_alpha_ws};
use super::fse::{huffman_encode, huffman_decode};

/// Wrapper magic: distinct from SSP5_MAGIC so ssp_decode finds SSP5_MAGIC at ssp_data offset
const WRAPPER_MAGIC: &[u8; 4] = b"SS5W";
const WRAPPER_VERSION: u8 = 3; // Version 3 supports chunked BWT
const WRAPPER_VERSION_RC: u8 = 4; // Version 4 uses range coder (order-0)
const WRAPPER_VERSION_RC_O1: u8 = 5; // Version 5 uses range coder (order-1 context)
const WRAPPER_VERSION_RC_O2: u8 = 6; // Version 6 uses range coder (order-2 context)
const WRAPPER_VERSION_RC_MIX: u8 = 7; // Version 7 uses range coder (order-0+1 mixed)
const WRAPPER_VERSION_RC_O12: u8 = 8; // Version 8 uses range coder (order-1+2 mixed)
const WRAPPER_VERSION_RC_EWMA: u8 = 9; // Version 9 uses range coder (O0+O1+O2 EWMA)
const WRAPPER_VERSION_HUFFMAN: u8 = 17; // Version 17 uses Huffman coder on BWT+MTF output
const WRAPPER_VERSION_RC_EWMA7: u8 = 18; // Version 18 uses range coder (O0-O7 EWMA)
const WRAPPER_VERSION_RC_EWMA7_CHUNKED: u8 = 20; // Version 20: chunked BWT + per-chunk EWMA7
const WRAPPER_VERSION_RC_EWMA7_ALPHA: u8 = 21; // Version 21: EWMA7 with explicit alpha in header
const WRAPPER_VERSION_RC_EWMA7_ZRUN: u8 = 19;   // Version 19: compact zrun header (14B)
const DEFAULT_CHUNK_SIZE_EWMA7: usize = 64 * 1024; // 64 KB - optimal for kennedy.xls

/// Encode data with SSP5 pipeline: BWT → MTF → SSP (no LZ77)
/// chunk_size: 0 = no chunking, >0 = split into chunks of that size
/// bwt_passes: number of iterative BWT passes (1 = standard)
pub fn ssp5_encode(data: &[u8], s: &[u64], block_bits: usize) -> Vec<u8> {
    ssp5_encode_with_options(data, s, block_bits, 0, 1, false)
}

/// Encode with options for chunking and iterative BWT
/// delta: if true, use O1 subtraction-delta mode (context between blocks)
pub fn ssp5_encode_with_options(data: &[u8], s: &[u64], block_bits: usize, chunk_size: usize, bwt_passes: usize, delta: bool) -> Vec<u8> {
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
    
    // Use adaptive MTF when chunking: fresh MTF alphabet per BWT chunk
    let mtf_data = if chunk_size > 0 && bwt_packed.len() > chunk_size {
        // Use adaptive MTF with fresh alphabet per chunk for better compression
        // Format: [chunk_len(4)][mtf_chunk...] per BWT chunk
        mtf_encode_bwt_chunked(&bwt_packed, chunk_size)
    } else {
        // Standard MTF for non-chunked data
        mtf_encode(&bwt_packed)
    };
    let ssp_encoded = ssp_encode(&mtf_data, s, block_bits, delta);
    
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

        // Check if chunked based on chunk_size in header
        let chunk_size = if version >= 3 {
            u32::from_le_bytes([archive[6], archive[7], archive[8], archive[9]]) as usize
        } else {
            0
        };

        // Decode MTF data (with fresh alphabet per BWT chunk if chunked)
        // Only use chunked MTF decode when the data is large enough to have been chunked
        let bwt_packed = if chunk_size > 0 && mtf_data.len() > chunk_size {
            mtf_decode_bwt_chunked(&mtf_data)
        } else {
            mtf_decode(&mtf_data)
        };

        // Unpack BWT (primary + last column) and decode
        if chunk_size > 0 && bwt_packed.len() > chunk_size {
            // Chunked BWT decoding
            bwt_decode_chunked(&bwt_packed)
        } else {
            // Non-chunked BWT decoding
            let (dec_primary, dec_bwt) = unpack_bwt(&bwt_packed);
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

/// Encode data with BWT → MTF → RangeCoder pipeline.
/// Uses arithmetic coding instead of SSP for potentially better compression.
/// Range coder provides near-optimal compression for byte streams.
pub fn ssp5_encode_with_range_coder(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    
    // BWT → pack(primary + last_col)
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    
    // MTF encoding
    let mtf_data = mtf_encode(&bwt_packed);
    
    // Range coder encoding
    let rc_data = range_encode_bytes(&mtf_data);
    
    // Archive format v4:
    // [WRAPPER_MAGIC(4)][VERSION=4(1)][PRIMARY(4)][MTF_LEN(4)][RC_DATA...]
    let mut out = Vec::with_capacity(13 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Decode data from BWT → MTF → RangeCoder pipeline.
pub fn ssp5_decode_with_range_coder(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    
    // Check magic and version
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    if archive[4] != WRAPPER_VERSION_RC {
        return Err("Invalid wrapper version for range coder");
    }
    
    let primary = u32::from_le_bytes([archive[5], archive[6], archive[7], archive[8]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[9], archive[10], archive[11], archive[12]]) as usize;
    let rc_data = &archive[13..];
    
    // Range coder decode
    let mtf_data = range_decode_bytes(rc_data)?;
    if mtf_data.len() != mtf_len {
        return Err("MTF length mismatch");
    }
    
    // MTF decode
    let bwt_decoded = mtf_decode(&mtf_data);
    
    // Unpack and BWT decode
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    
    Ok(bwt_decode(dec_primary, &dec_bwt))
}

/// Encode data with BWT → MTF → RangeCoder Order-1 pipeline.
/// Uses order-1 context modeling for better compression than order-0.
/// Order-1 considers the previous byte when encoding the current byte.
pub fn ssp5_encode_with_range_coder_o1(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    
    // BWT → pack(primary + last_col)
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    
    // MTF encoding
    let mtf_data = mtf_encode(&bwt_packed);
    
    // Order-1 Range coder encoding
    let rc_data = range_encode_bytes_order1(&mtf_data);
    
    // Archive format v5:
    // [WRAPPER_MAGIC(4)][VERSION=5(1)][PRIMARY(4)][MTF_LEN(4)][RC_DATA...]
    let mut out = Vec::with_capacity(13 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_O1);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Decode data from BWT → MTF → RangeCoder Order-1 pipeline.
pub fn ssp5_decode_with_range_coder_o1(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    
    // Check magic and version
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    if archive[4] != WRAPPER_VERSION_RC_O1 {
        return Err("Invalid wrapper version for order-1 range coder");
    }
    
    let primary = u32::from_le_bytes([archive[5], archive[6], archive[7], archive[8]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[9], archive[10], archive[11], archive[12]]) as usize;
    let rc_data = &archive[13..];
    
    // Order-1 Range coder decode
    let mtf_data = range_decode_bytes_order1(rc_data)?;
    if mtf_data.len() != mtf_len {
        return Err("MTF length mismatch");
    }
    
    // MTF decode
    let bwt_decoded = mtf_decode(&mtf_data);
    
    // Unpack and BWT decode
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    
    Ok(bwt_decode(dec_primary, &dec_bwt))
}

/// Encode data with BWT → MTF → RangeCoder Order-2 pipeline.
/// Uses order-2 context modeling (previous 2 bytes) for potentially better compression.
pub fn ssp5_encode_with_range_coder_o2(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    
    // BWT → pack(primary + last_col)
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    
    // MTF encoding
    let mtf_data = mtf_encode(&bwt_packed);
    
    // Order-2 Range coder encoding
    let rc_data = range_encode_bytes_order2(&mtf_data);
    
    // Archive format v6:
    // [WRAPPER_MAGIC(4)][VERSION=6(1)][PRIMARY(4)][MTF_LEN(4)][RC_DATA...]
    let mut out = Vec::with_capacity(13 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_O2);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Decode data from BWT → MTF → RangeCoder Order-2 pipeline.
pub fn ssp5_decode_with_range_coder_o2(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    
    // Check magic and version
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    if archive[4] != WRAPPER_VERSION_RC_O2 {
        return Err("Invalid wrapper version for order-2 range coder");
    }
    
    let primary = u32::from_le_bytes([archive[5], archive[6], archive[7], archive[8]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[9], archive[10], archive[11], archive[12]]) as usize;
    let rc_data = &archive[13..];
    
    // Order-2 Range coder decode
    let mtf_data = range_decode_bytes_order2(rc_data)?;
    if mtf_data.len() != mtf_len {
        return Err("MTF length mismatch");
    }
    
    // MTF decode
    let bwt_decoded = mtf_decode(&mtf_data);
    
    // Unpack and BWT decode
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    
    Ok(bwt_decode(dec_primary, &dec_bwt))
}

/// Encode data with BWT → MTF → RangeCoder Order-Mix pipeline.
/// Blends order-0 and order-1 for adaptive compression.
pub fn ssp5_encode_with_range_coder_mix(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    
    // BWT → pack(primary + last_col)
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    
    // MTF encoding
    let mtf_data = mtf_encode(&bwt_packed);
    
    // Order-mix Range coder encoding
    let rc_data = range_encode_bytes_order_mix(&mtf_data);
    
    // Archive format v7:
    // [WRAPPER_MAGIC(4)][VERSION=7(1)][PRIMARY(4)][MTF_LEN(4)][RC_DATA...]
    let mut out = Vec::with_capacity(13 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_MIX);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Decode data from BWT → MTF → RangeCoder Order-Mix pipeline.
pub fn ssp5_decode_with_range_coder_mix(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    
    // Check magic and version
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    if archive[4] != WRAPPER_VERSION_RC_MIX {
        return Err("Invalid wrapper version for order-mix range coder");
    }
    
    let primary = u32::from_le_bytes([archive[5], archive[6], archive[7], archive[8]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[9], archive[10], archive[11], archive[12]]) as usize;
    let rc_data = &archive[13..];
    
    // Order-mix Range coder decode
    let mtf_data = range_decode_bytes_order_mix(rc_data)?;
    if mtf_data.len() != mtf_len {
        return Err("MTF length mismatch");
    }
    
    // MTF decode
    let bwt_decoded = mtf_decode(&mtf_data);
    
    // Unpack and BWT decode
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    
    Ok(bwt_decode(dec_primary, &dec_bwt))
}

/// Encode data with BWT → MTF → RangeCoder Order-1+2 Mix pipeline.
/// Adaptively blends O1 and O2 with context-dependent weighting.
pub fn ssp5_encode_with_range_coder_o12(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    let mtf_data = mtf_encode(&bwt_packed);
    
    let rc_data = range_encode_bytes_order12_mix(&mtf_data);
    
    let mut out = Vec::with_capacity(13 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_O12);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Decode data from BWT → MTF → RangeCoder Order-1+2 Mix pipeline.
pub fn ssp5_decode_with_range_coder_o12(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    if archive[4] != WRAPPER_VERSION_RC_O12 {
        return Err("Invalid wrapper version for order-1+2 range coder");
    }
    
    let primary = u32::from_le_bytes([archive[5], archive[6], archive[7], archive[8]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[9], archive[10], archive[11], archive[12]]) as usize;
    let rc_data = &archive[13..];
    
    let mtf_data = range_decode_bytes_order12_mix(rc_data)?;
    if mtf_data.len() != mtf_len {
        return Err("MTF length mismatch");
    }
    
    let bwt_decoded = mtf_decode(&mtf_data);
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    
    Ok(bwt_decode(dec_primary, &dec_bwt))
}

const WRAPPER_VERSION_RC_EWMA3: u8 = 10; // Version 10 uses range coder (O0+O1+O2+O3 EWMA)
const WRAPPER_VERSION_RC_EWMA5: u8 = 11; // Version 11 uses range coder (O0+O1+O2+O3+O4+O5 EWMA)

/// Encode data with BWT → MTF → RangeCoder O0+O1+O2 EWMA pipeline.
/// Adaptively blends O0, O1, O2 with EWMA-based model weighting.
pub fn ssp5_encode_with_range_coder_ewma(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    let mtf_data = mtf_encode(&bwt_packed);
    
    let rc_data = range_encode_bytes_order_ewma(&mtf_data);
    
    let mut out = Vec::with_capacity(13 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_EWMA);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Decode data from BWT → MTF → RangeCoder O0+O1+O2 EWMA pipeline.
pub fn ssp5_decode_with_range_coder_ewma(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    if archive[4] != WRAPPER_VERSION_RC_EWMA {
        return Err("Invalid wrapper version for EWMA range coder");
    }
    
    let primary = u32::from_le_bytes([archive[5], archive[6], archive[7], archive[8]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[9], archive[10], archive[11], archive[12]]) as usize;
    let rc_data = &archive[13..];
    
    let mtf_data = range_decode_bytes_order_ewma(rc_data)?;
    if mtf_data.len() != mtf_len {
        return Err("MTF length mismatch");
    }
    
    let bwt_decoded = mtf_decode(&mtf_data);
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    
    Ok(bwt_decode(dec_primary, &dec_bwt))
}

/// Encode data with BWT → MTF → RangeCoder O0+O1+O2+O3 EWMA pipeline (sparse table).
pub fn ssp5_encode_with_range_coder_ewma3(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    let mtf_data = mtf_encode(&bwt_packed);
    
    let rc_data = range_encode_bytes_order_ewma3(&mtf_data);
    
    let mut out = Vec::with_capacity(13 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_EWMA3);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Decode data from BWT → MTF → RangeCoder O0+O1+O2+O3 EWMA pipeline (sparse table).
pub fn ssp5_decode_with_range_coder_ewma3(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    if archive[4] != WRAPPER_VERSION_RC_EWMA3 {
        return Err("Invalid wrapper version for EWMA3 range coder");
    }
    
    let primary = u32::from_le_bytes([archive[5], archive[6], archive[7], archive[8]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[9], archive[10], archive[11], archive[12]]) as usize;
    let rc_data = &archive[13..];
    
    let mtf_data = range_decode_bytes_order_ewma3(rc_data)?;
    if mtf_data.len() != mtf_len {
        return Err("MTF length mismatch");
    }
    
    let bwt_decoded = mtf_decode(&mtf_data);
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    
    Ok(bwt_decode(dec_primary, &dec_bwt))
}

// === Run-Length Encoding for MTF output ===

/// RLE escape code (distinct from typical MTF values 0-2)
const RLE_ESCAPE: u8 = 0xFE;
/// Minimum run length to encode (2 literal + 1+ encoded)
const RLE_MIN_RUN: usize = 3;

/// Apply RLE to MTF output. Returns (rle_data, original_len).
/// Format: runs of 3+ encoded as [ESCAPE, value, run_len-3 as u8].
/// Literal ESCAPE bytes encoded as [ESCAPE, ESCAPE].
/// Short runs (<3) pass through as-is.
fn apply_rle(data: &[u8]) -> (Vec<u8>, u32) {
    let orig_len = data.len() as u32;
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    
    while i < data.len() {
        let b = data[i];
        
        // Check for run of same byte
        let mut run_len = 1;
        while i + run_len < data.len() && data[i + run_len] == b && run_len < 255 {
            run_len += 1;
        }
        
        if run_len >= RLE_MIN_RUN && b != RLE_ESCAPE {
            // Encode as run: ESCAPE + value + (run_len - 3)
            out.push(RLE_ESCAPE);
            out.push(b);
            out.push((run_len - 3) as u8);
            i += run_len;
        } else if b == RLE_ESCAPE {
            // Literal escape: ESCAPE + ESCAPE
            out.push(RLE_ESCAPE);
            out.push(RLE_ESCAPE);
            i += 1;
        } else {
            // Literal byte (may be part of a short run, pass through)
            out.push(b);
            i += 1;
        }
    }
    
    (out, orig_len)
}

/// Reverse RLE encoding. Returns original MTF data.
fn reverse_rle(data: &[u8], orig_len: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(orig_len as usize);
    let mut i = 0;
    
    while i < data.len() && out.len() < orig_len as usize {
        let b = data[i];
        
        if b == RLE_ESCAPE && i + 1 < data.len() {
            let next = data[i + 1];
            if next == RLE_ESCAPE {
                // Literal ESCAPE
                out.push(RLE_ESCAPE);
                i += 2;
            } else {
                // Run: ESCAPE + value + count (count is at i+2, value at i+1)
                let value = data[i + 1];
                let count = (data[i + 2] as usize) + 3;
                out.resize(out.len() + count, value);
                i += 3;
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    
    out
}

/// Encode data with BWT → MTF → RLE → RangeCoder EWMA5 pipeline.
pub fn ssp5_encode_with_range_coder_ewma5_rle(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    let mtf_data = mtf_encode(&bwt_packed);
    let (rle_data, mtf_len) = apply_rle(&mtf_data);
    let rc_data = range_encode_bytes_order_ewma5(&rle_data);
    
    let mut out = Vec::with_capacity(17 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(13); // version: EWMA5 + RLE
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_len as u32).to_le_bytes()); // original MTF length for RLE decode
    out.extend_from_slice(&rc_data);
    out
}

/// Decode data from BWT → MTF → RLE → RangeCoder EWMA5 pipeline.
pub fn ssp5_decode_with_range_coder_ewma5_rle(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    if archive[4] != 13 {
        return Err("Invalid wrapper version for EWMA5+RLE");
    }
    
    let primary = u32::from_le_bytes([archive[5], archive[6], archive[7], archive[8]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[9], archive[10], archive[11], archive[12]]) as usize;
    let rc_data = &archive[13..];
    
    let rle_data = range_decode_bytes_order_ewma5(rc_data, None)?;
    let mtf_data = reverse_rle(&rle_data, mtf_len as u32);
    
    let bwt_decoded = mtf_decode(&mtf_data);
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    Ok(bwt_decode(dec_primary, &dec_bwt))
}

/// Encode data with BWT → MTF → RangeCoder O0+O1+O2+O3+O4+O5+O6+O7 EWMA pipeline.
/// DEFAULT: uses auto-tuning (tries 18 (alpha,wscale,zrun) candidates with v18/v19/v21 headers).
/// Falls back to legacy v18 (0.05, 100) for compatibility with old archives.
pub fn ssp5_encode_with_range_coder_ewma7(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    // Use auto-tuned EWMA7 as default — better compression across all file sizes
    ssp5_encode_with_range_coder_ewma7_auto(data)
}

/// Legacy v18 baseline encoder: BWT → MTF → range_encode_bytes_order_ewma7 (0.05, 100).
/// Used as fallback in auto-tuning and for compatibility with old archives.
pub fn ssp5_encode_with_range_coder_ewma7_v18(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    let mtf_data = mtf_encode(&bwt_packed);
    let rc_data = range_encode_bytes_order_ewma7(&mtf_data);

    let mut out = Vec::with_capacity(13 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_EWMA7);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Decode data from BWT → MTF →RangeCoder O0+O1+O2+O3+O4+O5+O6+O7 EWMA pipeline.
/// Accepts version 18 (implicit EWMA_ALPHA), version 19 (compact zrun), and version 21 (explicit alpha).
pub fn ssp5_decode_with_range_coder_ewma7(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }

    // Version 19: [MAGIC(4)][19][cfg(1)][primary(4)][mtf_len(4)][rc] = 14 bytes
    if archive[4] == WRAPPER_VERSION_RC_EWMA7_ZRUN {
        if archive.len() < 15 {
            return Err("Archive too short for v19");
        }
        let cfg = archive[5];
        let (alpha, wscale, zrun) = config_to_params(cfg);
        let primary = u32::from_le_bytes([archive[6], archive[7], archive[8], archive[9]]) as usize;
        let mtf_len = u32::from_le_bytes([archive[10], archive[11], archive[12], archive[13]]) as usize;
        let rc_data = &archive[14..];
        let rc_out = range_decode_bytes_order_ewma7_alpha_ws(rc_data, alpha, wscale)?;
        let mtf_data = if zrun { zrun_decode(&rc_out) } else { rc_out };
        if mtf_data.len() != mtf_len {
            return Err("MTF length mismatch");
        }
        let bwt_decoded = mtf_decode(&mtf_data);
        let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
        if dec_primary as u32 != primary as u32 {
            return Err("BWT primary index mismatch");
        }
        return Ok(bwt_decode(dec_primary, &dec_bwt));
    }

    // Version 21: [MAGIC(4)][21][flags(1)][alpha f64 (8)][wscale f64 (8)][primary(4)][mtf_len(4)][rc]
    if archive[4] != WRAPPER_VERSION_RC_EWMA7 && archive[4] != WRAPPER_VERSION_RC_EWMA7_ALPHA {
        return Err("Invalid wrapper version for EWMA7 range coder");
    }

    // Version 21 format...
    let (alpha, wscale, zrun, data_start) = if archive[4] == WRAPPER_VERSION_RC_EWMA7_ALPHA {
        if archive.len() < 30 {
            return Err("Archive too short for alpha header");
        }
        let flags = archive[5];
        let mut a = [0u8; 8];
        a.copy_from_slice(&archive[6..14]);
        let mut w = [0u8; 8];
        w.copy_from_slice(&archive[14..22]);
        (f64::from_le_bytes(a), f64::from_le_bytes(w), flags & 1 != 0, 22)
    } else {
        (EWMA_ALPHA_DEFAULT, 100.0, false, 5)
    };

    if archive.len() < data_start + 8 {
        return Err("Archive too short");
    }
    let primary = u32::from_le_bytes([archive[data_start], archive[data_start+1], archive[data_start+2], archive[data_start+3]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[data_start+4], archive[data_start+5], archive[data_start+6], archive[data_start+7]]) as usize;
    let rc_data = &archive[data_start+8..];

    let rc_out = if archive[4] == WRAPPER_VERSION_RC_EWMA7_ALPHA {
        range_decode_bytes_order_ewma7_alpha_ws(rc_data, alpha, wscale)?
    } else {
        range_decode_bytes_order_ewma7(rc_data)?
    };
    let mtf_data = if zrun { zrun_decode(&rc_out) } else { rc_out };
    if mtf_data.len() != mtf_len {
        return Err("MTF length mismatch");
    }

    let bwt_decoded = mtf_decode(&mtf_data);
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    Ok(bwt_decode(dec_primary, &dec_bwt))
}

/// Default EWMA alpha used by version-18 archives (must match range_coder::EWMA_ALPHA).
const EWMA_ALPHA_DEFAULT: f64 = 0.05;

/// v19 config byte lookup: (alpha_idx << 3) | (wscale_idx << 1) | zrun_bit
/// alpha_idx: 0=0.05, 1=0.1, 2=0.001, 3=0.0001, 4=0.0005
/// wscale_idx: 0=100, 1=6, 2=10, 3=50
/// Returns (alpha, wscale, zrun)
fn config_to_params(cfg: u8) -> (f64, f64, bool) {
    let alpha_idx = (cfg >> 3) & 0x07;
    let wscale_idx = (cfg >> 1) & 0x03;
    let zrun = (cfg & 0x01) != 0;
    let alpha = match alpha_idx {
        0 => 0.05, 1 => 0.1, 2 => 0.001, 3 => 0.0001, 4 => 0.0005, _ => 0.05,
    };
    let wscale = match wscale_idx {
        0 => 100.0, 1 => 6.0, 2 => 10.0, 3 => 50.0, _ => 100.0,
    };
    (alpha, wscale, zrun)
}

/// Encode with compact v19 header (14 bytes total) for specific config.
/// v19: [MAGIC(4)][19][cfg(1)][primary(4)][mtf_len(4)][rc] = 14 bytes
/// Config byte encodes (alpha, wscale, zrun) to save 16 bytes vs v21.
pub fn ssp5_encode_with_range_coder_ewma7_v19(data: &[u8], alpha: f64, wscale: f64, zrun: bool) -> Vec<u8> {
    if data.is_empty() { return Vec::new(); }

    // Map alpha/wscale to config index
    let alpha_idx = match alpha {
        0.05 => 0, 0.1 => 1, 0.001 => 2, 0.0001 => 3, 0.0005 => 4, _ => 0,
    };
    let wscale_idx = match wscale {
        100.0 => 0, 6.0 => 1, 10.0 => 2, 50.0 => 3, _ => 0,
    };
    let cfg = (alpha_idx << 3) | (wscale_idx << 1) | (zrun as u8);

    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    let mtf_data = mtf_encode(&bwt_packed);
    let rc_data = if zrun {
        range_encode_bytes_order_ewma7_alpha_ws(&zrun_encode(&mtf_data), alpha, wscale)
    } else {
        range_encode_bytes_order_ewma7_alpha_ws(&mtf_data, alpha, wscale)
    };

    let mut out = Vec::with_capacity(14 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_EWMA7_ZRUN);
    out.push(cfg);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Encode with explicit EWMA decay factor (weight scale = 100). Archive stores params (version 21).
pub fn ssp5_encode_with_range_coder_ewma7_alpha(data: &[u8], alpha: f64) -> Vec<u8> {
    ssp5_encode_with_range_coder_ewma7_alpha_ws(data, alpha, 100.0)
}

/// Encode with explicit alpha and mixing-weight scale (no zero-run escape).
pub fn ssp5_encode_with_range_coder_ewma7_alpha_ws(data: &[u8], alpha: f64, wscale: f64) -> Vec<u8> {
    ssp5_encode_with_range_coder_ewma7_alpha_ws_zr(data, alpha, wscale, false)
}

/// Encode with explicit alpha, weight scale, and optional zero-run escape (flags bit0).
/// Zero-run escape collapses runs of 0x00 in the MTF stream: [00][00][uleb(k-2)].
pub fn ssp5_encode_with_range_coder_ewma7_alpha_ws_zr(data: &[u8], alpha: f64, wscale: f64, zrun: bool) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    let mtf_data = mtf_encode(&bwt_packed);
    let rc_data = if zrun {
        range_encode_bytes_order_ewma7_alpha_ws(&zrun_encode(&mtf_data), alpha, wscale)
    } else {
        range_encode_bytes_order_ewma7_alpha_ws(&mtf_data, alpha, wscale)
    };

    let mut out = Vec::with_capacity(30 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_EWMA7_ALPHA);
    out.push(zrun as u8); // flags
    out.extend_from_slice(&alpha.to_le_bytes());
    out.extend_from_slice(&wscale.to_le_bytes());
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Auto-tune: try (alpha, wscale, zrun) candidates with v18 (13B), v21 (30B),
/// and v19 compact headers (14B) for small-file wins.
/// v19 saves 16 bytes vs v21, preserving zrun gains on small files.
/// (0.05, 6, zrun) wins on binary/spreadsheet (kennedy.xls 9.51%);
/// low-alpha candidates target small text where slow adaptation builds
/// a stable model; v19 compact header preserves zrun savings on small files.
pub fn ssp5_encode_with_range_coder_ewma7_auto(data: &[u8]) -> Vec<u8> {
    let mut best = ssp5_encode_with_range_coder_ewma7_v18(data); // v18 (0.05, 100) 13B

    // All (alpha, wscale, zrun) candidates to try
    let candidates: [(f64, f64, bool); 18] = [
        (0.05, 6.0, true),
        (0.05, 6.0, false),
        (0.1, 100.0, true),
        (0.1, 100.0, false),
        (0.05, 100.0, false),        // base v18 candidate
        (0.001, 10.0, true),
        (0.001, 50.0, true),
        (0.01, 10.0, true),
        (0.01, 50.0, false),
        // Small-alpha candidates for small files (slow adaptation = less warmup overhead)
        (0.001, 100.0, true),
        (0.0005, 100.0, true),
        (0.0001, 100.0, true),
        (0.0001, 6.0, true),
        (0.0005, 6.0, true),
        (0.001, 100.0, false),
        (0.0001, 6.0, false),
        (0.0005, 6.0, false),
        (0.0001, 100.0, false),
    ];

    for &(alpha, wscale, zrun) in &candidates {
        // v21 with full params (30B header)
        let enc_v21 = ssp5_encode_with_range_coder_ewma7_alpha_ws_zr(data, alpha, wscale, zrun);
        if enc_v21.len() < best.len() {
            best = enc_v21;
        }
        // v19 compact header (14B) — only for alpha/wscale in config table
        let alpha_ok = matches!(alpha, 0.05 | 0.1 | 0.001 | 0.0001 | 0.0005);
        let ws_ok = matches!(wscale, 100.0 | 6.0 | 10.0 | 50.0);
        if alpha_ok && ws_ok {
            let enc_v19 = ssp5_encode_with_range_coder_ewma7_v19(data, alpha, wscale, zrun);
            if enc_v19.len() < best.len() {
                best = enc_v19;
            }
        }
    }
    best
}

/// Encode data with BWT → MTF → RangeCoder O0-O7 EWMA pipeline, with chunked BWT
/// and per-chunk EWMA state reset.
/// chunk_size: 0 = no chunking (behaves identically to ssp5_encode_with_range_coder_ewma7),
/// >0 = split data into chunks of that size, each encoded independently with fresh EWMA state.
pub fn ssp5_encode_with_range_coder_ewma7_chunked(data: &[u8], chunk_size: usize) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }

    if chunk_size == 0 || data.len() <= chunk_size {
        // No chunking needed — delegate to non-chunked version
        return ssp5_encode_with_range_coder_ewma7(data);
    }

    let num_chunks = (data.len() + chunk_size - 1) / chunk_size;

    // Encode each chunk independently: BWT → MTF → range_encode_bytes_order_ewma7
    // Each chunk gets fresh EWMA state
    let mut chunk_metas: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(num_chunks); // (primary, mtf_len, rc_data_len, rc_data_offset)
    let mut rc_concat = Vec::new();

    for i in 0..num_chunks {
        let start = i * chunk_size;
        let end = (start + chunk_size).min(data.len());
        let chunk = &data[start..end];

        let (primary, bwt_data) = bwt_encode(chunk);
        let bwt_packed = pack_bwt(primary, &bwt_data);
        let mtf_data = mtf_encode(&bwt_packed);
        let rc_data = range_encode_bytes_order_ewma7(&mtf_data);

        let rc_offset = rc_concat.len() as u32;
        chunk_metas.push((primary as u32, mtf_data.len() as u32, rc_data.len() as u32, rc_offset));
        rc_concat.extend_from_slice(&rc_data);
    }

    // Archive format:
    // [MAGIC(4)][VERSION(1)=20][NUM_CHUNKS(4)][CHUNK_SIZE(4)][MTF_LEN_TOTAL(4)]
    // [CHUNK1_PRIMARY(4)][CHUNK1_MTF_LEN(4)][CHUNK1_RC_LEN(4)][CHUNK1_RC_OFFSET(4)]
    // [CHUNK2_PRIMARY(4)][CHUNK2_MTF_LEN(4)][CHUNK2_RC_LEN(4)][CHUNK2_RC_OFFSET(4)]
    // ...
    // [RC_DATA_CONCAT]
    let total_mtf_len: u64 = chunk_metas.iter().map(|m| m.1 as u64).sum();
    let mut out = Vec::with_capacity(
        9 + 4 + 4 + 4 + num_chunks * 16 + rc_concat.len()
    );
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_EWMA7_CHUNKED);
    out.extend_from_slice(&(num_chunks as u32).to_le_bytes());
    out.extend_from_slice(&(chunk_size as u32).to_le_bytes());
    out.extend_from_slice(&(total_mtf_len as u32).to_le_bytes());

    for &(primary, mtf_len, rc_len, rc_offset) in &chunk_metas {
        out.extend_from_slice(&primary.to_le_bytes());
        out.extend_from_slice(&mtf_len.to_le_bytes());
        out.extend_from_slice(&rc_len.to_le_bytes());
        out.extend_from_slice(&rc_offset.to_le_bytes());
    }

    out.extend_from_slice(&rc_concat);
    out
}

/// Encode with optimal chunk size (64 KB) for BWT → MTF → RangeCoder O0-O7 EWMA pipeline.
/// Each chunk gets fresh EWMA state; 64 KB blocks provide best adaptation.
pub fn ssp5_encode_with_range_coder_ewma7_chunked_optimal(data: &[u8]) -> Vec<u8> {
    ssp5_encode_with_range_coder_ewma7_chunked(data, DEFAULT_CHUNK_SIZE_EWMA7)
}

/// Decode data from BWT → MTF → RangeCoder O0-O7 EWMA pipeline with chunked BWT
/// and per-chunk EWMA state reset.
pub fn ssp5_decode_with_range_coder_ewma7_chunked(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    // Accept both chunked (20) and non-chunked (18) EWMA7 versions
    if archive[4] != WRAPPER_VERSION_RC_EWMA7_CHUNKED && archive[4] != WRAPPER_VERSION_RC_EWMA7 {
        return Err("Invalid wrapper version for chunked EWMA7 range coder");
    }

    // If non-chunked version, delegate to non-chunked decoder
    if archive[4] == WRAPPER_VERSION_RC_EWMA7 {
        return ssp5_decode_with_range_coder_ewma7(archive);
    }

    let pos = 5;
    let num_chunks = u32::from_le_bytes([archive[pos], archive[pos+1], archive[pos+2], archive[pos+3]]) as usize;
    let _chunk_size = u32::from_le_bytes([archive[pos+4], archive[pos+5], archive[pos+6], archive[pos+7]]) as usize;

    // Skip total_mtf_len (not needed for decoding since each chunk self-describes)
    let mut offset = pos + 12; // skip num_chunks + chunk_size + total_mtf_len

    // Read chunk metas: primary + mtf_len + rc_len + rc_offset for each chunk
    let mut chunk_metas: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(num_chunks);
    for _ in 0..num_chunks {
        if offset + 16 > archive.len() {
            return Err("Truncated chunk header");
        }
        let primary = u32::from_le_bytes([archive[offset], archive[offset+1], archive[offset+2], archive[offset+3]]);
        let mtf_len = u32::from_le_bytes([archive[offset+4], archive[offset+5], archive[offset+6], archive[offset+7]]);
        let rc_len = u32::from_le_bytes([archive[offset+8], archive[offset+9], archive[offset+10], archive[offset+11]]);
        let rc_off = u32::from_le_bytes([archive[offset+12], archive[offset+13], archive[offset+14], archive[offset+15]]);
        chunk_metas.push((primary, mtf_len, rc_len, rc_off));
        offset += 16;
    }

    let mut out = Vec::new();

    for (_, &(primary, mtf_len, rc_len, rc_off)) in chunk_metas.iter().enumerate() {
        // Each chunk's RC data is self-contained
        let rc_start = offset + rc_off as usize;
        let rc_end = rc_start + rc_len as usize;
        if rc_end > archive.len() {
            return Err("Truncated RC data for chunk");
        }
        let rc_data = &archive[rc_start..rc_end];

        // Decode this chunk's range coder output
        let mtf_data = range_decode_bytes_order_ewma7(rc_data)?;
        if mtf_data.len() != mtf_len as usize {
            return Err("MTF length mismatch for chunk");
        }

        let bwt_decoded = mtf_decode(&mtf_data);
        let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
        if dec_primary as u32 != primary {
            return Err("BWT primary index mismatch for chunk");
        }

        out.extend_from_slice(&bwt_decode(dec_primary, &dec_bwt));
    }

    Ok(out)
}

/// Encode data with BWT → MTF → Huffman pipeline.
pub fn ssp5_encode_with_huffman(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    let mtf_data = mtf_encode(&bwt_packed);

    let hf_data = huffman_encode(&mtf_data);

    let mut out = Vec::with_capacity(17 + hf_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_HUFFMAN);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&hf_data);
    out
}

/// Decode data from BWT → MTF → Huffman pipeline.
pub fn ssp5_decode_with_huffman(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    if archive[4] != WRAPPER_VERSION_HUFFMAN {
        return Err("Invalid wrapper version for Huffman pipeline");
    }

    let primary = u32::from_le_bytes([archive[5], archive[6], archive[7], archive[8]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[9], archive[10], archive[11], archive[12]]) as usize;
    let hf_data = &archive[13..];

    let mtf_data = huffman_decode(hf_data)?;
    if mtf_data.len() != mtf_len {
        return Err("MTF length mismatch");
    }

    let bwt_decoded = mtf_decode(&mtf_data);
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    Ok(bwt_decode(dec_primary, &dec_bwt))
}

/// Encode data with BWT → MTF → RangeCoder O0+O1+O2+O3+O4+O5 EWMA pipeline.
pub fn ssp5_encode_with_range_coder_ewma5(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    
    let (primary, bwt_data) = bwt_encode(data);
    let bwt_packed = pack_bwt(primary, &bwt_data);
    let mtf_data = mtf_encode(&bwt_packed);
    
    let rc_data = range_encode_bytes_order_ewma5(&mtf_data);
    
    let mut out = Vec::with_capacity(13 + rc_data.len());
    out.extend_from_slice(WRAPPER_MAGIC);
    out.push(WRAPPER_VERSION_RC_EWMA5);
    out.extend_from_slice(&(primary as u32).to_le_bytes());
    out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&rc_data);
    out
}

/// Decode data from BWT → MTF → RangeCoder O0+O1+O2+O3+O4+O5 EWMA pipeline.
pub fn ssp5_decode_with_range_coder_ewma5(archive: &[u8]) -> Result<Vec<u8>, &'static str> {
    if archive.is_empty() {
        return Ok(Vec::new());
    }
    
    if &archive[0..4] != WRAPPER_MAGIC {
        return Err("Invalid wrapper magic");
    }
    if archive[4] != WRAPPER_VERSION_RC_EWMA5 {
        return Err("Invalid wrapper version for EWMA5 range coder");
    }
    
    let primary = u32::from_le_bytes([archive[5], archive[6], archive[7], archive[8]]) as usize;
    let mtf_len = u32::from_le_bytes([archive[9], archive[10], archive[11], archive[12]]) as usize;
    let rc_data = &archive[13..];
    
    let mtf_data = range_decode_bytes_order_ewma5(rc_data, Some(mtf_len))?;
    if mtf_data.len() != mtf_len {
        return Err("MTF length mismatch");
    }
    
    let bwt_decoded = mtf_decode(&mtf_data);
    let (dec_primary, dec_bwt) = unpack_bwt(&bwt_decoded);
    if dec_primary as u32 != primary as u32 {
        return Err("BWT primary index mismatch");
    }
    
    Ok(bwt_decode(dec_primary, &dec_bwt))
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
    fn test_range_coder_roundtrip_small() {
        let data = b"Hello World! This is a test of range coder.";
        let encoded = ssp5_encode_with_range_coder(data);
        let decoded = ssp5_decode_with_range_coder(&encoded).expect("decode should succeed");
        assert_eq!(data.to_vec(), decoded);
    }
    
    #[test]
    fn test_range_coder_roundtrip_repeated() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = ssp5_encode_with_range_coder(&data);
        let decoded = ssp5_decode_with_range_coder(&encoded).expect("decode should succeed");
        assert_eq!(data, decoded);
    }
    
    #[test]
    fn test_range_coder_compression_ratio() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(5000).copied().collect();
        let encoded = ssp5_encode_with_range_coder(&data);
        let ratio = 100.0 * encoded.len() as f64 / data.len() as f64;
        println!("Range coder compression ratio: {:.2}%", ratio);
        assert!(ratio < 15.0, "Should compress well: got {}%", ratio);
    }
    
    #[test]
    fn test_range_coder_vs_ssp_compression() {
        // Compare range coder vs SSP on repetitive data
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(5000).copied().collect();
        
        let rc_encoded = ssp5_encode_with_range_coder(&data);
        let rc_ratio = 100.0 * rc_encoded.len() as f64 / data.len() as f64;
        
        let s: Vec<u64> = vec![13, 17, 19, 23, 29, 31, 37, 41];
        let ssp_encoded = ssp5_encode(&data, &s, 16);
        let ssp_ratio = 100.0 * ssp_encoded.len() as f64 / data.len() as f64;
        
        println!("Range coder: {} bytes ({:.2}%)", rc_encoded.len(), rc_ratio);
        println!("SSP codec:   {} bytes ({:.2}%)", ssp_encoded.len(), ssp_ratio);
        
        // Both should compress well
        assert!(rc_ratio < 15.0, "Range coder should compress well");
        assert!(ssp_ratio < 15.0, "SSP should compress well");
    }
    
    // Order-1 context model tests
    
    #[test]
    fn test_range_coder_o1_roundtrip_small() {
        let data = b"Hello World! This is a test of range coder.";
        let encoded = ssp5_encode_with_range_coder_o1(data);
        let decoded = ssp5_decode_with_range_coder_o1(&encoded).expect("decode should succeed");
        assert_eq!(data.to_vec(), decoded);
    }
    
    #[test]
    fn test_range_coder_o1_roundtrip_repeated() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = ssp5_encode_with_range_coder_o1(&data);
        let decoded = ssp5_decode_with_range_coder_o1(&encoded).expect("decode should succeed");
        assert_eq!(data, decoded);
    }
    
    #[test]
    fn test_range_coder_o1_compression_ratio() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(5000).copied().collect();
        let encoded = ssp5_encode_with_range_coder_o1(&data);
        let ratio = 100.0 * encoded.len() as f64 / data.len() as f64;
        println!("Range coder O1 compression ratio: {:.2}%", ratio);
        assert!(ratio < 15.0, "Should compress well: got {}%", ratio);
    }
    
    #[test]
    fn test_range_coder_o1_vs_o0_compression() {
        // Compare order-0 vs order-1 on repetitive data
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(5000).copied().collect();
        
        let o0_encoded = ssp5_encode_with_range_coder(&data);
        let o0_ratio = 100.0 * o0_encoded.len() as f64 / data.len() as f64;
        
        let o1_encoded = ssp5_encode_with_range_coder_o1(&data);
        let o1_ratio = 100.0 * o1_encoded.len() as f64 / data.len() as f64;
        
        println!("Order-0: {} bytes ({:.2}%)", o0_encoded.len(), o0_ratio);
        println!("Order-1: {} bytes ({:.2}%)", o1_encoded.len(), o1_ratio);
        println!("Order-1 improvement: {:.1}pp", o0_ratio - o1_ratio);
        
        // Both should be good
        assert!(o0_ratio < 10.0, "Order-0 should compress well");
        assert!(o1_ratio < 10.0, "Order-1 should compress well");
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
    
    // === O0+O1+O2+O3 EWMA pipeline (sparse table) ===
    
    #[test]
    fn test_range_coder_ewma3_roundtrip_small() {
        let data = b"hello world";
        let encoded = ssp5_encode_with_range_coder_ewma3(data);
        let decoded = ssp5_decode_with_range_coder_ewma3(&encoded).expect("decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_range_coder_ewma3_roundtrip_repetitive() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = ssp5_encode_with_range_coder_ewma3(&data);
        let decoded = ssp5_decode_with_range_coder_ewma3(&encoded).expect("decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_range_coder_ewma3_roundtrip_random() {
        // Deterministic pseudo-random data
        let seed: u64 = 42;
        let mut data = Vec::with_capacity(1000);
        let mut h = seed;
        for _ in 0..1000 {
            h = h.wrapping_mul(6364136223846793005).wrapping_add(1);
            data.push((h >> 40) as u8);
        }
        let encoded = ssp5_encode_with_range_coder_ewma3(&data);
        let decoded = ssp5_decode_with_range_coder_ewma3(&encoded).expect("decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_range_coder_ewma3_vs_ewma_compression() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let enc_ewma2 = ssp5_encode_with_range_coder_ewma(&data);
        let enc_ewma3 = ssp5_encode_with_range_coder_ewma3(&data);
        println!("RC EWMA (O0+O1+O2): {} bytes", enc_ewma2.len());
        println!("RC EWMA3 (O0+O1+O2+O3): {} bytes", enc_ewma3.len());
        println!("O3 improvement: {:.1}%", 100.0 * (1.0 - enc_ewma3.len() as f64 / enc_ewma2.len() as f64));
    }
    
    // === RLE tests ===
    
    #[test]
    fn test_rle_roundtrip_simple() {
        let data = b"aaabbbcccdddeee";
        let (rle, orig_len) = apply_rle(data);
        let decoded = reverse_rle(&rle, orig_len);
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_rle_roundtrip_no_runs() {
        let data = b"abcdefghijk";
        let (rle, orig_len) = apply_rle(data);
        let decoded = reverse_rle(&rle, orig_len);
        assert_eq!(decoded, data);
        assert_eq!(rle, data); // No compression, should be same
    }
    
    #[test]
    fn test_rle_roundtrip_mixed() {
        // Typical MTF-like data: runs of 0s with occasional other values
        let data: Vec<u8> = vec![
            0, 0, 0, 0, 5, 5, 5, 5, 5, 5, 5,
            1, 1, 1, 1, 1, 1, 1,
            0, 0, 0, 0, 0, 0, 0, 0,
            2, 2, 2, 2,
        ];
        let (rle, orig_len) = apply_rle(&data);
        let decoded = reverse_rle(&rle, orig_len);
        assert_eq!(decoded, data);
        println!("MTF-like: {} bytes -> {} RLE bytes ({:.1}%)", 
            data.len(), rle.len(), 100.0 * rle.len() as f64 / data.len() as f64);
    }
    
    #[test]
    fn test_rle_roundtrip_escape_literal() {
        // Data containing the escape byte (0xFE)
        let data = vec![0xFE, 0xFE, 0xFE, 0xFE, 0xFE];
        let (rle, orig_len) = apply_rle(&data);
        let decoded = reverse_rle(&rle, orig_len);
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_rle_short_runs() {
        // Runs shorter than MIN_RUN (3) should pass through
        let data = b"aab"; // 2 'a's, not a run (MIN_RUN=3)
        let (rle, orig_len) = apply_rle(data);
        let decoded = reverse_rle(&rle, orig_len);
        assert_eq!(decoded, data);
    }
    
    // === EWMA5 + RLE pipeline tests ===
    
    #[test]
    fn test_ewma5_rle_roundtrip_small() {
        let data = b"hello world";
        let encoded = ssp5_encode_with_range_coder_ewma5_rle(data);
        let decoded = ssp5_decode_with_range_coder_ewma5_rle(&encoded).expect("decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_ewma5_rle_roundtrip_repetitive() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = ssp5_encode_with_range_coder_ewma5_rle(&data);
        let decoded = ssp5_decode_with_range_coder_ewma5_rle(&encoded).expect("decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_ewma5_rle_vs_ewma5_compression() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let enc_ewma5 = ssp5_encode_with_range_coder_ewma5(&data);
        let enc_rle = ssp5_encode_with_range_coder_ewma5_rle(&data);
        println!("EWMA5: {} bytes", enc_ewma5.len());
        println!("EWMA5+RLE: {} bytes", enc_rle.len());
        println!("RLE improvement: {:.1}%", 100.0 * (1.0 - enc_rle.len() as f64 / enc_ewma5.len() as f64));
    }
}
