//! SSP4/SSP5 codec CLI — encode/decode/optimize files using special prime sequences
//! 
//! Commands:
//!   encode <input> <output> [block_bits] [delta] [S]       - Encode with SSP codec
//!   decode <input> <output> [S]                             - Decode SSP archive
//!   ssp5_encode <input> <output> [block_bits] [--auto | --s=S] - Encode with SSP5 (BWT+MTF+RC)
//!   ssp5_decode <input> <output>                           - Decode SSP5 (S extracted from archive)
//!   auto <input> <output> [block_bits]                    - Alias: encode with auto-optimized S
//!   optimize <input> [--output <file>] [--block-bits N]    - Find best S-sequence
//!   ewma7-auto <input> <output>                            - EWMA7 auto-tuned (v19/v21, 18 candidates)
//!   ewma7-decode <input> <output>                           - Decode EWMA7 archive
//!   ewma7-v18 <input> <output>                             - Legacy EWMA7 v18 (baseline)

use std::env;
use std::fs;
use std::time::Instant;

/// Parse S sequence from comma-separated string, or return default.
fn parse_s_sequence(s_arg: Option<&str>, ssp5_mode: bool) -> Vec<u64> {
    match s_arg {
        Some(s_str) => {
            s_str.split(',')
                .filter_map(|x| x.trim().parse::<u64>().ok())
                .collect()
        }
        None => {
            if ssp5_mode {
                // OPTIMIZED for text: sparse primes (skip 2,3,5,7,11)
                // Best for bible_100k: 33.57%, alice29: 35.30%
                vec![
                    13, 17, 19, 23, 29, 31, 37, 41
                ]
            } else {
                (1u64..=64).collect()
            }
        }
    }
}

/// Evaluate S-sequence on data, return compressed size or None if roundtrip fails.
fn evaluate_s(data: &[u8], s: &[u64], block_bits: usize) -> Option<usize> {
    let enc = ssp4_rs::ssp5_encode(data, s, block_bits);
    let dec = ssp4_rs::ssp5_decode(&enc);
    if dec != data {
        return None;
    }
    Some(enc.len())
}

/// Calculate ratio percentage.
fn ratio(size: usize, data_len: usize) -> f64 {
    size as f64 / data_len as f64 * 100.0
}

/// Generate standard S-sequence variations for testing.
fn generate_s_variations() -> Vec<(String, Vec<u64>)> {
    let mut sequences = Vec::new();
    
    // 1. Dense ranges
    for n in [16, 32, 64, 96, 128] {
        sequences.push((format!("dense_1-{}", n), (1u64..=n as u64).collect()));
    }
    
    // 2. Prime sequences
    let prime_base: Vec<u64> = vec![
        2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71,
        73, 79, 83, 89, 97, 101, 103, 107, 109, 113, 127, 131
    ];
    sequences.push(("primes_32".to_string(), prime_base.clone()));
    
    // 3. Primes + dense mix (known good combination)
    sequences.push(("primes_dense_mix".to_string(), vec![
        1, 2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
        101, 103, 107, 109, 113, 127
    ]));
    
    // 4. Small dense + primes
    let small_primes: Vec<u64> = vec![
        1, 2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47
    ];
    sequences.push(("small_dense_primes".to_string(), small_primes));
    
    // 5. Fibonacci-based
    let mut fib_s = vec![1, 2];
    while fib_s.len() < 32 {
        let n = fib_s.len();
        fib_s.push(fib_s[n-1] + fib_s[n-2]);
    }
    sequences.push(("fibonacci_32".to_string(), fib_s));
    
    // 6. Powers of 2 based
    let powers: Vec<u64> = (0u64..).map(|e| 1u64 << e).take(16).collect();
    sequences.push(("powers_of_2".to_string(), powers));
    
    // 7. Alternating dense/sparse
    let mut alt = Vec::new();
    for i in 0..32 {
        if i % 2 == 0 {
            alt.push(i as u64 + 1);
        } else {
            alt.push((i as u64 + 1).saturating_pow(2));
        }
    }
    sequences.push(("alternating".to_string(), alt));
    
    // 8. Prime + gaps (best for bible)
    let mut prime_gaps = vec![2, 3, 5, 7, 11, 13];
    for i in 14..50 {
        if i % 2 != 0 && i % 3 != 0 && i % 5 != 0 && i % 7 != 0 && i % 11 != 0 {
            prime_gaps.push(i as u64);
        }
        if prime_gaps.len() >= 32 {
            break;
        }
    }
    sequences.push(("primes_with_gaps".to_string(), prime_gaps));
    
    // 9. Sparse primes (optimized for text: skip 2,3,5,7,11)
    let sparse_primes: Vec<u64> = vec![
        13, 17, 19, 23, 29, 31, 37, 41
    ];
    sequences.push(("sparse_primes_8".to_string(), sparse_primes));
    
    // 10. All odds up to 64
    sequences.push(("odds_1-63".to_string(), (1u64..=63).filter(|x| x % 2 == 1).collect()));
    
    // 11. First N primes
    let first_primes: Vec<u64> = (2u64..).filter(|n| {
        if *n < 2 { false }
        else if *n == 2 { true }
        else if *n % 2 == 0 { false }
        else {
            let mut i = 3;
            while i * i <= *n as u64 {
                if *n % i == 0 { return false; }
                i += 2;
            }
            true
        }
    }).take(48).collect();
    sequences.push(("primes_48".to_string(), first_primes));
    
    sequences
}

