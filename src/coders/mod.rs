//! SSP4 coders — all compression modules

pub mod bit_io;
pub mod range_coder;
pub mod symbols;
pub mod ssp_codec;
pub mod bwt;
pub mod mtf;
pub mod ssp5_pipeline;
pub mod lz77;
pub mod real_data_test;

// Re-exports for convenience
pub use bit_io::{BitReader, BitWriter, uleb_decode, uleb_encode};
pub use range_coder::{RangeDecoder, RangeEncoder, range_encode_bytes, range_decode_bytes, range_encode_bytes_order1, range_decode_bytes_order1, range_encode_bytes_order2, range_decode_bytes_order2, range_encode_bytes_order_mix, range_decode_bytes_order_mix, range_encode_bytes_order12_mix, range_decode_bytes_order12_mix, range_encode_bytes_order_ewma, range_decode_bytes_order_ewma, range_encode_bytes_order_ewma3, range_decode_bytes_order_ewma3, range_encode_bytes_order_ewma5, range_decode_bytes_order_ewma5, range_encode_bytes_order_ewma7, range_decode_bytes_order_ewma7};
pub use symbols::{decode_symbol, find_symbol, rank_symbol, unrank_symbol, build_lut};
pub use ssp_codec::{encode, decode, SSP5_MAGIC, SSP5_VERSION};
pub use bwt::{bwt_encode, bwt_decode, pack_bwt, unpack_bwt};
pub use mtf::{mtf_encode, mtf_decode, pack_mtf, unpack_mtf};
pub use lz77::{encode as lz77_encode, decode as lz77_decode, tokens_to_bytes, bytes_to_tokens};
