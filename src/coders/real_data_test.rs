//! Test Huffman pipeline compression on real data files.
use super::ssp5_pipeline::ssp5_encode_auto;
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
                let compressed = ssp5_encode_auto(&data, &s, 16);
                let ratio = 100.0 * compressed.len() as f64 / data.len() as f64;
                println!(
                    "  {} ({:.0} bytes): {} → {} bytes ({:.2}%)",
                    path,
                    data.len(),
                    data.len(),
                    compressed.len(),
                    ratio
                );
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