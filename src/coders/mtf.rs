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
}
