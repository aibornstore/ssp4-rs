# ssp4-rs Changelog

## Unreleased

### Added
- **Real data compression tests** (`src/coders/real_data_test.rs`): New test module verifying SSP5 pipeline compression on real data files (alice29, test_100k, test_repetitive). Compression ratios: 62-64% on text data, 25% on repetitive 1000-byte data.
- Integrated `real_data_test` module into `src/coders/mod.rs`.

### Fixed
- **OOM in `test_encode_decode_roundtrip`**: Resolved stack buffer overrun (exit code 0xc0000409) that occurred during `cargo test --lib test_encode_decode_roundtrip`. Test now passes reliably in isolation.

### Changed
- Updated `real_data_test.rs` to use current pipeline API (`ssp5_encode_auto`) instead of deprecated `ssp5_encode_with_huffman` and `rle_encode` functions.

## Previous

### Added
- RLE after MTF before Huffman (bzip2-style) pipeline: BWT→MTF→RLE→Huffman
- All 19 ssp5_pipeline tests passing
- Gap to bzip2 reduced to 1.9pp (15.10% vs 13.2%)
- Huffman v4 canonical code fix (`code += 1` matching between encode/decode)
- LZ77 dictionary improvements (MAX_CHAIN_SIZE 512, 256KB window, lazy matching, 4-byte hash)

### Fixed
- Canonical Huffman code mismatch between encode/decode
- Decode loop rewrite removing incorrect `num_symbols` bound
- Shift overflow on long Huffman codes (u32 → u64)
- Pipeline decode path bugs (wrong data offset 17→14, wrong decoder called)
- MTF decode OOB on empty data from cascading Huffman decode failure
- UNICO ser.rs corrections (BrIf 3 fields, U30TableDecl id, U30BinaryOp names)
- UNICO T64-T68 coverage improvements (runtime.rs, ser.rs, e3.rs, e4_disasm.rs, e4_ser.rs)
