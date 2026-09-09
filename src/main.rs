//! SSP4 codec CLI — encode/decode files using special prime sequences

use std::env;
use std::fs;
use std::time::Instant;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        println!("Usage:");
        println!("  {} encode <input> <output> [block_bits=16] [delta=0]", args[0]);
        println!("  {} decode <input> <output>", args[0]);
        return;
    }

    let cmd = &args[1];
    match cmd.as_str() {
        "encode" => {
            let input_path = &args[2];
            let output_path = &args[3];
            let block_bits: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(16);
            let delta = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0) != 0;

            let data = fs::read(input_path).expect("failed to read input file");
            println!("Encoding {} bytes (bb={}, delta={})...", data.len(), block_bits, delta);

            // Build test S sequence (first N primes)
            let s = build_test_s(64);

            let t0 = Instant::now();
            let enc = ssp4_rs::encode(&data, &s, block_bits, delta);
            let elapsed = t0.elapsed();

            fs::write(output_path, &enc).expect("failed to write output");
            let ratio = if !data.is_empty() {
                100.0 * enc.len() as f64 / data.len() as f64
            } else {
                0.0
            };
            println!(
                "Done: {} → {} bytes ({:.1}%) in {:.3}s",
                data.len(),
                enc.len(),
                ratio,
                elapsed.as_secs_f64()
            );
        }
        "decode" => {
            let input_path = &args[2];
            let output_path = &args[3];

            let data = fs::read(input_path).expect("failed to read input file");
            println!("Decoding {} bytes...", data.len());

            // Build same S sequence
            let s = build_test_s(64);

            let t0 = Instant::now();
            let dec = ssp4_rs::decode(&data, &s).expect("decode failed");
            let elapsed = t0.elapsed();

            fs::write(output_path, &dec).expect("failed to write output");
            println!("Done: {} bytes decoded in {:.3}s", dec.len(), elapsed.as_secs_f64());
        }
        _ => {
            println!("Unknown command: {cmd}");
        }
    }
}

/// Build a test S sequence: first n primes starting from 2.
fn build_test_s(n: usize) -> Vec<u64> {
    let mut primes = Vec::with_capacity(n);
    let mut candidate = 2u64;
    while primes.len() < n {
        if is_prime(candidate) {
            primes.push(candidate);
        }
        candidate += 1;
    }
    primes
}

fn is_prime(n: u64) -> bool {
    if n < 2 {
        return false;
    }
    if n % 2 == 0 {
        return n == 2;
    }
    let mut d = 3;
    while d * d <= n {
        if n % d == 0 {
            return false;
        }
        d += 2;
    }
    true
}
