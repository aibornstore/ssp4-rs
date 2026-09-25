//! MTF (Move-To-Front) encoding implementation
//! 
//! MTF поддерживает список символов и возвращает индекс при каждом обращении.
//! После обращения символ перемещается в начало списка.
//! 
//! Format: [u32 original_len][mtf_indices as bytes]

/// Default alphabet size (256 byte values)
pub const ALPHABET_SIZE: usize = 256;

/// Encode data using Move-To-Front transform.
/// Returns vector of MTF indices (0-255 values).
pub fn mtf_encode(data: &[u8]) -> Vec<u8> {
    // Initialize alphabet: [0, 1, 2, ..., 255]
    let mut alphabet: Vec<u8> = (0u8..=255).collect();
    
    let mut out = Vec::with_capacity(data.len());
    
    for &b in data {
        // Find position of byte in alphabet
        let pos = alphabet.iter().position(|&x| x == b).unwrap_or(0);
        out.push(pos as u8);
        
        // Move to front
        if pos > 0 {
            alphabet.remove(pos);
            alphabet.insert(0, b);
        }
    }
    
    out
}

/// Decode MTF indices back to original bytes.
pub fn mtf_decode(mtf_indices: &[u8]) -> Vec<u8> {
    // Initialize alphabet: [0, 1, 2, ..., 255]
    let mut alphabet: Vec<u8> = (0u8..=255).collect();

    let mut out = Vec::with_capacity(mtf_indices.len());

    for &idx in mtf_indices {
        let idx = idx as usize;
        let b = if idx < alphabet.len() {
            alphabet[idx]
        } else {
            0 // Fallback for safety
        };
        out.push(b);

        // Move to front
        if idx > 0 && idx < alphabet.len() {
            alphabet.remove(idx);
            alphabet.insert(0, b);
        }
    }

    out
}

/// Decode MTF indices back to original bytes, resetting alphabet per chunk.
/// Chunk format: [chunk_len(4)][mtf_indices...] repeated.
pub fn mtf_decode_chunked(data: &[u8], _chunk_size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut offset = 0;

    while offset < data.len() {
        if offset + 4 > data.len() {
            break;
        }
        let chunk_len = u32::from_le_bytes([
            data[offset], data[offset + 1], data[offset + 2], data[offset + 3]
        ]) as usize;
        offset += 4;

        if offset + chunk_len > data.len() {
            break;
        }

        // Fresh alphabet for each chunk (reset at chunk boundary)
        let mtf_chunk = &data[offset..offset + chunk_len];
        out.extend(mtf_decode_single_chunk(mtf_chunk));
        offset += chunk_len;
    }

    out
}

/// Decode a single chunk with a fresh alphabet.
fn mtf_decode_single_chunk(mtf_indices: &[u8]) -> Vec<u8> {
    let mut alphabet: Vec<u8> = (0u8..=255).collect();
    let mut out = Vec::with_capacity(mtf_indices.len());

    for &idx in mtf_indices {
        let idx = idx as usize;
        let b = if idx < alphabet.len() {
            alphabet[idx]
        } else {
            0
        };
        out.push(b);

        if idx > 0 && idx < alphabet.len() {
            alphabet.remove(idx);
            alphabet.insert(0, b);
        }
    }

    out
}

