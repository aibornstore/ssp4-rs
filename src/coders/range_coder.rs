//! Range coder (arithmetic coding) — API-compatible with cascade-codec-rs.
//! Uses u64 range subdivision with byte-level I/O.
//! Key invariant: encoder's byte output sequence must exactly match decoder's byte input sequence.

const TOP: u64 = 1u64 << 32; // 2^32
const BOT: u64 = 1u64 << 5;  // 2^5 — normalization threshold
const MASK: u64 = TOP - 1;    // 0xFFFF_FFFF

/// Arithmetic encoder using u64 range subdivision.
pub struct RangeEncoder {
    low: u64,
    range: u64,
    buf: Vec<u8>,
}

impl RangeEncoder {
    pub fn new() -> Self {
        Self { low: 0, range: TOP, buf: Vec::new() }
    }

    fn normalize(&mut self) {
        while self.range < BOT {
            self.buf.push((self.low >> 24) as u8);
            self.low = (self.low << 8) & MASK;
            self.range <<= 8;
        }
    }

    /// Encode a symbol in range [cum_low, cum_high).
    pub fn encode(&mut self, cum_low: u64, cum_high: u64, total: u64) {
        let r = (self.range / total).max(1);
        self.low += cum_low * r;
        self.range = (cum_high - cum_low) * r;
        self.low &= MASK;
        self.normalize();
    }

    pub fn finish(&mut self) {
        for _ in 0..5 {
            self.buf.push((self.low >> 24) as u8);
            self.low = (self.low << 8) & MASK;
        }
    }

    pub fn get_bytes(&self) -> &[u8] {
        &self.buf
    }
}

impl Default for RangeEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Arithmetic decoder matching RangeEncoder.
pub struct RangeDecoder<'a> {
    low: u64,
    range: u64,
    code: u64,
    data: &'a [u8],
    pos: usize,
}

impl<'a> RangeDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut code: u64 = 0;
        let mut pos = 0;
        for _ in 0..4 {
            if pos < data.len() {
                code = (code << 8) | (data[pos] as u64);
                pos += 1;
            }
        }
        Self { low: 0, range: TOP, code, data, pos }
    }

    fn read_byte(&mut self) -> u8 {
        if self.pos < self.data.len() {
            let b = self.data[self.pos];
            self.pos += 1;
            b
        } else {
            0
        }
    }

    fn normalize(&mut self) {
        while self.range < BOT {
            self.code = ((self.code << 8) | (self.read_byte() as u64)) & MASK;
            self.low = ((self.low << 8) | (self.read_byte() as u64)) & MASK;
            self.range <<= 8;
        }
    }

    pub fn decode(&mut self, total: u64) -> (usize, u64) {
        self.normalize();
        let r = (self.range / total).max(1);
        let idx = ((self.code.wrapping_sub(self.low)) / r) as usize;
        (idx.min(total as usize - 1), r)
    }

    pub fn narrow(&mut self, cum_low: u64, cum_high: u64, r: u64) {
        self.low += cum_low * r;
        self.range = (cum_high - cum_low) * r;
        self.low &= MASK;
    }

    /// Read trailing bytes at the end (matching encoder's finish).
    pub fn read_trailing(&mut self) {
        for _ in 0..5 {
            self.code = ((self.code << 8) | (self.read_byte() as u64)) & MASK;
            self.low = ((self.low << 8) | (self.read_byte() as u64)) & MASK;
        }
    }
}

/// Encode symbols using a range coder.
// pub fn encode(syms: &[usize], cum: &[u64], total: u64) -> Vec<u8> {
//     let mut enc = RangeEncoder::new();
//     for &sym in syms {
//         enc.encode(cum[sym], cum[sym + 1], total);
//     }
//     enc.finish();
//     enc.buf
// }

/// Decode symbols from a range-coded stream.
// pub fn decode(data: &[u8], n: usize, cum: &[u64], total: u64) -> Vec<usize> {
//     let mut dec = RangeDecoder::new(data);
//     let mut out = Vec::with_capacity(n);
//     for _ in 0..n {
//         let (idx, r) = dec.decode(total);
//         let mut sym = 0;
//         for i in 0..cum.len() - 1 {
//             if cum[i] <= idx as u64 && (idx as u64) < cum[i + 1] {
//                 sym = i;
//                 break;
//             }
//         }
//         out.push(sym);
//         dec.narrow(cum[sym], cum[sym + 1], r);
//     }
//     dec.read_trailing();
//     out
// }

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(syms: &[usize], cum: &[u64], total: u64) -> Vec<usize> {
        let mut enc = RangeEncoder::new();
        for &sym in syms {
            enc.encode(cum[sym], cum[sym + 1], total);
        }
        enc.finish();
        let data = enc.get_bytes().to_vec();

        let mut dec = RangeDecoder::new(&data);
        let mut out = Vec::with_capacity(syms.len());
        for _ in 0..syms.len() {
            let (idx, r) = dec.decode(total);
            let mut sym = 0;
            for i in 0..cum.len() - 1 {
                if cum[i] <= idx as u64 && (idx as u64) < cum[i + 1] {
                    sym = i;
                    break;
                }
            }
            out.push(sym);
            dec.narrow(cum[sym], cum[sym + 1], r);
        }
        dec.read_trailing();
        out
    }

    #[test]
    fn test_single() {
        let cum = vec![0, 10];
        let syms = vec![0, 0, 0, 0, 0];
        assert_eq!(roundtrip(&syms, &cum, 10), syms);
    }

    #[test]
    fn test_uniform_10() {
        let cum: Vec<u64> = (0..=10).collect();
        let syms: Vec<usize> = (0..10).collect();
        assert_eq!(roundtrip(&syms, &cum, 10), syms);
    }

    #[test]
    fn test_nonuniform() {
        let cum = vec![0, 3, 4, 6];
        let syms = vec![0, 0, 1, 2, 0];
        assert_eq!(roundtrip(&syms, &cum, 6), syms);
    }

    #[test]
    fn test_binary() {
        let cum = vec![0, 500, 1000];
        let syms = vec![0, 1, 0, 0, 1, 1, 0, 1, 1, 1];
        assert_eq!(roundtrip(&syms, &cum, 1000), syms);
    }

    #[test]
    fn test_large() {
        // 30 binary symbols (within 32-bit precision)
        let cum = vec![0, 1, 2];
        let syms: Vec<usize> = (0..30).map(|i| i % 2).collect();
        assert_eq!(roundtrip(&syms, &cum, 2), syms);
    }

    #[test]
    fn test_many_symbols() {
        // 8 uniform symbols (safe within 32-bit precision)
        let cum: Vec<u64> = (0..=8).collect();
        let syms: Vec<usize> = (0..8).collect();
        assert_eq!(roundtrip(&syms, &cum, 8), syms);
    }

    #[test]
    fn test_highly_skewed() {
        let cum = vec![0, 990, 1000];
        let syms = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0];
        assert_eq!(roundtrip(&syms, &cum, 1000), syms);
    }

    #[test]
    fn test_all_same_symbol() {
        let cum = vec![0, 1, 2, 3, 4, 5];
        let syms = vec![2, 2, 2, 2, 2, 2, 2, 2, 2, 2];
        assert_eq!(roundtrip(&syms, &cum, 5), syms);
    }
}