/// Find best S-sequence for given data (auto-optimize).
/// Returns (best_name, best_s, best_size, total_time_ms).
fn find_best_s(data: &[u8], block_bits: usize, verbose: bool) -> (String, Vec<u64>, usize, u128) {
    let data_len = data.len();
    let sequences = generate_s_variations();
    
    let t0 = Instant::now();
    let mut results: Vec<_> = sequences.iter()
        .map(|(name, s)| {
            let size = evaluate_s(data, s, block_bits);
            (name.clone(), s.clone(), size)
        })
        .collect();
    
    results.sort_by_key(|r| r.2);
    
    let best = results.first().cloned().unwrap();
    let elapsed = t0.elapsed().as_millis();
    
    if verbose {
        println!("\n=== S-Sequence Optimization Results ===");
        println!("{:<25} {:>10} {:>10}", "Name", "Size", "Ratio%");
        println!("{}", "-".repeat(45));
        for (name, _, size) in &results {
            if let Some(sz) = size {
                println!("{:<25} {:>10} {:>9.2}%", name, sz, ratio(*sz, data_len));
            } else {
                println!("{:<25} {:>10} {:>9}", name, "FAIL", "-");
            }
        }
        println!("\nBest: {} ({} bytes, {:.2}%) in {}ms", 
                 best.0, best.2.unwrap_or(0), ratio(best.2.unwrap_or(0), data_len), elapsed);
    }
    
    (best.0, best.1, best.2.unwrap_or(0), elapsed)
}

/// Run S-sequence optimization on input file.
fn cmd_optimize(input_path: &str, output_path: Option<&str>, block_bits: usize) {
    let data = fs::read(input_path).expect("failed to read input file");
    let data_len = data.len();
    
    println!("=== S-Sequence Optimizer ===");
    println!("File: {} ({} bytes, {:.2} MB)", input_path, data_len, data_len as f64 / 1_048_576.0);
    println!("Block bits: {}", block_bits);
    
    let (best_name, best_s, best_size, _) = find_best_s(&data, block_bits, true);
    
    println!("\n=== BEST S-SEQUENCE ===");
    println!("Name: {}", best_name);
    println!("Size: {} bytes ({:.2}%)", best_size, ratio(best_size, data_len));
    println!("S sequence: {:?}", &best_s[..best_s.len().min(10)]);
    if best_s.len() > 10 {
        println!("... ({} total elements)", best_s.len());
    }
    
    // Save best S sequence with name as comment
    let out_file = output_path.unwrap_or("best_s_sequence.txt");
    let s_str: String = best_s.iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let content = format!("# {}\n{}\n", best_name, s_str);
    fs::write(out_file, &content).expect("failed to write");
    println!("\nSaved to: {}", out_file);
    println!("Use with: ssp5_encode input output --s={}", s_str);
}

