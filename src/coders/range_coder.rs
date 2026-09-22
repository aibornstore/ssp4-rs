//! Range coder (arithmetic coding) — bit-level, matching Python ssp4_local_v44.py.
//! Uses 64-bit arithmetic internally, PREC=32 bits precision.

const PREC: u64 = 32;
const HALF: u64 = 1u64 << (PREC - 1);   // 2^31
const QUARTER: u64 = 1u64 << (PREC - 2); // 2^30
const THREE_QUARTERS: u64 = 3 * QUARTER;  // 3 * 2^30

/// Bit-level range encoder (matches Python RangeEncoder).
pub struct RangeEncoder {
    lo: u64,
    hi: u64,
    pending: u64,
    bits: Vec<u8>,
}

impl RangeEncoder {
    pub fn new() -> Self {
        Self {
            lo: 0,
            hi: (1u64 << PREC) - 1,
            pending: 0,
            bits: Vec::new(),
        }
    }

    fn output_bit(&mut self, bit: u8) {
        self.bits.push(bit);
        for _ in 0..self.pending {
            self.bits.push(1 - bit);
        }
        self.pending = 0;
    }

    /// Encode symbol with frequency range [cum_freq, cum_freq + freq).
    /// cum_freq and freq are in [0, total), total is the scale.
    pub fn encode(&mut self, cum_freq: u64, freq: u64, total: u64) {
        let rng = self.hi - self.lo + 1;
        self.hi = self.lo + (rng * (cum_freq + freq)) / total - 1;
        self.lo = self.lo + (rng * cum_freq) / total;

        loop {
            if self.hi < HALF {
                self.output_bit(0);
            } else if self.lo >= HALF {
                self.output_bit(1);
                self.lo -= HALF;
                self.hi -= HALF;
            } else if self.lo >= QUARTER && self.hi < THREE_QUARTERS {
                self.pending += 1;
                self.lo -= QUARTER;
                self.hi -= QUARTER;
            } else {
                break;
            }
            self.lo <<= 1;
            self.hi = (self.hi << 1) | 1;
        }
    }

    /// Finish encoding and return packed bytes (matches Python RangeEncoder.flush).
    pub fn flush(&mut self) -> Vec<u8> {
        self.pending += 1;
        if self.lo < QUARTER {
            self.output_bit(0);
        } else {
            self.output_bit(1);
        }

        // Pack bits into bytes (MSB first)
        let mut out = Vec::new();
        for i in (0..self.bits.len()).step_by(8) {
            let mut byte = 0u8;
            for j in 0..8 {
                if i + j < self.bits.len() {
                    byte = (byte << 1) | self.bits[i + j];
                } else {
                    byte <<= 1;
                }
            }
            out.push(byte);
        }
        out
    }
}

impl Default for RangeEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Bit-level range decoder (matches Python RangeDecoder).
pub struct RangeDecoder<'a> {
    bits: Vec<u8>,
    pos: usize,
    lo: u64,
    hi: u64,
    code: u64,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> RangeDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        // Unpack bytes to bits (MSB first, matching Python)
        let mut bits = Vec::with_capacity(data.len() * 8);
        for &b in data {
            for i in (0..8u8).rev() {
                bits.push(((b >> i) & 1) as u8);
            }
        }

        let lo = 0;
        let hi = (1u64 << PREC) - 1;
        let mut code = 0u64;
        let mut pos = 0usize;
        for _ in 0..PREC {
            code = (code << 1) | (bits.get(pos).copied().unwrap_or(0) as u64);
            pos += 1;
        }

        Self { bits, pos, lo, hi, code, _marker: std::marker::PhantomData }
    }

    fn read_bit(&mut self) -> u8 {
        let b = self.bits.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    /// Get frequency bucket for current code position.
    pub fn get_freq(&self, total: u64) -> u64 {
        let rng = self.hi - self.lo + 1;
        let cum = ((self.code.wrapping_sub(self.lo) + 1) * total - 1) / rng;
        cum.min(total - 1)
    }

    /// Decode and narrow interval.
    pub fn decode(&mut self, cum_freq: u64, freq: u64, total: u64) {
        let rng = self.hi - self.lo + 1;
        self.hi = self.lo + (rng * (cum_freq + freq)) / total - 1;
        self.lo = self.lo + (rng * cum_freq) / total;

        loop {
            if self.hi < HALF {
                // nothing
            } else if self.lo >= HALF {
                self.code -= HALF;
                self.lo -= HALF;
                self.hi -= HALF;
            } else if self.lo >= QUARTER && self.hi < THREE_QUARTERS {
                self.code -= QUARTER;
                self.lo -= QUARTER;
                self.hi -= QUARTER;
            } else {
                break;
            }
            self.lo <<= 1;
            self.hi = (self.hi << 1) | 1;
            self.code = (self.code << 1) | (self.read_bit() as u64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(syms: &[usize], cum: &[u64], total: u64) -> Vec<usize> {
        let mut enc = RangeEncoder::new();
        for &sym in syms {
            enc.encode(cum[sym], cum[sym + 1] - cum[sym], total);
        }
        let data = enc.flush();

        let mut dec = RangeDecoder::new(&data);
        let mut out = Vec::with_capacity(syms.len());
        for _ in 0..syms.len() {
            let f = dec.get_freq(total);
            let mut sym = 0usize;
            for i in 0..cum.len() - 1 {
                if cum[i] <= f && f < cum[i + 1] {
                    sym = i;
                    break;
                }
            }
            out.push(sym);
            dec.decode(cum[sym], cum[sym + 1] - cum[sym], total);
        }
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
        let cum = vec![0, 1, 2];
        let syms: Vec<usize> = (0..30).map(|i| i % 2).collect();
        assert_eq!(roundtrip(&syms, &cum, 2), syms);
    }

    #[test]
    fn test_many_symbols() {
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