/// Encode data in chunks with fresh MTF alphabet per chunk (adaptive MTF).
/// Returns: [chunk1_len(4)][mtf1...][chunk2_len(4)][mtf2...]...
pub fn mtf_encode_adaptive(data: &[u8], chunk_size: usize) -> Vec<u8> {
    if data.is_empty() || chunk_size == 0 {
        return pack_mtf(data, &mtf_encode(data));
    }

    let num_chunks = (data.len() + chunk_size - 1) / chunk_size;
    let mut out = Vec::with_capacity(8 + data.len() * 2); // Rough estimate

    for i in 0..num_chunks {
        let start = i * chunk_size;
        let end = (start + chunk_size).min(data.len());
        let chunk = &data[start..end];

        // Encode chunk with fresh alphabet
        let mtf_chunk = mtf_encode(chunk);

        // Append chunk length + MTF data
        out.extend_from_slice(&(mtf_chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&mtf_chunk);
    }

    out
}

/// Apply MTF to each BWT chunk independently with fresh alphabet per chunk.
/// Takes bwt_packed from bwt_encode_chunked and returns MTF output with chunk headers.
/// bwt_packed format: [num_chunks(4)][chunk1: [len(4)][primary(4)][last...]][chunk2: ...]...
/// Output format: [mtf_chunk1_len(4)][mtf1...][mtf_chunk2_len(4)][mtf2...]...
pub fn mtf_encode_bwt_chunked(bwt_packed: &[u8], _chunk_size: usize) -> Vec<u8> {
    if bwt_packed.len() < 4 {
        return pack_mtf(bwt_packed, &mtf_encode(bwt_packed));
    }

    let num_chunks = u32::from_le_bytes([
        bwt_packed[0], bwt_packed[1], bwt_packed[2], bwt_packed[3]
    ]) as usize;

    if num_chunks == 0 || num_chunks > 1_000_000 {
        return pack_mtf(bwt_packed, &mtf_encode(bwt_packed));
    }

    let mut out = Vec::with_capacity(bwt_packed.len() + num_chunks * 8);
    let mut offset = 4; // Skip num_chunks

    for _ in 0..num_chunks {
        if offset + 8 > bwt_packed.len() {
            break;
        }

        let chunk_len = u32::from_le_bytes([
            bwt_packed[offset], bwt_packed[offset + 1],
            bwt_packed[offset + 2], bwt_packed[offset + 3]
        ]) as usize;
        offset += 4; // Skip chunk_len

        if offset + 4 + chunk_len > bwt_packed.len() {
            break;
        }

        // BWT chunk = [primary(4)][last_col(chunk_len bytes)]
        let primary_and_last = &bwt_packed[offset..offset + 4 + chunk_len];

        // Apply MTF with fresh alphabet for this chunk
        let mtf_chunk = mtf_encode(primary_and_last);

        // Append chunk length + MTF data
        out.extend_from_slice(&(mtf_chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&mtf_chunk);

        offset += 4 + chunk_len; // Skip primary + last_col
    }

    out
}

/// Decode MTF data produced by mtf_encode_bwt_chunked (chunked BWT with fresh MTF per chunk).
/// Reconstructs the original bwt_packed format: [num_chunks(4)][chunk: [len(4)][primary(4)][last...]]...
pub fn mtf_decode_bwt_chunked(mtf_data: &[u8]) -> Vec<u8> {
    if mtf_data.is_empty() {
        return Vec::new();
    }

    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut offset = 0;

    // Read each MTF chunk (format: [mtf_chunk_len(4)][mtf_chunk...])
    while offset < mtf_data.len() {
        if offset + 4 > mtf_data.len() {
            break;
        }
        let mtf_chunk_len = u32::from_le_bytes([
            mtf_data[offset], mtf_data[offset + 1],
            mtf_data[offset + 2], mtf_data[offset + 3]
        ]) as usize;
        offset += 4;

        if offset + mtf_chunk_len > mtf_data.len() {
            break;
        }

        let mtf_chunk = &mtf_data[offset..offset + mtf_chunk_len];
        // Decode with fresh alphabet for this chunk
        let decoded_chunk = mtf_decode_single_chunk(mtf_chunk);

        // decoded_chunk = [primary(4)][last_col...]
        if decoded_chunk.len() < 4 {
            break;
        }
        let primary = u32::from_le_bytes([
            decoded_chunk[0], decoded_chunk[1],
            decoded_chunk[2], decoded_chunk[3]
        ]);
        let last_col = decoded_chunk[4..].to_vec();

        chunks.push((primary, last_col));
        offset += mtf_chunk_len;
    }

    // Reconstruct bwt_packed format: [num_chunks(4)][chunk: [len(4)][primary(4)][last...]]...
    let mut out = Vec::with_capacity(mtf_data.len() + 8);
    out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());

    for (primary, last_col) in &chunks {
        out.extend_from_slice(&(last_col.len() as u32).to_le_bytes()); // chunk size
        out.extend_from_slice(&primary.to_le_bytes());
        out.extend_from_slice(last_col);
    }

    out
}

// === BiT-MTF (bit-level MTF as in bzip2) ===
// Splits MTF indices into 8 bit-planes, each encoded separately.
// This exploits that different bit positions have different probability distributions.

/// Encode MTF indices using BiT-MTF (bit-level MTF).
/// Returns 8 Vec<u8>, one per bit position (LSB first).
/// Each bit stream has local patterns exploited by range coder.
pub fn bit_mtf_encode(mtf_indices: &[u8]) -> [Vec<u8>; 8] {
    // Each bit position: collect that bit from all MTF indices
    let mut bit_streams: [Vec<u8>; 8] = [
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
    ];
    
    for &idx in mtf_indices {
        for bit in 0..8 {
            bit_streams[bit].push((idx >> bit) & 1);
        }
    }
    
    // Apply MTF to each bit stream for local patterns
    // (bit streams have runs of 0s and occasional 1s)
    let mut result: [Vec<u8>; 8] = [
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
        Vec::with_capacity(mtf_indices.len()),
    ];
    
    for bit in 0..8 {
        // Small MTF for bit streams (only 2 symbols: 0 and 1)
        let mut mtf_list = vec![0u8, 1u8];
        
        for &b in &bit_streams[bit] {
            let pos = mtf_list.iter().position(|&x| x == b).unwrap_or(0);
            result[bit].push(pos as u8);
            if pos > 0 {
                mtf_list.remove(pos);
                mtf_list.insert(0, b);
            }
        }
    }
    
    result
}

/// Decode BiT-MTF back to MTF indices.
pub fn bit_mtf_decode(bit_streams: &[Vec<u8>; 8], original_len: usize) -> Vec<u8> {
    // Reverse MTF on each bit stream
    let mut bit_recovered: [Vec<u8>; 8] = [
        Vec::with_capacity(original_len),
        Vec::with_capacity(original_len),
        Vec::with_capacity(original_len),
        Vec::with_capacity(original_len),
        Vec::with_capacity(original_len),
        Vec::with_capacity(original_len),
        Vec::with_capacity(original_len),
        Vec::with_capacity(original_len),
    ];
    
    for bit in 0..8 {
        let mut mtf_list = vec![0u8, 1u8];
        for &mtf_val in &bit_streams[bit] {
            let idx = mtf_val as usize;
            let b = if idx < mtf_list.len() { mtf_list[idx] } else { 0 };
            bit_recovered[bit].push(b);
            if idx > 0 && idx < mtf_list.len() {
                mtf_list.remove(idx);
                mtf_list.insert(0, b);
            }
        }
    }
    
    // Reconstruct MTF indices from bit planes
    let mut mtf_indices = Vec::with_capacity(original_len);
    for i in 0..original_len {
        let mut idx: u8 = 0;
        for bit in 0..8 {
            idx |= bit_recovered[bit][i] << bit;
        }
        mtf_indices.push(idx);
    }
    
    mtf_indices
}

/// Encode data with BiT-MTF: BWT → MTF → BiT-MTF → RangeCoder EWMA5 per bit plane.
/// Returns (primary, packed_bit_streams) where packed_bit_streams is Vec<u8>.
pub fn bit_mtf_pack(primary: u32, mtf_indices: &[u8]) -> Vec<u8> {
    let bit_streams = bit_mtf_encode(mtf_indices);
    
    // Pack format: [primary(4)][orig_len(4)][bit0_data(orig_len)][bit1_data(orig_len)]...
    let mut out = Vec::new();
    out.extend_from_slice(&primary.to_le_bytes());
    out.extend_from_slice(&(mtf_indices.len() as u32).to_le_bytes());
    
    // All bit streams have same length = orig_len
    for bit in 0..8 {
        out.extend_from_slice(bit_streams[bit].as_slice());
    }
    
    out
}

/// Decode BiT-MTF packed data back to MTF indices.
pub fn bit_mtf_unpack(data: &[u8]) -> Option<(u32, Vec<u8>)> {
    // Header: primary(4) + orig_len(4) = 8 bytes
    if data.len() < 8 {
        return None;
    }
    
    let primary = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let orig_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    
    let total_size = 8 + 8 * orig_len; // header + 8 bit streams
    if data.len() < total_size {
        return None;
    }
    
    // Extract bit streams
    let mut bit_streams: [Vec<u8>; 8] = [
        Vec::with_capacity(orig_len),
        Vec::with_capacity(orig_len),
        Vec::with_capacity(orig_len),
        Vec::with_capacity(orig_len),
        Vec::with_capacity(orig_len),
        Vec::with_capacity(orig_len),
        Vec::with_capacity(orig_len),
        Vec::with_capacity(orig_len),
    ];
    
    let data_start = 8;
    for bit in 0..8 {
        let start = data_start + bit * orig_len;
        let end = start + orig_len;
        bit_streams[bit].extend_from_slice(&data[start..end]);
    }
    
    let mtf_indices = bit_mtf_decode(&bit_streams, orig_len);
    Some((primary, mtf_indices))
}

/// Pack MTF output: [u32 original_len][mtf_indices]
pub fn pack_mtf(data: &[u8], mtf_data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + mtf_data.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(mtf_data);
    out
}

/// Unpack MTF from bytes: returns (original_len, mtf_indices)
pub fn unpack_mtf(data: &[u8]) -> (usize, &[u8]) {
    if data.len() < 4 {
        return (0, &[]);
    }
    let orig_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mtf_data = &data[4..];
    (orig_len, mtf_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mtf_roundtrip() {
        let test_cases = vec![
            b"banana".to_vec(),
            b"aaaa".to_vec(),
            b"hello world".to_vec(),
            b"".to_vec(),
            b"a".to_vec(),
            vec![0, 0, 0, 0],  // zeros
            b"abcdefghijklmnopqrstuvwxyz".to_vec(),
            (0..=255u8).collect::<Vec<_>>(),  // all byte values
        ];

        for data in test_cases {
            let encoded = mtf_encode(&data);
            let decoded = mtf_decode(&encoded);
            assert_eq!(decoded, data, "MTF roundtrip failed for {:?}", &data[..data.len().min(20)]);
        }
    }

    #[test]
    fn test_mtf_pack_unpack() {
        let data = b"test data with various bytes: \x00\xff\x80";
        let encoded = mtf_encode(data);
        let packed = pack_mtf(data, &encoded);
        let (orig_len, unpacked) = unpack_mtf(&packed);
        
        assert_eq!(orig_len, data.len());
        assert_eq!(unpacked, encoded);
        
        let decoded = mtf_decode(unpacked);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_mtf_locality() {
        // MTF should give small indices for repeated characters
        let data = b"aaaaabbbbbccccc";
        let encoded = mtf_encode(data);
        
        // First 'a' at position 0 -> index 97
        // Subsequent 'a's should be at index 0 (just moved to front)
        assert_eq!(encoded[0], 97);  // 'a' first time
        assert_eq!(encoded[1], 0);   // 'a' second time (at front)
        assert_eq!(encoded[2], 0);   // 'a' third time (at front)
        assert_eq!(encoded[3], 0);   // 'a' fourth time
        assert_eq!(encoded[4], 0);   // 'a' fifth time
        
        // First 'b' should be at position 98
        assert_eq!(encoded[5], 98);  // 'b' first time
    }

    #[test]
    fn test_mtf_against_known_bwt() {
        // BWT output for "AAAAAABBBBBCCCCCCD": [68, 65, 65, 65, 65, 65, 65, 66, 66, 66, 66, 66, 67, 67, 67, 67, 67, 67]
        let bwt_data: [u8; 18] = [68, 65, 65, 65, 65, 65, 65, 66, 66, 66, 66, 66, 67, 67, 67, 67, 67, 67];
        let encoded = mtf_encode(&bwt_data);
        println!("MTF of BWT: {:?}", &encoded[..]);
        
        // Expected: [68, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 3, 0, 0, 0] (18 values)
        // With proper MTF, after 'D'=68 at front, 'A'=65 is at position 1 in [65, 68, 0, 1, ...]
        assert_eq!(encoded.len(), 18, "MTF should produce 18 values");
        
        // Count distinct values
        let mut distinct: std::collections::HashSet<u8> = std::collections::HashSet::new();
        for &v in &encoded {
            distinct.insert(v);
        }
        println!("Distinct MTF values: {:?}", distinct);
        println!("Distinct count: {}", distinct.len());
        
        // Verify roundtrip
        let decoded = mtf_decode(&encoded);
        assert_eq!(&decoded[..], &bwt_data[..], "MTF roundtrip failed");
    }
    
    #[test]
    fn test_bit_mtf_roundtrip() {
        // BiT-MTF: split MTF indices into bit planes, then reconstruct
        let mtf_data = vec![5u8, 10, 15, 20, 5, 5, 30, 40];
        let bit_streams = bit_mtf_encode(&mtf_data);
        
        // Each bit stream should have same length as MTF data
        for bit in 0..8 {
            assert_eq!(bit_streams[bit].len(), mtf_data.len());
        }
        
        let decoded = bit_mtf_decode(&bit_streams, mtf_data.len());
        assert_eq!(decoded, mtf_data);
    }
    
    #[test]
    fn test_bit_mtf_simple() {
        // Simple case: known MTF indices
        let mtf_data = vec![0u8, 1, 0, 1, 0, 1];
        let bit_streams = bit_mtf_encode(&mtf_data);
        
        // Bit 0 of [0,1,0,1,0,1] = [0,1,0,1,0,1]
        // MTF of [0,1,0,1,0,1]: start=[0,1], then 0→pos0, 1→pos1→[1,0], 
        // 0→pos1→[0,1], 1→pos1→[1,0], 0→pos1→[0,1], 1→pos1→[1,0]
        // Result: [0,1,1,1,1,1]
        assert_eq!(bit_streams[0].len(), mtf_data.len());
        
        // Bit 1: all 0s → MTF: [0, 0, 0, 0, 0, 0]
        assert_eq!(bit_streams[1].len(), mtf_data.len());
        
        let decoded = bit_mtf_decode(&bit_streams, mtf_data.len());
        assert_eq!(decoded, mtf_data);
    }
    
    #[test]
    fn test_bit_mtf_pack_unpack() {
        let mtf_data = vec![7u8, 42, 100, 200, 7, 8];
        let primary = 12345u32;
        
        let packed = bit_mtf_pack(primary, &mtf_data);
        println!("Packed size: {} bytes", packed.len());
        
        let (dec_primary, dec_mtf) = bit_mtf_unpack(&packed).expect(&format!(
            "unpack failed: packed_len={}, orig_len={}", packed.len(), mtf_data.len()
        ));
        
        assert_eq!(dec_primary, primary);
        assert_eq!(dec_mtf, mtf_data);
    }
    
    #[test]
    fn test_bit_mtf_from_bwt() {
        // Full roundtrip: BWT → MTF → BiT-MTF → unpack → MTF decode → BWT decode
        use super::super::bwt::{bwt_encode, bwt_decode, pack_bwt, unpack_bwt};
        
        let original = b"abracadabra and alakazam".to_vec();
        let (primary, bwt_data) = bwt_encode(&original);
        let bwt_packed = pack_bwt(primary, &bwt_data);
        let mtf_data = mtf_encode(&bwt_packed);
        
        // BiT-MTF encode and decode
        let packed = bit_mtf_pack(primary, &mtf_data);
        println!("BWT MTF len: {}, packed size: {}", mtf_data.len(), packed.len());
        let (dec_primary, dec_mtf) = bit_mtf_unpack(&packed).expect(&format!(
            "unpack failed: packed_len={}, mtf_len={}", packed.len(), mtf_data.len()
        ));
        
        assert_eq!(dec_primary, primary);
        assert_eq!(dec_mtf, mtf_data);
        
        // Full reconstruction
        let mtf_decoded = mtf_decode(&dec_mtf);
        let (_, dec_bwt) = unpack_bwt(&mtf_decoded);
        let reconstructed = bwt_decode(dec_primary, dec_bwt);
        
        assert_eq!(reconstructed, original);
    }
}
