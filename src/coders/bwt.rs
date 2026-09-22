//! BWT (Burrows-Wheeler Transform) implementation
//! 
//! Format: [u32 primary_index][last_column_bytes]
//! 
//! BWT переставляет символы так, чтобы похожие символы группировались,
//! что создаёт локальную корреляцию для MTF и Range Coder.

/// Encode data using Burrows-Wheeler Transform.
/// Returns (primary_index, last_column) where primary_index is the position
/// of the original string in the sorted rotations.
pub fn bwt_encode(data: &[u8]) -> (u32, Vec<u8>) {
    bwt_encode_impl(data, false)
}

/// Encode with iterative BWT passes (can improve compression for some data)
pub fn bwt_encode_iterative(data: &[u8], passes: usize) -> (u32, Vec<u8>) {
    let mut current = data.to_vec();
    let mut final_primary = 0u32;
    
    for p in 0..passes {
        let (primary, last) = bwt_encode_impl(&current, p > 0);
        final_primary = primary;
        current = last;
    }
    
    (final_primary, current)
}

fn bwt_encode_impl(data: &[u8], _is_iteration: bool) -> (u32, Vec<u8>) {
    let n = data.len();
    if n == 0 {
        return (0, Vec::new());
    }
    if n == 1 {
        return (0, data.to_vec());
    }

    // Build suffix array using prefix-doubling (same as Python reference)
    let mut sa: Vec<usize> = (0..n).collect();
    // Use usize for rank to handle large inputs (n > 65535)
    let mut rank: Vec<usize> = data.iter().map(|&b| b as usize).collect();
    let mut tmp = vec![0usize; n];
    let mut keys = vec![(0usize, 0usize); n];

    let mut k = 1usize;
    while k < n {
        // Pre-compute keys: (rank[i], rank[(i+k) % n])
        for i in 0..n {
            let second = if i + k < n { rank[i + k] } else { rank[i + k - n] };
            keys[i] = (rank[i], second);
        }

        // Sort by keys (stable sort)
        sa.sort_by(|&a, &b| keys[a].cmp(&keys[b]));

        // Update ranks
        tmp[sa[0]] = 0;
        for i in 1..n {
            let prev = sa[i - 1];
            let cur = sa[i];
            tmp[cur] = tmp[prev] + (keys[prev] != keys[cur]) as usize;
        }

        std::mem::swap(&mut rank, &mut tmp);
        if rank[sa[n - 1]] == n - 1 {
            break;
        }
        k <<= 1;
    }

    // Build last column: character at position i-1 in original string
    // (or n-1+i-1 for i=0)
    let mut last = Vec::with_capacity(n);
    for &i in &sa {
        let pos = if i == 0 { n - 1 } else { i - 1 };
        last.push(data[pos]);
    }

    // Find primary index: where does original string end up?
    let primary = sa.iter().position(|&i| i == 0).unwrap() as u32;

    (primary, last)
}

/// Decode data from Burrows-Wheeler Transform.
/// Takes primary_index and last_column, reconstructs original data.
pub fn bwt_decode(primary: u32, last: &[u8]) -> Vec<u8> {
    let n = last.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return last.to_vec();
    }

    // Build first column (sorted last column)
    let mut first_col: Vec<u8> = last.to_vec();
    first_col.sort();

    // Build LF-mapping
    // For each position i in last column, find its occurrence in first column
    let mut lf = vec![0usize; n];

    // Track occurrences of each character
    let mut occ_count = vec![0usize; 256];
    let mut l_rank = vec![0usize; n];

    for i in 0..n {
        let c = last[i] as usize;
        l_rank[i] = occ_count[c];
        occ_count[c] += 1;
    }

    // Build first occurrence table
    let mut first_occ = vec![0usize; 256];
    let mut prev_c = 256;
    for (i, &c) in first_col.iter().enumerate() {
        let c = c as usize;
        if c != prev_c {
            first_occ[c] = i;
            prev_c = c;
        }
    }

    // LF[i] = position of i-th character from last column in first column
    for i in 0..n {
        let c = last[i] as usize;
        lf[i] = first_occ[c] + l_rank[i];
    }

    // Reconstruct: start at primary, follow LF-mapping
    // This gives the string in REVERSE order, so we reverse at the end
    let mut out = Vec::with_capacity(n);
    let mut pos = primary as usize;
    for _ in 0..n {
        out.push(last[pos]);
        pos = lf[pos];
    }
    out.reverse();
    out
}

