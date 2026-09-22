//! BitWriter / BitReader — MSB-first bit packing for SSP stream encoding
//! Corresponds to Python BitWriter/BitReader in ssp4_local_v44.py (lines 846-933)

/// Pack bits into a Vec<u8>, MSB-first within each byte.
/// Python-style: `byte: u8` accumulator + `buf: Vec<u8>` + `bit_pos: u8`.
/// Flush at bit_pos == 8.
#[derive(Default)]
pub struct BitWriter {
    buf: Vec<u8>,     // completed bytes
    byte: u8,         // current byte accumulator (written MSB-first)
    bit_pos: u8,      // bits filled in current byte (0..8), flush when 8
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a single bit (MSB-first within each byte).
    pub(crate) fn write_bit(&mut self, b: u8) {
        self.byte = (self.byte << 1) | (b & 1);
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.buf.push(self.byte);
            self.byte = 0;
            self.bit_pos = 0;
        }
    }

    /// Write `count` bits from `value` (MSB-first).
    /// When at byte boundary (bit_pos=0) and count >= 8, writes complete bytes directly.
    pub fn write_bits(&mut self, value: u64, mut count: usize) {
        // Byte-aligned fast path: write complete bytes directly
        if self.bit_pos == 0 && count >= 8 {
            while count >= 8 {
                self.buf.push((value >> (count - 8)) as u8);
                count -= 8;
            }
        }
        // Per-bit loop for remaining bits.
        // remaining tracks bits left to write; initialized to current count (post-fast-path).
        let mut remaining = count;
        for _ in 0..remaining {
            let bit = ((value >> (remaining - 1)) & 1) as u8;
            self.write_bit(bit);
            remaining -= 1;
        }
    }

    /// Write a ULEB128-encoded integer.
    /// Algorithm: each byte stores 7 data bits (x & 0x7F) with bit 7=1 (more),
    /// last byte has bit 7=0. Flushes pending bits first to byte-align.
    pub fn write_uleb(&mut self, mut x: u64) {
        // Flush any pending partial bits first (to byte-align before writing ULEB bytes)
        if self.bit_pos > 0 {
            self.buf.push(self.byte << (8 - self.bit_pos));
            self.byte = 0;
            self.bit_pos = 0;
        }
        // Write ULEB bytes directly to buf (now byte-aligned)
        if x < 0x80 {
            self.buf.push(x as u8);
            return;
        }
        while x >= 0x80 {
            self.buf.push(((x & 0x7F) as u8) | 0x80);
            x >>= 7;
        }
        self.buf.push(x as u8);
    }

    /// Write a Rice-coded value with parameter k (0 ≤ k ≤ 31).
    /// Format: unary quotient (q ones + 0), then k bits of remainder.
    pub fn write_rice(&mut self, value: u64, k: usize) {
        let mut q = value >> k;
        while q > 0 {
            self.write_bit(1);
            q -= 1;
        }
        self.write_bit(0);
        if k > 0 {
            self.write_bits(value & ((1u64 << k) - 1), k);
        }
    }

    /// Flush partial byte and return owned bytes.
    pub fn flush(mut self) -> Vec<u8> {
        if self.bit_pos > 0 {
            self.buf.push(self.byte << (8 - self.bit_pos));
        }
        self.buf
    }

    /// Flush any pending partial byte (for byte-aligned encoding between blocks).
    #[allow(dead_code)]
    pub(crate) fn flush_partial(&mut self) {
        if self.bit_pos > 0 {
            self.buf.push(self.byte << (8 - self.bit_pos));
            self.byte = 0;
            self.bit_pos = 0;
        }
    }
}

