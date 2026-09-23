# ssp4-rs Changelog

## Unreleased

### Added
- **Range coder pipeline** (`src/coders/range_coder.rs`): Adaptive order-0 range coder for byte streams
  - `range_encode_bytes()` and `range_decode_bytes()` functions
  - Near-optimal compression for byte distributions
- **BWT→MTF→RangeCoder pipeline** (`src/coders/ssp5_pipeline.rs`):
  - `ssp5_encode_with_range_coder()` and `ssp5_decode_with_range_coder()` functions
  - Archive v4 format: [MAGIC(4)][VERSION=4(1)][PRIMARY(4)][MTF_LEN(4)][RC_DATA...]
  - Compression improvement: alice29 62.39%→32.22% (30pp), repetitive 25.40%→5.32% (20pp)
- **Real data compression tests** (`src/coders/real_data_test.rs`): New test module comparing SSP vs RangeCoder pipelines on real data files.

### Fixed
- **OOM in `test_encode_decode_roundtrip`**: Resolved stack buffer overrun (exit code 0xc0000409).

### Changed
- Updated `real_data_test.rs` to compare SSP and RangeCoder pipelines with roundtrip verification.

## Previous (v0.1.0)

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