fn main() {
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        println!("SSP4/SSP5 Codec CLI v0.4.0");
        println!();
        println!("Usage:");
        println!("  {} ewma7-auto <input> <output>", args[0]);
        println!("  {} ewma7-v18 <input> <output>", args[0]);
        println!("  {} ewma7-decode <input> <output>", args[0]);
        println!("  {} encode <input> <output> [block_bits] [delta] [S]", args[0]);
        println!("  {} decode <input> <output> [S]", args[0]);
        println!("  {} ssp5_encode <input> <output> [block_bits] [--auto | --s=S]", args[0]);
        println!("  {} ssp5_decode <input> <output>", args[0]);
        println!("  {} auto <input> <output> [block_bits]", args[0]);
        println!("  {} optimize <input> [--output <file>] [--block-bits N]", args[0]);
        println!();
        println!("Options:");
        println!("  --auto      Automatically find best S-sequence before encoding");
        println!("  --s=S       Use custom S-sequence (comma-separated)");
        println!("  EWMA7 auto is now DEFAULT for ssp5_encode (18 candidates, v18/v19/v21)");
        println!();
        println!("Examples:");
        println!("  {} ssp5_encode data.txt data.ssp5", args[0]);
        println!("  {} ssp5_encode data.txt data.ssp5 --auto  # auto-optimize S-sequence", args[0]);
        println!("  {} ssp5_encode data.txt data.ssp5 --s=1,2,3,5,7,11", args[0]);
        println!("  {} ssp5_decode data.ssp5 restored.txt", args[0]);
        println!("  {} ewma7-auto data.txt data.ssp5  # EWMA7 auto-tuned", args[0]);
        println!("  {} ewma7-v18 data.txt data.ssp5   # EWMA7 v18 baseline", args[0]);
        println!("  {} auto big_text.txt compressed.ssp5  # alias for ssp5_encode --auto", args[0]);
        println!("  {} optimize big_text.txt --output best_s.txt", args[0]);
        return;
    }

    let cmd = &args[1];
    
    match cmd.as_str() {
        "ewma7-auto" => {
            if args.len() < 4 {
                eprintln!("Usage: {} ewma7-auto <input> <output>", args[0]);
                return;
            }
            let input_path = &args[2];
            let output_path = &args[3];
            
            let data = fs::read(input_path).expect("failed to read input file");
            let data_len = data.len();
            
            println!("EWMA7 auto-tuning (v19 compact header) for {} bytes...", data_len);
            
            let t0 = Instant::now();
            let enc = ssp4_rs::ssp5_encode_with_range_coder_ewma7_auto(&data);
            let elapsed = t0.elapsed();
            
            fs::write(output_path, &enc).expect("failed to write output");
            let ratio_pct = if data_len > 0 {
                100.0 * enc.len() as f64 / data_len as f64
            } else {
                0.0
            };
            println!(
                "Done: {} → {} bytes ({:.2}%) in {:.3}s",
                data_len,
                enc.len(),
                ratio_pct,
                elapsed.as_secs_f64()
            );
        }
        
        "encode" => {
            if args.len() < 4 {
                eprintln!("Usage: {} encode <input> <output> [block_bits] [delta] [S]", args[0]);
                return;
            }
            let input_path = &args[2];
            let output_path = &args[3];
            let block_bits: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(16);
            let delta = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0) != 0;
            let s = parse_s_sequence(args.get(6).map(|s| s.as_str()), false);

            let data = fs::read(input_path).expect("failed to read input file");
            println!("Encoding {} bytes (bb={}, delta={}, |S|={})...", data.len(), block_bits, delta, s.len());

            let t0 = Instant::now();
            let enc = ssp4_rs::encode(&data, &s, block_bits, delta);
            let elapsed = t0.elapsed();

            fs::write(output_path, &enc).expect("failed to write output");
            let ratio_pct = if !data.is_empty() {
                100.0 * enc.len() as f64 / data.len() as f64
            } else {
                0.0
            };
            println!(
                "Done: {} → {} bytes ({:.1}%) in {:.3}s",
                data.len(),
                enc.len(),
                ratio_pct,
                elapsed.as_secs_f64()
            );
        }
        
        "decode" => {
            if args.len() < 4 {
                eprintln!("Usage: {} decode <input> <output> [S]", args[0]);
                return;
            }
            let input_path = &args[2];
            let output_path = &args[3];
            let s = parse_s_sequence(args.get(4).map(|s| s.as_str()), false);

            let data = fs::read(input_path).expect("failed to read input file");
            println!("Decoding {} bytes (|S|={})...", data.len(), s.len());

            let t0 = Instant::now();
            let dec = ssp4_rs::decode(&data, &s).expect("decode failed");
            let elapsed = t0.elapsed();

            fs::write(output_path, &dec).expect("failed to write output");
            println!("Done: {} bytes decoded in {:.3}s", dec.len(), elapsed.as_secs_f64());
        }
        
        "ssp5_encode" => {
            if args.len() < 4 {
                eprintln!("Usage: {} ssp5_encode <input> <output> [block_bits] [--auto | --s=S]", args[0]);
                return;
            }
            let input_path = &args[2];
            let output_path = &args[3];
            
            // Parse remaining args: may include block_bits (number) or flags
            let mut auto_mode = false;
            let mut custom_s: Option<&str> = None;
            let mut block_bits = 16usize;
            let mut use_lz77 = false;
            let mut chunk_size = 0usize;
            let mut bwt_passes = 1usize;
            
            let mut i = 4;
            while i < args.len() {
                let arg = &args[i];
                match arg.as_str() {
                    "--auto" => {
                        auto_mode = true;
                        i += 1;
                    }
                    "--lz77" => {
                        use_lz77 = true;
                        i += 1;
                    }
                    "--s" => {
                        if i + 1 < args.len() {
                            custom_s = Some(&args[i + 1]);
                            i += 2;
                        } else {
                            eprintln!("Error: --s requires a value");
                            return;
                        }
                    }
                    s if s.starts_with("--s=") => {
                        custom_s = Some(&s[4..]);
                        i += 1;
                    }
                    s if s.starts_with("--chunk=") => {
                        chunk_size = s[8..].parse().unwrap_or(0);
                        i += 1;
                    }
                    s if s.starts_with("--passes=") => {
                        bwt_passes = s[9..].parse().unwrap_or(1);
                        i += 1;
                    }
                    _ => {
                        // Check if it's a number (block_bits) or custom S
                        if let Ok(bb) = arg.parse::<usize>() {
                            block_bits = bb;
                        } else if !arg.starts_with("-") {
                            custom_s = Some(arg.as_str());
                        }
                        i += 1;
                    }
                }
            }
            
            let data = fs::read(input_path).expect("failed to read input file");
            let data_len = data.len();
            
            // Determine S sequence
            let (s, s_name) = if auto_mode {
                println!("Auto-optimizing S-sequence for {} bytes...", data_len);
                let (name, s_vec, _, _opt_ms) = find_best_s(&data, block_bits, true);
                println!("\n=== Encoding with best S-sequence ({}) ===", name);
                (s_vec, name)
            } else if let Some(s_str) = custom_s {
                let s_vec = parse_s_sequence(Some(s_str), true);
                println!("Using custom S-sequence: {}", s_str);
                (s_vec, "custom".to_string())
            } else {
                let s_vec = parse_s_sequence(None, true);
                println!("Using default S-sequence (primes_dense_mix)");
                (s_vec, "default".to_string())
            };

            let pipeline_name = if use_lz77 { 
                "LZ77+BWT+MTF+RC".to_string()
            } else if chunk_size > 0 {
                format!("Chunked BWT({})", chunk_size)
            } else if bwt_passes > 1 {
                format!("Iterative BWT({})", bwt_passes)
            } else {
                "BWT+MTF+RC".to_string()
            };
            println!("SSP5 {} encoding {} bytes (bb={}, |S|={}, s_name={})...", 
                     pipeline_name, data_len, block_bits, s.len(), s_name);

            let t0 = Instant::now();
            let enc = if auto_mode {
                ssp4_rs::ssp5_encode_auto(&data, &s, block_bits)
            } else if use_lz77 {
                ssp4_rs::ssp5_encode_with_lz77(&data, &s, block_bits)
            } else if chunk_size > 0 || bwt_passes > 1 {
                ssp4_rs::ssp5_encode_with_options(&data, &s, block_bits, chunk_size, bwt_passes, false)
            } else {
                ssp4_rs::ssp5_encode(&data, &s, block_bits)
            };
            let elapsed = t0.elapsed();

            fs::write(output_path, &enc).expect("failed to write output");
            let ratio_pct = if data_len > 0 {
                100.0 * enc.len() as f64 / data_len as f64
            } else {
                0.0
            };
            println!(
                "Done: {} → {} bytes ({:.2}%) in {:.3}s",
                data_len,
                enc.len(),
                ratio_pct,
                elapsed.as_secs_f64()
            );
        }
        
        "ssp5_decode" => {
            if args.len() < 4 {
                eprintln!("Usage: {} ssp5_decode <input> <output>", args[0]);
                return;
            }
            let input_path = &args[2];
            let output_path = &args[3];

            let data = fs::read(input_path).expect("failed to read input file");
            println!("SSP5 BWT+MTF+RC decoding {} bytes...", data.len());

            let t0 = Instant::now();
            let dec = ssp4_rs::ssp5_decode(&data);
            let elapsed = t0.elapsed();

            fs::write(output_path, &dec).expect("failed to write output");
            println!("Done: {} bytes decoded in {:.3}s", dec.len(), elapsed.as_secs_f64());
        }
        
        "ewma7-decode" => {
            if args.len() < 4 {
                eprintln!("Usage: {} ewma7-decode <input> <output>", args[0]);
                return;
            }
            let input_path = &args[2];
            let output_path = &args[3];

            let data = fs::read(input_path).expect("failed to read input file");
            println!("Decoding EWMA7 auto-tuned archive ({} bytes)...", data.len());

            let t0 = Instant::now();
            let dec = ssp4_rs::ssp5_decode_with_range_coder_ewma7(&data).expect("EWMA7 decode failed");
            let elapsed = t0.elapsed();

            fs::write(output_path, &dec).expect("failed to write output");
            println!("Done: {} bytes decoded in {:.3}s", dec.len(), elapsed.as_secs_f64());
        }

        "ewma7-v18" => {
            if args.len() < 4 {
                eprintln!("Usage: {} ewma7-v18 <input> <output>", args[0]);
                return;
            }
            let input_path = &args[2];
            let output_path = &args[3];

            let data = fs::read(input_path).expect("failed to read input file");
            let data_len = data.len();
            println!("EWMA7 v18 baseline encoding {} bytes...", data_len);

            let t0 = Instant::now();
            let enc = ssp4_rs::ssp5_encode_with_range_coder_ewma7_v18(&data);
            let elapsed = t0.elapsed();

            fs::write(output_path, &enc).expect("failed to write output");
            let ratio_pct = if data_len > 0 { 100.0 * enc.len() as f64 / data_len as f64 } else { 0.0 };
            println!("Done: {} → {} bytes ({:.2}%) in {:.3}s", data_len, enc.len(), ratio_pct, elapsed.as_secs_f64());
        }
        
        "auto" => {
            // Alias for: ssp5_encode with --auto
            if args.len() < 4 {
                eprintln!("Usage: {} auto <input> <output> [block_bits]", args[0]);
                return;
            }
            let input_path = &args[2];
            let output_path = &args[3];
            let block_bits: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(16);
            
            let data = fs::read(input_path).expect("failed to read input file");
            let data_len = data.len();
            
            println!("Auto-optimizing S-sequence for {} bytes...", data_len);
            let (name, s_vec, _, _opt_ms) = find_best_s(&data, block_bits, true);
            println!("\n=== Encoding with best S-sequence ({}) ===", name);
            
            let t0 = Instant::now();
            let enc = ssp4_rs::ssp5_encode(&data, &s_vec, block_bits);
            let elapsed = t0.elapsed();
            
            fs::write(output_path, &enc).expect("failed to write output");
            let ratio_pct = if data_len > 0 {
                100.0 * enc.len() as f64 / data_len as f64
            } else {
                0.0
            };
            println!(
                "Done: {} → {} bytes ({:.2}%) in {:.3}s",
                data_len,
                enc.len(),
                ratio_pct,
                elapsed.as_secs_f64()
            );
        }
        
        "optimize" => {
            if args.len() < 3 {
                eprintln!("Usage: {} optimize <input> [--output <file>] [--block-bits N]", args[0]);
                return;
            }
            let input_path = &args[2];
            
            // Parse optional flags
            let mut output_path: Option<&str> = None;
            let mut block_bits = 16usize;
            
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--output" | "-o" => {
                        if i + 1 < args.len() {
                            output_path = Some(&args[i + 1]);
                            i += 2;
                        } else {
                            eprintln!("Error: --output requires a file argument");
                            return;
                        }
                    }
                    "--block-bits" | "-b" => {
                        if i + 1 < args.len() {
                            block_bits = args[i + 1].parse().unwrap_or(16);
                            i += 2;
                        } else {
                            eprintln!("Error: --block-bits requires a number");
                            return;
                        }
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
            
            cmd_optimize(input_path, output_path, block_bits);
        }
        
        _ => {
            eprintln!("Unknown command: {}. Run without arguments for help.", cmd);
        }
    }
}
