//! Test Huffman pipeline compression on real data files.
use super::ssp5_pipeline::{ssp5_encode_auto, ssp5_encode, ssp5_decode, 
                           ssp5_encode_with_range_coder, ssp5_decode_with_range_coder};
use std::fs;

fn test_real_data_compression() {
    let mut files = vec![];
    
    let possible_paths = [
        "test_alice.txt",
        "tests/comparison_corpora/canterbury/alice29.txt",
        r"D:\PROJECT UNIVERSE\01Compression\SSP5\tests\comparison_corpora\canterbury\alice29.txt",
        "alice29.txt",
    ];
    
    for path in &possible_paths {
        if fs::metadata(path).is_ok() {
            files.push((*path).to_string());
        }
    }
    
    println!("Testing SSP5 pipeline compression on real data:");
    for path in &files {
        match fs::read(path) {
            Ok(data) => {
                let s: Vec<u64> = vec![13, 17, 19, 23, 29, 31, 37, 41];
                
                // SSP pipeline
                let compressed = ssp5_encode_auto(&data, &s, 16);
                let ratio = 100.0 * compressed.len() as f64 / data.len() as f64;
                println!(
                    "  {} ({:.0} bytes): SSP {} → {} bytes ({:.2}%)",
                    path,
                    data.len(),
                    data.len(),
                    compressed.len(),
                    ratio
                );
                
                // Range coder pipeline
                let rc_compressed = ssp5_encode_with_range_coder(&data);
                let rc_ratio = 100.0 * rc_compressed.len() as f64 / data.len() as f64;
                println!(
                    "    Range coder: {} → {} bytes ({:.2}%)",
                    data.len(),
                    rc_compressed.len(),
                    rc_ratio
                );
                
                // Verify roundtrip
                let rc_decoded = ssp5_decode_with_range_coder(&rc_compressed).expect("RC decode failed");
                if rc_decoded != data {
                    println!("    ERROR: Range coder roundtrip failed!");
                } else {
                    println!("    Range coder roundtrip: OK");
                }
            }
            Err(e) => println!("    Error reading {}: {}", path, e),
        }
    }
}

#[test]
fn test_real_data_compression_pipeline() {
    test_real_data_compression();
    
    let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
        .cycle().take(1000).copied().collect();
    
    let s: Vec<u64> = vec![13, 17, 19, 23, 29, 31, 37, 41];
    let compressed = ssp5_encode_auto(&data, &s, 16);
    let ratio = 100.0 * compressed.len() as f64 / data.len() as f64;
    println!(
        "Repetitive 1000 bytes: {} → {} bytes ({:.2}%)",
        data.len(),
        compressed.len(),
        ratio
    );
    
    assert!(compressed.len() < data.len(), "Compression should reduce size");
}