/// Pack BWT output: [u32 primary][last column]
pub fn pack_bwt(primary: u32, last: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + last.len());
    out.extend_from_slice(&primary.to_le_bytes());
    out.extend_from_slice(last);
    out
}

/// Unpack BWT from bytes: returns (primary, last)
pub fn unpack_bwt(data: &[u8]) -> (u32, &[u8]) {
    if data.len() < 4 {
        return (0, &[]);
    }
    let primary = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let last = &data[4..];
    (primary, last)
}

/// Chunked BWT: split data into blocks and BWT each block independently
/// Format: [num_chunks(4)][chunk1_header][chunk1_data][chunk2_header][chunk2_data]...
/// Each chunk header: [chunk_size(4)][primary(4)]
pub fn bwt_encode_chunked(data: &[u8], chunk_size: usize) -> Vec<u8> {
    if data.len() <= chunk_size {
        // No chunking needed
        let (primary, last) = bwt_encode(data);
        return pack_bwt(primary, &last);
    }
    
    let num_chunks = (data.len() + chunk_size - 1) / chunk_size;
    let mut out = Vec::with_capacity(data.len() + num_chunks * 8 + 4);
    
    // Write number of chunks
    out.extend_from_slice(&(num_chunks as u32).to_le_bytes());
    
    for i in 0..num_chunks {
        let start = i * chunk_size;
        let end = (start + chunk_size).min(data.len());
        let chunk = &data[start..end];
        
        let (primary, last) = bwt_encode(chunk);
        
        // Write chunk header: size + primary
        let chunk_len = last.len() as u32;
        out.extend_from_slice(&chunk_len.to_le_bytes());
        out.extend_from_slice(&primary.to_le_bytes());
        
        // Write chunk data
        out.extend_from_slice(&last);
    }
    
    out
}

/// Decode chunked BWT
pub fn bwt_decode_chunked(data: &[u8]) -> Vec<u8> {
    if data.len() < 4 {
        return Vec::new();
    }
    
    let num_chunks = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut offset = 4;
    
    // Check if this is actually a non-chunked BWT (no chunk header prefix)
    // If first 4 bytes look like chunk_size and primary for a single chunk, try decode as single
    if num_chunks == 1 && data.len() >= 12 {
        // This might be a non-chunked BWT, try decoding directly
        let (primary, last) = unpack_bwt(data);
        if last.len() > 0 {
            return bwt_decode(primary, last);
        }
    }
    
    // Chunked decode
    let mut out = Vec::new();
    
    for _ in 0..num_chunks {
        if offset + 8 > data.len() {
            break;
        }
        
        let chunk_len = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]) as usize;
        let primary = u32::from_le_bytes([data[offset+4], data[offset+5], data[offset+6], data[offset+7]]);
        offset += 8;
        
        if offset + chunk_len > data.len() {
            break;
        }
        
        let last = &data[offset..offset + chunk_len];
        offset += chunk_len;
        
        let decoded = bwt_decode(primary, last);
        out.extend_from_slice(&decoded);
    }
    
    out
}

/// Pack chunked BWT for MTF
pub fn pack_chunked_bwt(encoded: &[u8]) -> Vec<u8> {
    encoded.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bwt_roundtrip() {
        let test_cases = vec![
            b"banana".to_vec(),
            b"aaaa".to_vec(),
            b"abracadabra".to_vec(),
            b"hello world".to_vec(),
            b"".to_vec(),
            b"a".to_vec(),
            b"aaaaabbbbb".to_vec(),
        ];

        for data in test_cases {
            let (primary, last) = bwt_encode(&data);
            let decoded = bwt_decode(primary, &last);
            assert_eq!(decoded, data, "BWT roundtrip failed for {:?}", data);
        }
    }

    #[test]
    fn test_bwt_pack_unpack() {
        let data = b"test data";
        let (primary, last) = bwt_encode(data);
        let packed = pack_bwt(primary, &last);
        let (unpacked_primary, unpacked_last) = unpack_bwt(&packed);
        
        assert_eq!(primary, unpacked_primary);
        assert_eq!(&last, unpacked_last);
        
        let decoded = bwt_decode(unpacked_primary, unpacked_last);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_bwt_alice() {
        // Test on larger data
        let data = b"alice in wonderland chapter one down the rabbit hole";
        let (primary, last) = bwt_encode(data);
        let decoded = bwt_decode(primary, &last);
        assert_eq!(decoded, data);
    }
}
