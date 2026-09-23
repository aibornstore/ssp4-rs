# ssp4-rs Changelog

## Unreleased

### Added
- **Range coder pipelines** (`src/coders/range_coder.rs`, `src/coders/ssp5_pipeline.rs`):
  - Order-0: `range_encode_bytes()` / `range_decode_bytes()` (v4)
  - Order-1: `range_encode_bytes_order1()` / `range_decode_bytes_order1()` (v5)
  - Order-2: `range_encode_bytes_order2()` / `range_decode_bytes_order2()` (v6)
- **Real data compression tests** comparing all pipeline variants.

### Compression Results (alice29.txt 152KB)
| Pipeline | Ratio | vs SSP |
|----------|-------|--------|
| SSP | 62.39% | baseline |
| RC O0 | 32.22% | 30pp better |
| **RC O1** | **31.63%** | **31pp better** |
| RC O2 | 37.26% | 25pp better |

**Note:** Order-2 underperforms due to 65536 contexts being too sparse for MTF data. Order-1 is optimal.

### Fixed
- **OOM in `test_encode_decode_roundtrip`**: Resolved stack buffer overrun.

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
