//! ssp4-rs — SSP4 codec in Rust
//!
//! Core SSP compression using special prime sequences.
//! Corresponds to ssp4_local_v44.py encode_data/decode_data core path.

pub mod coders;

// Re-exports
pub use coders::{encode, decode};
pub use coders::bit_io::{BitReader, BitWriter, uleb_decode, uleb_encode};
pub use coders::symbols::{decode_symbol, find_symbol, rank_symbol, unrank_symbol};
pub use coders::range_coder::{RangeDecoder, RangeEncoder};
