//! ssp4-rs — SSP4/SSP5 codec in Rust
//!
//! Core SSP compression using special prime sequences.
//! SSP5 adds BWT + MTF preprocessing for better text compression.

pub mod coders;

// Re-exports
pub use coders::{encode, decode};
pub use coders::bit_io::{BitReader, BitWriter, uleb_decode, uleb_encode};
pub use coders::symbols::{decode_symbol, find_symbol, rank_symbol, unrank_symbol};
pub use coders::range_coder::{RangeDecoder, RangeEncoder};
pub use coders::bwt::{bwt_encode, bwt_decode, pack_bwt, unpack_bwt};
pub use coders::mtf::{mtf_encode, mtf_decode, pack_mtf, unpack_mtf};
pub use coders::ssp5_pipeline::{
    ssp5_encode, ssp5_decode, ssp5_encode_with_lz77, ssp5_encode_auto,
    ssp5_encode_with_options,
    ssp5_encode_with_range_coder_ewma7, ssp5_decode_with_range_coder_ewma7,
    ssp5_encode_with_range_coder_ewma7_alpha, ssp5_encode_with_range_coder_ewma7_auto,
    ssp5_encode_with_range_coder_ewma7_v18,
};
pub use coders::lz77::{encode as lz77_encode, decode as lz77_decode};
