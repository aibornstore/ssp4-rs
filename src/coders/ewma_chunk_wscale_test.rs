//! Chunking × (alpha, wscale) interaction test for EWMA7 pipeline.
#[cfg(test)]
mod tests {
    use std::fs;
    use super::super::bwt::{bwt_encode, pack_bwt, unpack_bwt, bwt_decode};
    use super::super::mtf::{mtf_encode, mtf_decode};
    use super::super::range_coder::{range_encode_bytes_order_ewma7_alpha_ws, range_decode_bytes_order_ewma7_alpha_ws};

    #[test]
    fn test_ewma7_chunked_x_wscale_kennedy() {
        let kennedy_path = r"D:\PROJECT UNIVERSE\01Compression\SSP5\tests\comparison_corpora\canterbury\kennedy.xls";
        let data = fs::read(kennedy_path).expect("Cannot read kennedy.xls");
        println!("\n=== EWMA7 chunking × (alpha, wscale) on kennedy.xls ===");
        println!("Input: {} bytes", data.len());

        let configs: [(f64, f64); 4] = [(0.05, 6.0), (0.001, 7.0), (0.05, 100.0), (0.1, 100.0)];
        let chunk_sizes: [usize; 3] = [0, 64 * 1024, 128 * 1024];

        println!("\nchunk \\ (alpha,ws) | {:>12} | {:>12} | {:>12} | {:>12}",
                 "(0.05,6)", "(0.001,7)", "(0.05,100)", "(0.1,100)");

        let mut best: (usize, f64, f64, usize) = (usize::MAX, 0.0, 0.0, 0); // size, alpha, ws, chunk

        for &chunk_size in &chunk_sizes {
            let mut row = if chunk_size == 0 {
                format!("{:>16} |", "none")
            } else {
                format!("{:>11} KB |", chunk_size / 1024)
            };

            for &(alpha, ws) in &configs {
                let mut rc_sum = 0usize;
                let mut num_chunks = 1usize;
                if chunk_size == 0 {
                    let (p, last) = bwt_encode(&data);
                    let m = mtf_encode(&pack_bwt(p, &last));
                    rc_sum = range_encode_bytes_order_ewma7_alpha_ws(&m, alpha, ws).len();
                } else {
                    num_chunks = (data.len() + chunk_size - 1) / chunk_size;
                    for i in 0..num_chunks {
                        let start = i * chunk_size;
                        let end = (start + chunk_size).min(data.len());
                        let (p, last) = bwt_encode(&data[start..end]);
                        let m = mtf_encode(&pack_bwt(p, &last));
                        rc_sum += range_encode_bytes_order_ewma7_alpha_ws(&m, alpha, ws).len();
                    }
                }
                let overhead = if chunk_size == 0 { 0 } else { 17 + 16 * num_chunks };
                let total = overhead + rc_sum;
                row.push_str(&format!(" {:>12}", total));
                if total < best.0 {
                    best = (total, alpha, ws, chunk_size);
                }
            }
            println!("{}", row);
        }

        let best_ratio = 100.0 * best.0 as f64 / data.len() as f64;
        let chunk_desc = if best.3 == 0 { "none".to_string() } else { format!("{}KB", best.3 / 1024) };
        println!("\nBest: {} bytes ({:.2}%) at alpha={}, wscale={}, chunk={}",
                 best.0, best_ratio, best.1, best.2, chunk_desc);

        // Roundtrip best config
        let (alpha, ws, chunk_size) = (best.1, best.2, best.3);
        let mut decoded = Vec::with_capacity(data.len());
        if chunk_size == 0 {
            let (p, last) = bwt_encode(&data);
            let m = mtf_encode(&pack_bwt(p, &last));
            let rc = range_encode_bytes_order_ewma7_alpha_ws(&m, alpha, ws);
            let m_back = range_decode_bytes_order_ewma7_alpha_ws(&rc, alpha, ws).expect("decode failed");
            assert_eq!(m_back, m, "MTF mismatch");
            let packed_back = mtf_decode(&m_back);
            let (pb, lb) = unpack_bwt(&packed_back);
            decoded = bwt_decode(pb, lb);
        } else {
            let num_chunks = (data.len() + chunk_size - 1) / chunk_size;
            for i in 0..num_chunks {
                let start = i * chunk_size;
                let end = (start + chunk_size).min(data.len());
                let (p, last) = bwt_encode(&data[start..end]);
                let m = mtf_encode(&pack_bwt(p, &last));
                let rc = range_encode_bytes_order_ewma7_alpha_ws(&m, alpha, ws);
                let m_back = range_decode_bytes_order_ewma7_alpha_ws(&rc, alpha, ws).expect("decode failed");
                assert_eq!(m_back, m, "MTF mismatch at chunk {}", i);
                let packed_back = mtf_decode(&m_back);
                let (pb, lb) = unpack_bwt(&packed_back);
                decoded.extend_from_slice(&bwt_decode(pb, lb));
            }
        }
        assert_eq!(decoded, data, "Roundtrip failed");
        println!("Roundtrip (alpha={}, wscale={}, chunk={}): OK", alpha, ws, chunk_desc);
    }

    #[test]
    fn test_ewma7_auto_corpus_all_files() {
        use super::super::ssp5_pipeline::{ssp5_encode_with_range_coder_ewma7_auto,
                                          ssp5_decode_with_range_coder_ewma7,
                                          ssp5_encode_with_range_coder_ewma7};

        let corpus_dir = r"D:\PROJECT UNIVERSE\01Compression\SSP5\tests\comparison_corpora\canterbury";
        let files = ["alice29.txt", "cp.html", "fields.c", "kennedy.xls"];

        println!("\n=== EWMA7 auto vs default across Canterbury corpus ===");
        let mut total_auto = 0usize;
        let mut total_def = 0usize;
        let mut total_in = 0usize;

        for name in &files {
            let path = format!(r"{}\{}", corpus_dir, name);
            let data = match fs::read(&path) {
                Ok(d) => d,
                Err(_) => { println!("{}: SKIPPED (not found)", name); continue; }
            };

            let auto = ssp5_encode_with_range_coder_ewma7_auto(&data);
            let default = ssp5_encode_with_range_coder_ewma7(&data);

            let auto_ratio = 100.0 * auto.len() as f64 / data.len() as f64;
            let def_ratio = 100.0 * default.len() as f64 / data.len() as f64;
            println!("{:>12} ({} B): auto {:>7} ({:6.2}%) | default {:>7} ({:6.2}%) | {:+.2}pp",
                     name, data.len(), auto.len(), auto_ratio, default.len(), def_ratio,
                     def_ratio - auto_ratio);

            let decoded = ssp5_decode_with_range_coder_ewma7(&auto).expect("auto decode failed");
            assert_eq!(decoded, data, "{} roundtrip failed", name);

            total_auto += auto.len();
            total_def += default.len();
            total_in += data.len();
        }

        println!("TOTAL ({} B): auto {} ({:.2}%) | default {} ({:.2}%) | {:+.2}pp",
                 total_in, total_auto, 100.0 * total_auto as f64 / total_in as f64,
                 total_def, 100.0 * total_def as f64 / total_in as f64,
                 100.0 * (total_def - total_auto) as f64 / total_in as f64);
    }
}
