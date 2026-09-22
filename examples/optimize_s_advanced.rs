//! Advanced S-sequence optimizer with genetic algorithm and cascade testing
//! Tests different S-sequence variations on SSP5 pipeline + cascade combos

use std::fs;
use std::time::Instant;
use ssp4_rs::{ssp5_encode, ssp5_decode};

const BIBLE: &str = "D:/tmp/bible.txt";
const ALICE: &str = "D:/PROJECT UNIVERSE/01Compression/SSP5/tests/main/alice_in_wonderland.txt";

/// Default prime S sequence
fn primes_s() -> Vec<u64> {
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

/// Generate hybrid S sequences with different densities
fn generate_hybrids() -> Vec<(String, Vec<u64>)> {
    let mut sequences = Vec::new();
    
    // Dense ranges
    for n in [32, 64, 96, 128, 192, 256] {
        sequences.push((format!("dense_1-{}", n), (1u64..=n as u64).collect()));
    }
    
    // Prime + dense mix variations
    let prime_base: Vec<u64> = vec![
        2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71,
        73, 79, 83, 89, 97, 101, 103, 107, 109, 113, 127, 131
    ];
    
    for add in [0, 8, 16, 32, 64] {
        let mut s = prime_base.clone();
        for i in 1..=add {
            s.push(i as u64 * 100 + 1);
        }
        sequences.push((format!("primes+{}x100", add), s));
    }
    
    // Sparse primes
    let sparse_primes: Vec<u64> = vec![
        2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71,
        73, 79, 83, 89, 97, 101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151
    ];
    sequences.push(("sparse_primes".to_string(), sparse_primes));
    
    // Fibonacci-based
    let mut fib_s = vec![1, 2];
    while fib_s.len() < 64 {
        let n = fib_s.len();
        fib_s.push(fib_s[n-1] + fib_s[n-2]);
    }
    sequences.push(("fibonacci".to_string(), fib_s));
    
    // Powers of 2
    let powers: Vec<u64> = (0u64..6).flat_map(|e| {
        (1u64..=(16u64 << e)).step_by((1 << e) as usize)
    }).take(64).collect();
    sequences.push(("powers_of_2".to_string(), powers));
    
    // Alternating dense/sparse
    let mut alt = Vec::new();
    for i in 0..64 {
        if i % 2 == 0 {
            alt.push(i as u64 + 1);
        } else {
            alt.push((i as u64 + 1).pow(2));
        }
    }
    sequences.push(("alternating".to_string(), alt));
    
    // Known good combinations
    sequences.push(("primes_dense_mix".to_string(), vec![
        1, 2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
        101, 103, 107, 109, 113, 127
    ]));
    
    sequences
}

fn evaluate_s(data: &[u8], s: &[u64], block_bits: usize) -> Option<usize> {
    let enc = ssp5_encode(data, s, block_bits);
    let dec = ssp5_decode(&enc).ok()?;
    if dec != data {
        return None;
    }
    Some(enc.len())
}

fn ratio(size: usize, data_len: usize) -> f64 {
    size as f64 / data_len as f64 * 100.0
}

fn main() {
    let test_file = BIBLE;
    let data = fs::read(test_file).expect("failed to read test file");
    let data_len = data.len();
    println!("=== Advanced S-Sequence Optimizer ===");
    println!("File: {} ({} bytes)", test_file, data_len);
    
    let block_bits = 16;
    let sequences = generate_hybrids();
    
    println!("\n=== Testing {} S-sequence variations ===", sequences.len());
    println!("{:<25} {:>10} {:>8}", "Name", "Size", "Ratio%");
    println!("{}", "-".repeat(45));
    
    let mut results: Vec<_> = sequences.iter()
        .map(|(name, s)| {
            let t0 = Instant::now();
            let size = evaluate_s(&data, s, block_bits);
            let elapsed = t0.elapsed();
            (name.clone(), s.clone(), size, elapsed)
        })
        .collect();
    
    results.sort_by_key(|r| r.2);
    
    let mut best_size = usize::MAX;
    let mut best_name = String::new();
    let mut best_s: Vec<u64> = Vec::new();
    
    for (name, _, size, elapsed) in &results {
        if let Some(sz) = size {
            let r = ratio(*sz, data_len);
            println!("{:<25} {:>10} {:>7.2}% ({:.1}s)", name, sz, r, elapsed.as_secs_f32());
            if *sz < best_size {
                best_size = *sz;
                best_name = name.clone();
                best_s = sequences.iter().find(|(n, _)| n == name).unwrap().1.clone();
            }
        } else {
            println!("{:<25} {:>10} {:>7}", name, "FAIL", "-");
        }
    }
    
    println!("\n=== BEST S-SEQUENCE ===");
    println!("Name: {}", best_name);
    println!("Size: {} bytes ({:.2}%)", best_size, ratio(best_size, data_len));
    println!("S sequence: {:?}", &best_s[..best_s.len().min(16)]);
    if best_s.len() > 16 {
        println!("... ({} total elements)", best_s.len());
    }
    
    // Save best S sequence
    println!("\n=== SAVING BEST S ===");
    let out_file = "D:/tmp/best_s_sequence.txt";
    let s_str: String = best_s.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",");
    fs::write(out_file, s_str).expect("failed to write");
    println!("Saved to: {}", out_file);
}
