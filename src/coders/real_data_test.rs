//! Test Huffman pipeline compression on real data files.
use super::ssp5_pipeline::{ssp5_encode_auto, ssp5_encode, ssp5_decode, 
                           ssp5_encode_with_range_coder, ssp5_decode_with_range_coder,
                           ssp5_encode_with_range_coder_o1, ssp5_decode_with_range_coder_o1,
                           ssp5_encode_with_range_coder_o2, ssp5_decode_with_range_coder_o2,
                           ssp5_encode_with_range_coder_mix, ssp5_decode_with_range_coder_mix};
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
    
    println!("\n=== SSP5 Pipeline Compression Comparison ===");
    for path in &files {
        match fs::read(path) {
            Ok(data) => {
                let s: Vec<u64> = vec![13, 17, 19, 23, 29, 31, 37, 41];
                
                // SSP pipeline
                let compressed = ssp5_encode_auto(&data, &s, 16);
                let ratio = 100.0 * compressed.len() as f64 / data.len() as f64;
                println!("\n  {} ({:.0} bytes):", path, data.len());
                println!("    SSP:        {} bytes ({:.2}%)", compressed.len(), ratio);
                
                // Range coder order-0 pipeline
                let rc_compressed = ssp5_encode_with_range_coder(&data);
                let rc_ratio = 100.0 * rc_compressed.len() as f64 / data.len() as f64;
                println!("    RC O0:      {} bytes ({:.2}%)", rc_compressed.len(), rc_ratio);
                
                // Range coder order-1 pipeline
                let rc1_compressed = ssp5_encode_with_range_coder_o1(&data);
                let rc1_ratio = 100.0 * rc1_compressed.len() as f64 / data.len() as f64;
                println!("    RC O1:      {} bytes ({:.2}%)", rc1_compressed.len(), rc1_ratio);
                
                // Range coder order-2 pipeline
                let rc2_compressed = ssp5_encode_with_range_coder_o2(&data);
                let rc2_ratio = 100.0 * rc2_compressed.len() as f64 / data.len() as f64;
                println!("    RC O2:      {} bytes ({:.2}%)", rc2_compressed.len(), rc2_ratio);
                
                // Range coder order-mix pipeline
                let rcm_compressed = ssp5_encode_with_range_coder_mix(&data);
                let rcm_ratio = 100.0 * rcm_compressed.len() as f64 / data.len() as f64;
                println!("    RC MIX:     {} bytes ({:.2}%)", rcm_compressed.len(), rcm_ratio);
                
                // Verify roundtrips
                let rc_decoded = ssp5_decode_with_range_coder(&rc_compressed);
                let rc1_decoded = ssp5_decode_with_range_coder_o1(&rc1_compressed);
                let rc2_decoded = ssp5_decode_with_range_coder_o2(&rc2_compressed);
                
                if rc_decoded.as_ref().map_or(false, |d| d == &data) {
                    println!("    RC O0 roundtrip: OK");
                } else {
                    println!("    RC O0 roundtrip: FAILED");
                }
                
                if rc1_decoded.as_ref().map_or(false, |d| d == &data) {
                    println!("    RC O1 roundtrip: OK");
                } else {
                    println!("    RC O1 roundtrip: FAILED");
                }
                
                if rc2_decoded.as_ref().map_or(false, |d| d == &data) {
                    println!("    RC O2 roundtrip: OK");
                } else {
                    println!("    RC O2 roundtrip: FAILED");
                }
                
                let rcm_decoded = ssp5_decode_with_range_coder_mix(&rcm_compressed);
                if rcm_decoded.as_ref().map_or(false, |d| d == &data) {
                    println!("    RC MIX roundtrip: OK");
                } else {
                    println!("    RC MIX roundtrip: FAILED");
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
        "\nRepetitive 1000 bytes: {} → {} bytes ({:.2}%)",
        data.len(),
        compressed.len(),
        ratio
    );
    
    assert!(compressed.len() < data.len(), "Compression should reduce size");
}