/// Read bits from bytes, MSB-first within each byte.
pub struct BitReader<'a> {
    data: &'a [u8],
    pub(crate) byte_pos: usize,
    pub(crate) bit_pos: u8, // next bit to read within current byte (0..7)
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    pub(crate) fn read_bit(&mut self) -> Result<u8, &'static str> {
        if self.byte_pos >= self.data.len() {
            return Err("BitReader: unexpected end of data");
        }
        let b = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
        Ok(b)
    }

    /// Read one full byte, advancing past it.
    pub fn read_byte(&mut self) -> Result<u8, &'static str> {
        if self.byte_pos >= self.data.len() {
            return Err("BitReader: unexpected end of data");
        }
        if self.bit_pos == 0 {
            let b = self.data[self.byte_pos];
            self.byte_pos += 1;
            return Ok(b);
        }
        // Non-byte-aligned: read 8 bits via per-bit loop
        let mut value = 0u64;
        for _ in 0..8 {
            value = (value << 1) | self.read_bit()? as u64;
        }
        Ok(value as u8)
    }

    /// Read `count` bits and return as integer (MSB-first).
    pub fn read_bits(&mut self, count: usize) -> Result<u64, &'static str> {
        // For byte-aligned reads: read complete bytes directly
        if self.bit_pos == 0 && (count & 7) == 0 {
            let nbytes = count >> 3;
            let end = self.byte_pos + nbytes;
            if end <= self.data.len() {
                let mut value = 0u64;
                for &b in &self.data[self.byte_pos..end] {
                    value = (value << 8) | b as u64;
                }
                self.byte_pos = end;
                return Ok(value);
            }
        }
        // Non-byte-aligned: read remaining bits of current byte, then full bytes.
        let mut bits_read = 0usize;
        let mut value = 0u64;

        // 1. Remaining bits in current byte (to reach next byte boundary)
        if self.bit_pos > 0 {
            let bits_to_byte_boundary = 8 - self.bit_pos as usize;
            for _ in 0..bits_to_byte_boundary {
                if bits_read >= count || self.byte_pos >= self.data.len() { break; }
                let b = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
                self.bit_pos += 1;
                if self.bit_pos == 8 {
                    self.bit_pos = 0;
                    self.byte_pos += 1;
                }
                value = (value << 1) | b as u64;
                bits_read += 1;
            }
        }

        // 2. Full bytes (read 8 bits at a time)
        while bits_read + 8 <= count && self.byte_pos < self.data.len() {
            let b = self.data[self.byte_pos];
            self.byte_pos += 1;
            value = (value << 8) | b as u64;
            bits_read += 8;
        }

        // 3. Remaining bits (less than 8, per-bit loop)
        while bits_read < count && self.byte_pos < self.data.len() {
            let b = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
            value = (value << 1) | b as u64;
            bits_read += 1;
        }

        Ok(value)
    }

    /// Read remaining bits in current byte, then advance to the next byte boundary.
    /// For SYM (tag=0) and ZERO-RLE (tag=3), the ULEB payload needs byte alignment.
    /// For RAW (tag=1) and ZERO (tag=2), payload starts at current bit_pos — NO skip.
    pub(crate) fn skip_to_byte_boundary(&mut self) {
        if self.bit_pos != 0 && self.byte_pos < self.data.len() {
            let remaining = 8 - self.bit_pos;
            for _ in 0..remaining {
                self.bit_pos += 1;
                if self.bit_pos == 8 {
                    self.bit_pos = 0;
                    self.byte_pos += 1;
                }
            }
        }
    }

    /// Read a ULEB128-encoded integer (7 bits per byte, MSB = continuation).
    /// Skips to the next byte boundary first, then reads raw bytes.
    pub fn read_uleb(&mut self) -> Result<u64, &'static str> {
        // Skip to next byte boundary: consume remaining bits of current byte
        if self.bit_pos != 0 && self.byte_pos < self.data.len() {
            self.byte_pos += 1;
            self.bit_pos = 0;
        }
        // Now read raw bytes (ULEB is byte-aligned)
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            if self.byte_pos >= self.data.len() {
                return Err("ULEB read past end of data");
            }
            let byte = self.data[self.byte_pos];
            self.byte_pos += 1;
            result |= ((byte & 0x7F) as u64) << shift;
            if (byte & 0x80) == 0 {
                break;
            }
            shift += 7;
        }
        Ok(result)
    }

    /// Read a Rice-coded value with parameter k (0 ≤ k ≤ 31).
    /// Format: unary quotient (q ones + 0), then k bits of remainder.
    pub fn read_rice(&mut self, k: usize) -> Result<u64, &'static str> {
        let mut q = 0u64;
        loop {
            let b = self.read_bit()?;
            if b == 1 {
                q += 1;
            } else {
                break;
            }
        }
        let r = if k > 0 { self.read_bits(k)? } else { 0 };
        Ok((q << k) | r)
    }

    /// Returns true if all data has been consumed.
    pub fn is_exhausted(&self) -> bool {
        self.byte_pos >= self.data.len()
    }
}

/// Encode a ULEB128 value.
pub fn uleb_encode(x: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = x;
    while v >= 0x80 {
        out.push(((v & 0x7F) | 0x80) as u8);
        v >>= 7;
    }
    out.push((v & 0x7F) as u8);
    out
}

/// Decode a ULEB128 value, returning (value, bytes_consumed).
pub fn uleb_decode(data: &[u8]) -> Result<(u64, usize), &'static str> {
    let mut result = 0u64;
    let mut shift = 0;
    let mut pos = 0;
    while pos < data.len() {
        let b = data[pos];
        pos += 1;
        result |= ((b & 0x7F) as u64) << shift;
        if (b & 0x80) == 0 {
            return Ok((result, pos));
        }
        shift += 7;
    }
    Err("ULEB: incomplete byte sequence")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bit_writer_roundtrip() {
        let mut bw = BitWriter::new();
        bw.write_bits(0b10110101, 8);
        bw.write_bit(1);
        bw.write_bits(0b11, 2);
        let data = bw.flush();
        assert!(!data.is_empty(), "flush returned empty data!");
        let mut br = BitReader::new(&data);
        assert_eq!(br.read_bits(8).unwrap(), 0b10110101);
        assert_eq!(br.read_bit().unwrap(), 1);
        assert_eq!(br.read_bits(2).unwrap(), 0b11);
    }

    #[test]
    fn test_uleb_roundtrip() {
        for x in [0u64, 1, 127, 128, 300, 10000, u64::MAX] {
            let enc = uleb_encode(x);
            let (dec, n) = uleb_decode(&enc).unwrap();
            assert_eq!(dec, x);
            assert_eq!(n, enc.len());
        }
    }

    #[test]
    fn test_bit_reader_uleb() {
        let mut bw = BitWriter::new();
        bw.write_uleb(300);
        bw.write_uleb(10000);
        let data = bw.flush();

        let mut br = BitReader::new(&data);
        assert_eq!(br.read_uleb().unwrap(), 300);
        assert_eq!(br.read_uleb().unwrap(), 10000);
    }

    #[test]
    fn test_rice_roundtrip() {
        for k in 0..=16 {
            for val in [0u64, 1, 5, 10, 100, 1000, 65535] {
                let mut bw = BitWriter::new();
                bw.write_rice(val, k);
                let data = bw.flush();
                let mut br = BitReader::new(&data);
                let dec = br.read_rice(k).unwrap();
                assert_eq!(dec, val, "k={} val={}", k, val);
            }
        }
    }
}
