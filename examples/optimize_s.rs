//! Greedy S-sequence optimizer for SSP5 pipeline (BWT + MTF + Range Coder)

use std::fs;
use std::time::Instant;
use ssp4_rs::{ssp5_encode, ssp5_decode};

const TEST_FILE: &str = "D:/PROJECT UNIVERSE/01Compression/SSP5/tests/main/alice_in_wonderland.txt";

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

fn evaluate_s(data: &[u8], s: &[u64], block_bits: usize) -> Option<usize> {
    let enc = ssp5_encode(data, s, block_bits);
    let dec = ssp5_decode(&enc);
    if dec != data {
        return None;
    }
    Some(enc.len())
}

fn dense_s(n: usize) -> Vec<u64> {
    (1u64..=n as u64).collect()
}

fn ratio(size: usize, data_len: usize) -> f64 {
    size as f64 / data_len as f64 * 100.0
}

fn main() {
    let data = fs::read(TEST_FILE).expect("failed to read test file");
    let data_len = data.len();
    println!("Loaded {} bytes from {}", data_len, TEST_FILE);
    
    let block_bits = 16;
    
    // Test various S sequences
    println!("\n=== Testing S sequences on {} bytes ===", data_len);
    println!("{:<25} {:>8} {:>8}", "Name", "Size", "Ratio%");
    println!("{}", "-".repeat(45));

    let sequences: Vec<(&str, Vec<u64>)> = vec![
        ("first_64_primes", default_s()),
        ("dense_1-64", dense_s(64)),
        ("dense_1-32", dense_s(32)),
        ("dense_1-128", dense_s(128)),
        ("dense_1-256", dense_s(256)),
        ("primes_dense_mix", vec![
            1, 2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
            101, 103, 107, 109, 113, 127
        ]),
        ("very_dense", (1u64..=128).collect()),
        ("super_dense", (1u64..=256).collect()),
    ];
    
    let mut results: Vec<_> = sequences.iter()
        .map(|(name, s)| {
            let t0 = Instant::now();
            let size = evaluate_s(&data, s, block_bits);
            let elapsed = t0.elapsed();
            (name, s.clone(), size, elapsed)
        })
        .collect();

    results.sort_by_key(|r| r.2);

    for (name, _, size, _elapsed) in &results {
        if let Some(sz) = size {
            let r = ratio(*sz, data_len);
            println!("{:<25} {:>8} {:>7.1}%", name, sz, r);
        } else {
            println!("{:<25} {:>8} {:>7}", name, "-", "FAIL");
        }
    }
    
    // Find best baseline
    let baseline_best = results.iter().filter_map(|r| r.2).min().unwrap_or(usize::MAX);
    println!("\nBest baseline: {} bytes ({:.1}%)", baseline_best, ratio(baseline_best, data_len));

    // Greedy optimization
    println!("\n=== Greedy S optimization ===");
    let mut best_s = default_s();
    let mut best_size = evaluate_s(&data, &best_s, block_bits).unwrap_or(usize::MAX);
    println!("Start: {} bytes ({:.1}%)", best_size, ratio(best_size, data_len));
    
    for iteration in 0..30 {
        let mut improved = false;
        
        // Try adding small values
        for v in 1..=200u64 {
            if best_s.contains(&v) { continue; }
            
            let mut candidate = best_s.clone();
            candidate.push(v);
            candidate.sort();
            candidate.dedup();
            
            if let Some(size) = evaluate_s(&data, &candidate, block_bits) {
                if size < best_size {
                    best_size = size;
                    best_s = candidate;
                    println!("Iter {}: +{} → {} ({:.1}%)", iteration, v, size, ratio(size, data_len));
                    improved = true;
                    break;
                }
            }
        }
        
        if !improved {
            // Try removing elements
            for i in 0..best_s.len() {
                let mut candidate = best_s.clone();
                candidate.remove(i);
                if candidate.len() < 3 { continue; }
                
                if let Some(size) = evaluate_s(&data, &candidate, block_bits) {
                    if size < best_size {
                        best_size = size;
                        best_s = candidate;
                        println!("Iter {}: -S[{}] → {} ({:.1}%)", iteration, i, size, ratio(size, data_len));
                        improved = true;
                        break;
                    }
                }
            }
        }
        
        if !improved {
            // Try replacing elements
            for i in 0..best_s.len() {
                for v in 1..=100u64 {
                    if best_s.contains(&v) { continue; }
                    
                    let mut candidate = best_s.clone();
                    candidate[i] = v;
                    candidate.sort();
                    
                    if let Some(size) = evaluate_s(&data, &candidate, block_bits) {
                        if size < best_size {
                            best_size = size;
                            best_s = candidate;
                            println!("Iter {}: S[{}]={} → {} ({:.1}%)", iteration, i, v, size, ratio(size, data_len));
                            improved = true;
                            break;
                        }
                    }
                }
                if improved { break; }
            }
        }
        
        if !improved {
            println!("No improvement at iteration {}", iteration);
            break;
        }
    }
    
    println!("\n=== BEST S SEQUENCE ===");
    println!("S = [{}]", best_s.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", "));
    println!("Size: {} bytes ({:.1}%)", best_size, ratio(best_size, data_len));
    println!("Compression: {:.1}x", data_len as f64 / best_size as f64);
}
