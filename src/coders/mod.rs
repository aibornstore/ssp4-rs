//! SSP4 coders — all compression modules

pub mod bit_io;
pub mod range_coder;
pub mod symbols;
pub mod ssp_codec;

// Re-exports for convenience
pub use bit_io::{BitReader, BitWriter, uleb_decode, uleb_encode};
pub use range_coder::{RangeDecoder, RangeEncoder};
pub use symbols::{decode_symbol, find_symbol, rank_symbol, unrank_symbol, build_lut};
pub use ssp_codec::{encode, decode, SSP5_MAGIC, SSP5_VERSION};
