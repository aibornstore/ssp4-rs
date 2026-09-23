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

/// Adaptive order-0 range encoder for byte data.
/// Uses uniform initial frequencies (all 256 symbols), then adapts after each symbol.
pub fn range_encode_bytes(data: &[u8]) -> Vec<u8> {
    let mut enc = RangeEncoder::new();
    let mut freqs = [1u64; 256];
    let mut total: u64 = 256;
    
    for &b in data {
        let sym = b as usize;
        let mut cum = 0u64;
        for i in 0..sym {
            cum += freqs[i];
        }
        let freq = freqs[sym];
        enc.encode(cum, freq, total);
        freqs[sym] += 1;
        total += 1;
    }
    
    let mut out = Vec::new();
    // Write symbol count as ULEB first (allows decoder to know expected length)
    let mut v = data.len() as u64;
    while v >= 0x80 {
        out.push(((v & 0x7F) | 0x80) as u8);
        v >>= 7;
    }
    out.push((v & 0x7F) as u8);
    out.extend(enc.flush());
    out
}

/// Adaptive order-0 range decoder for byte data.
/// Returns error if data is truncated or corrupted.
pub fn range_decode_bytes(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    
    // Read symbol count from ULEB prefix
    let mut pos = 0;
    let mut count: u64 = 0;
    let mut shift = 0u32;
    loop {
        if pos >= data.len() {
            return Err("Range decode: truncated ULEB count");
        }
        let b = data[pos];
        pos += 1;
        count |= ((b & 0x7F) as u64) << shift;
        if (b & 0x80) == 0 {
            break;
        }
        shift += 7;
    }
    
    let range_data = &data[pos..];
    let mut dec = RangeDecoder::new(range_data);
    let mut out = Vec::with_capacity(count as usize);
    
    let mut freqs = [1u64; 256];
    let mut total: u64 = 256;
    
    for _ in 0..count {
        let f = dec.get_freq(total);
        
        // Find symbol by cumulative frequency lookup
        let mut cum = 0u64;
        let mut sym = 0u8;
        for i in 0..256 {
            cum += freqs[i];
            if f < cum {
                sym = i as u8;
                break;
            }
        }
        
        let freq = freqs[sym as usize];
        dec.decode(cum - freq, freq, total);
        freqs[sym as usize] += 1;
        total += 1;
        out.push(sym);
    }
    
    Ok(out)
}

/// Adaptive order-1 context model for byte data.
/// Uses 256 contexts (one per previous byte), each with 256 symbol frequencies.
/// Context 0 is used for the first byte (no previous context).
const CTX_SIZE: usize = 256;
const SYM_SIZE: usize = 256;
const CONTEXT_ORDER1: usize = CTX_SIZE * SYM_SIZE; // 65536 entries

/// Adaptive order-1 range encoder for byte data.
/// Uses context=previous_byte for encoding, providing better compression for text.
pub fn range_encode_bytes_order1(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        let mut out = Vec::new();
        out.push(1); // flag: order-1
        out.push(0); out.push(0); out.push(0); out.push(0); // length = 0 (4 bytes LE)
        return out;
    }
    
    let mut enc = RangeEncoder::new();
    
    // Order-1 context tables: [context][symbol] = frequency
    // Initialize with small non-zero values for smoothing
    let mut ctx: [[u64; SYM_SIZE]; CTX_SIZE] = [[1u64; SYM_SIZE]; CTX_SIZE];
    let mut ctx_totals: [u64; CTX_SIZE] = [256u64; CTX_SIZE]; // total per context
    
    let mut prev = 0u8; // context for first byte
    
    for &b in data {
        let sym = b as usize;
        let ctx_idx = prev as usize;
        
        // Get frequencies for this context
        let total = ctx_totals[ctx_idx];
        
        // Encode symbol using context-specific frequencies
        let mut cum = 0u64;
        for i in 0..sym {
            cum += ctx[ctx_idx][i];
        }
        let freq = ctx[ctx_idx][sym];
        enc.encode(cum, freq, total);
        
        // Update context model
        ctx[ctx_idx][sym] += 1;
        ctx_totals[ctx_idx] += 1;
        
        prev = b; // current becomes previous for next iteration
    }
    
    let mut out = Vec::new();
    out.push(1); // flag: order-1
    
    // Write symbol count as 4-byte little-endian
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_le_bytes());
    
    out.extend(enc.flush());
    out
}

/// Adaptive order-1 range decoder for byte data.
pub fn range_decode_bytes_order1(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 5 {
        return Err("Range decode: data too short");
    }
    
    let flag = data[0];
    if flag != 1 {
        return Err("Range decode: not order-1 encoded");
    }
    
    let count = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    let range_data = &data[5..];
    
    let mut dec = RangeDecoder::new(range_data);
    let mut out = Vec::with_capacity(count);
    
    // Order-1 context tables
    let mut ctx: [[u64; SYM_SIZE]; CTX_SIZE] = [[1u64; SYM_SIZE]; CTX_SIZE];
    let mut ctx_totals: [u64; CTX_SIZE] = [256u64; CTX_SIZE];
    
    let mut prev = 0u8;
    
    for _ in 0..count {
        let ctx_idx = prev as usize;
        let total = ctx_totals[ctx_idx];
        
        let f = dec.get_freq(total);
        
        // Find symbol by cumulative frequency lookup
        let mut cum = 0u64;
        let mut sym = 0u8;
        for i in 0..SYM_SIZE {
            cum += ctx[ctx_idx][i];
            if f < cum {
                sym = i as u8;
                break;
            }
        }
        
        // Decode and update context
        let freq = ctx[ctx_idx][sym as usize];
        dec.decode(cum - freq, freq, total);
        ctx[ctx_idx][sym as usize] += 1;
        ctx_totals[ctx_idx] += 1;
        
        out.push(sym);
        prev = sym;
    }
    
    Ok(out)
}

/// Adaptive order-2 context model for byte data.
/// Uses 256*256 = 65536 contexts (previous 2 bytes), each with 256 symbol frequencies.
/// Context (0, 0) is used for first two bytes (no previous context).

/// Adaptive order-2 range encoder for byte data.
/// Uses context=previous_2_bytes for encoding, providing better compression than order-1.
pub fn range_encode_bytes_order2(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        let mut out = Vec::new();
        out.push(2); // flag: order-2
        out.push(0); out.push(0); out.push(0); out.push(0); // length = 0 (4 bytes LE)
        return out;
    }
    
    let mut enc = RangeEncoder::new();
    
    // Order-2 context tables: [ctx][symbol] = frequency
    // ctx = (prev1 as usize) * 256 + (prev2 as usize)
    // Initialize with small non-zero values for smoothing
    let mut ctx: Vec<[u64; SYM_SIZE]> = vec![[1u64; SYM_SIZE]; CTX_SIZE * CTX_SIZE];
    let mut ctx_totals: Vec<u64> = vec![256u64; CTX_SIZE * CTX_SIZE];
    
    let mut prev1 = 0u8;
    let mut prev2 = 0u8; // context for first byte is (0, 0)
    
    for &b in data {
        let sym = b as usize;
        let ctx_idx = (prev1 as usize) * CTX_SIZE + (prev2 as usize);
        
        // Get frequencies for this context
        let total = ctx_totals[ctx_idx];
        
        // Encode symbol using context-specific frequencies
        let mut cum = 0u64;
        for i in 0..sym {
            cum += ctx[ctx_idx][i];
        }
        let freq = ctx[ctx_idx][sym];
        enc.encode(cum, freq, total);
        
        // Update context model
        ctx[ctx_idx][sym] += 1;
        ctx_totals[ctx_idx] += 1;
        
        prev2 = prev1;
        prev1 = b; // current becomes previous for next iteration
    }
    
    let mut out = Vec::new();
    out.push(2); // flag: order-2
    
    // Write symbol count as 4-byte little-endian
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_le_bytes());
    
    out.extend(enc.flush());
    out
}

/// Adaptive order-2 range decoder for byte data.
pub fn range_decode_bytes_order2(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 5 {
        return Err("Range decode: data too short");
    }
    
    let flag = data[0];
    if flag != 2 {
        return Err("Range decode: not order-2 encoded");
    }
    
    let count = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    let range_data = &data[5..];
    
    let mut dec = RangeDecoder::new(range_data);
    let mut out = Vec::with_capacity(count);
    
    // Order-2 context tables
    let mut ctx: Vec<[u64; SYM_SIZE]> = vec![[1u64; SYM_SIZE]; CTX_SIZE * CTX_SIZE];
    let mut ctx_totals: Vec<u64> = vec![256u64; CTX_SIZE * CTX_SIZE];
    
    let mut prev1 = 0u8;
    let mut prev2 = 0u8;
    
    for _ in 0..count {
        let ctx_idx = (prev1 as usize) * CTX_SIZE + (prev2 as usize);
        let total = ctx_totals[ctx_idx];
        
        let f = dec.get_freq(total);
        
        // Find symbol by cumulative frequency lookup
        let mut cum = 0u64;
        let mut sym = 0u8;
        for i in 0..SYM_SIZE {
            cum += ctx[ctx_idx][i];
            if f < cum {
                sym = i as u8;
                break;
            }
        }
        
        // Decode and update context
        let freq = ctx[ctx_idx][sym as usize];
        dec.decode(cum - freq, freq, total);
        ctx[ctx_idx][sym as usize] += 1;
        ctx_totals[ctx_idx] += 1;
        
        out.push(sym);
        prev2 = prev1;
        prev1 = sym;
    }
    
    Ok(out)
}

/// Adaptive order-1+2 mixing range coder.
/// Blends O1 and O2 predictions with adaptive weighting based on context reliability.
/// O2 has more context (2 prev bytes) but sparser statistics.
/// Weight adapts: more weight to O2 when its context is well-populated.
pub fn range_encode_bytes_order12_mix(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        let mut out = Vec::new();
        out.push(4); // flag: order-1+2 mix
        out.push(0); out.push(0); out.push(0); out.push(0); // length = 0
        return out;
    }
    
    let mut enc = RangeEncoder::new();
    
    // Order-1 model: [context][symbol] = frequency
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    
    // Order-2 model: [context][symbol] = frequency, context = prev1 * 256 + prev2
    let mut o2_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; 256 * 256];
    let mut o2_totals: Vec<u64> = vec![256u64; 256 * 256];
    
    let mut prev1 = 0u8;
    let mut prev2 = 0u8;
    
    // Reliability thresholds for adaptive weighting
    let o2_reliable_threshold: u64 = 50; // O2 context is reliable if seen 50+ symbols
    
    for &b in data {
        let sym = b as usize;
        let o1_ctx = prev1 as usize;
        let o2_ctx = (prev1 as usize) * 256 + (prev2 as usize);
        
        // Adaptive weight based on O2 context reliability
        let o2_reliability = o2_totals[o2_ctx].min(o1_totals[o1_ctx] * 2);
        let weight = if o2_reliability >= o2_reliable_threshold {
            2u64 // O2 is reliable: weight 2:1
        } else {
            1u64 // O2 not reliable yet: equal weight
        };
        
        // Mixed cumulative frequency
        let o1_cum: u64 = o1_freqs[o1_ctx][..sym].iter().sum();
        let o2_cum: u64 = o2_freqs[o2_ctx][..sym].iter().sum();
        let mixed_cum = o1_cum + o2_cum * weight;
        
        // Mixed symbol frequency
        let o1_count = o1_freqs[o1_ctx][sym];
        let o2_count = o2_freqs[o2_ctx][sym];
        let mixed_count = o1_count + o2_count * weight;
        
        // Mixed total
        let mixed_total = o1_totals[o1_ctx] + o2_totals[o2_ctx] * weight;
        
        // Encode
        let total = mixed_total.max(1);
        let cum = mixed_cum.min(total - 1);
        let freq = mixed_count.max(1).min(total - cum);
        
        enc.encode(cum, freq, total);
        
        // Update models
        o1_freqs[o1_ctx][sym] += 1;
        o1_totals[o1_ctx] += 1;
        o2_freqs[o2_ctx][sym] += 1;
        o2_totals[o2_ctx] += 1;
        
        prev2 = prev1;
        prev1 = b;
    }
    
    let mut out = Vec::new();
    out.push(4); // flag: order-1+2 mix
    
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_le_bytes());
    
    out.extend(enc.flush());
    out
}

/// Decode data encoded with adaptive O1+O2 mixing.
pub fn range_decode_bytes_order12_mix(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 5 {
        return Err("Range decode: data too short");
    }
    
    let flag = data[0];
    if flag != 4 {
        return Err("Range decode: not order-1+2 mix encoded");
    }
    
    let count = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    let range_data = &data[5..];
    
    let mut dec = RangeDecoder::new(range_data);
    let mut out = Vec::with_capacity(count);
    
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    let mut o2_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; 256 * 256];
    let mut o2_totals: Vec<u64> = vec![256u64; 256 * 256];
    
    let mut prev1 = 0u8;
    let mut prev2 = 0u8;
    let o2_reliable_threshold: u64 = 50;
    
    for _ in 0..count {
        let o1_ctx = prev1 as usize;
        let o2_ctx = (prev1 as usize) * 256 + (prev2 as usize);
        
        let o2_reliability = o2_totals[o2_ctx].min(o1_totals[o1_ctx] * 2);
        let weight = if o2_reliability >= o2_reliable_threshold { 2 } else { 1 };
        
        let mixed_total = o1_totals[o1_ctx] + o2_totals[o2_ctx] * weight;
        let f = dec.get_freq(mixed_total.max(1));
        
        // Find symbol
        let mut sym = 0u8;
        let mut cum = 0u64;
        
        for s in 0..256 {
            let o1_cum = o1_freqs[o1_ctx][..s].iter().sum::<u64>();
            let o2_cum = o2_freqs[o2_ctx][..s].iter().sum::<u64>();
            let next_cum = o1_cum + o2_cum * weight + o1_freqs[o1_ctx][s] + o2_freqs[o2_ctx][s] * weight;
            
            if cum <= f && f < next_cum {
                sym = s as u8;
                break;
            }
            cum = next_cum;
        }
        
        let o1_cum = o1_freqs[o1_ctx][..sym as usize].iter().sum::<u64>();
        let o2_cum = o2_freqs[o2_ctx][..sym as usize].iter().sum::<u64>();
        let dec_cum = o1_cum + o2_cum * weight;
        let dec_freq = o1_freqs[o1_ctx][sym as usize] + o2_freqs[o2_ctx][sym as usize] * weight;
        
        dec.decode(dec_cum, dec_freq.max(1), mixed_total.max(1));
        
        out.push(sym);
        
        o1_freqs[o1_ctx][sym as usize] += 1;
        o1_totals[o1_ctx] += 1;
        o2_freqs[o2_ctx][sym as usize] += 1;
        o2_totals[o2_ctx] += 1;
        
        prev2 = prev1;
        prev1 = sym;
    }
    
    Ok(out)
}

/// Adaptive order-0/1/2/3/4/5/6/7 mixing with EWMA-based model weighting.
/// Each model has a reliability score updated via EWMA.
/// O3-O7 use fixed sparse tables (65K entries) for deterministic encoder/decoder sync.
const EWMA_ALPHA: f64 = 0.1; // Smoothing factor for EWMA
const O3_TABLE_SIZE: usize = 1 << 16; // 65536 fixed slots — no HashMap divergence
const O3_MIN_SAMPLES: u64 = 10; // Minimum samples before O3 contributes
const O4_MIN_SAMPLES: u64 = 15; // Minimum samples before O4 contributes
const O5_MIN_SAMPLES: u64 = 20; // Minimum samples before O5 contributes
const O6_MIN_SAMPLES: u64 = 25; // Minimum samples before O6 contributes
const O7_MIN_SAMPLES: u64 = 30; // Minimum samples before O7 contributes

fn o3_hash(p1: u8, p2: u8, p3: u8) -> usize {
    let x = (p1 as u32) ^ ((p2 as u32) << 8) ^ ((p3 as u32) << 16);
    let mut h = x.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    (h as usize) & (O3_TABLE_SIZE - 1)
}

fn o4_hash(p1: u8, p2: u8, p3: u8, p4: u8) -> usize {
    let x = (p1 as u32) ^ ((p2 as u32) << 8) ^ ((p3 as u32) << 16) ^ ((p4 as u32) << 24);
    let mut h = x.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    (h as usize) & (O3_TABLE_SIZE - 1)
}

fn o5_hash(p1: u8, p2: u8, p3: u8, p4: u8, p5: u8) -> usize {
    let x = (p1 as u32) ^ ((p2 as u32) << 8) ^ ((p3 as u32) << 16) ^ ((p4 as u32) << 24)
        ^ ((p5 as u32).wrapping_mul(0x45d9f3b));
    let mut h = x.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    (h as usize) & (O3_TABLE_SIZE - 1)
}

fn o6_hash(p1: u8, p2: u8, p3: u8, p4: u8, p5: u8, p6: u8) -> usize {
    let x = (p1 as u32) ^ ((p2 as u32) << 8) ^ ((p3 as u32) << 16) ^ ((p4 as u32) << 24)
        ^ ((p5 as u32).wrapping_mul(0x45d9f3b)) ^ ((p6 as u32).wrapping_mul(0x1b4e915d));
    let mut h = x.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    (h as usize) & (O3_TABLE_SIZE - 1)
}

fn o7_hash(p1: u8, p2: u8, p3: u8, p4: u8, p5: u8, p6: u8, p7: u8) -> usize {
    let x = (p1 as u32) ^ ((p2 as u32) << 8) ^ ((p3 as u32) << 16) ^ ((p4 as u32) << 24)
        ^ ((p5 as u32).wrapping_mul(0x45d9f3b)) ^ ((p6 as u32).wrapping_mul(0x1b4e915d))
        ^ ((p7 as u32).wrapping_mul(0x9e3779b9));
    let mut h = x.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    (h as usize) & (O3_TABLE_SIZE - 1)
}

pub fn range_encode_bytes_order_ewma3(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        let mut out = Vec::new();
        out.push(7); // flag: O0+O1+O2+O3 EWMA
        out.push(0); out.push(0); out.push(0); out.push(0);
        return out;
    }
    
    let mut enc = RangeEncoder::new();
    
    // O0 model
    let mut o0_freqs = [1u64; 256];
    let mut o0_total: u64 = 256;
    
    // O1 model
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    
    // O2 model
    let mut o2_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; 256 * 256];
    let mut o2_totals: Vec<u64> = vec![256u64; 256 * 256];
    
    // O3 model - sparse fixed table (65K entries)
    let mut o3_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o3_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    
    // EWMA error tracking
    let mut o0_err: f64 = 0.5;
    let mut o1_err: f64 = 0.5;
    let mut o2_err: f64 = 0.5;
    let mut o3_err: f64 = 0.5;
    
    let mut prev1 = 0u8;
    let mut prev2 = 0u8;
    let mut prev3 = 0u8;
    
    for &b in data {
        let sym = b as usize;
        let o1_ctx = prev1 as usize;
        let o2_ctx = (prev1 as usize) * 256 + (prev2 as usize);
        let o3_idx = o3_hash(prev1, prev2, prev3);
        
        // Get O3 frequencies
        let o3_total = o3_totals[o3_idx];
        let o3_reliable = o3_total >= O3_MIN_SAMPLES;
        
        // Compute inverse-error weights
        let inv_o0 = 1.0 / (o0_err + 0.001);
        let inv_o1 = 1.0 / (o1_err + 0.001);
        let inv_o2 = 1.0 / (o2_err + 0.001);
        let inv_o3 = if o3_reliable { 1.0 / (o3_err + 0.001) } else { 0.001 };
        let inv_sum = inv_o0 + inv_o1 + inv_o2 + inv_o3;
        
        let w0 = ((inv_o0 / inv_sum) * 100.0) as u64;
        let w1 = ((inv_o1 / inv_sum) * 100.0) as u64;
        let w2 = ((inv_o2 / inv_sum) * 100.0) as u64;
        let w3 = ((inv_o3 / inv_sum) * 100.0) as u64;
        
        // Cumulative frequencies
        let o0_cum: u64 = o0_freqs[..sym].iter().sum();
        let o1_cum: u64 = o1_freqs[o1_ctx][..sym].iter().sum();
        let o2_cum: u64 = o2_freqs[o2_ctx][..sym].iter().sum();
        let o3_cum: u64 = o3_freqs[o3_idx][..sym].iter().sum();
        
        let mixed_cum = o0_cum * w0 + o1_cum * w1 + o2_cum * w2 + o3_cum * w3;
        
        let o0_count = o0_freqs[sym];
        let o1_count = o1_freqs[o1_ctx][sym];
        let o2_count = o2_freqs[o2_ctx][sym];
        let o3_count = o3_freqs[o3_idx][sym];
        
        let mixed_count = o0_count * w0 + o1_count * w1 + o2_count * w2 + o3_count * w3;
        let mixed_total = o0_total * w0 + o1_totals[o1_ctx] * w1 
            + o2_totals[o2_ctx] * w2 + o3_total * w3;
        
        // Encode
        let total = mixed_total.max(1);
        let cum = mixed_cum.min(total - 1);
        let freq = mixed_count.max(1).min(total - cum);
        
        enc.encode(cum, freq, total);
        
        // Update errors
        let actual_prob = freq as f64 / total as f64;
        let symbol_prob = 1.0 / 256.0;
        let o0_err_delta = (actual_prob - symbol_prob).abs();
        let o1_err_delta = (actual_prob - o1_freqs[o1_ctx][sym] as f64 / o1_totals[o1_ctx] as f64).abs();
        let o2_err_delta = (actual_prob - o2_freqs[o2_ctx][sym] as f64 / o2_totals[o2_ctx] as f64).abs();
        let o3_err_delta = if o3_reliable {
            (actual_prob - o3_count as f64 / o3_total as f64).abs()
        } else {
            0.5 // Neutral error when O3 not reliable
        };
        
        o0_err = (1.0 - EWMA_ALPHA) * o0_err + EWMA_ALPHA * o0_err_delta;
        o1_err = (1.0 - EWMA_ALPHA) * o1_err + EWMA_ALPHA * o1_err_delta;
        o2_err = (1.0 - EWMA_ALPHA) * o2_err + EWMA_ALPHA * o2_err_delta;
        o3_err = (1.0 - EWMA_ALPHA) * o3_err + EWMA_ALPHA * o3_err_delta;
        
        // Update all models
        o0_freqs[sym] += 1;
        o0_total += 1;
        o1_freqs[o1_ctx][sym] += 1;
        o1_totals[o1_ctx] += 1;
        o2_freqs[o2_ctx][sym] += 1;
        o2_totals[o2_ctx] += 1;
        
        // Update O3 model
        o3_freqs[o3_idx][sym] += 1;
        o3_totals[o3_idx] += 1;
        
        prev3 = prev2;
        prev2 = prev1;
        prev1 = b;
    }
    
    let mut out = Vec::new();
    out.push(7); // flag: O0+O1+O2+O3 EWMA
    
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_le_bytes());
    
    out.extend(enc.flush());
    out
}

/// Decode data encoded with O0+O1+O2+O3 EWMA mixing.
pub fn range_decode_bytes_order_ewma3(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 5 {
        return Err("Range decode: data too short");
    }
    
    let flag = data[0];
    if flag != 7 {
        return Err("Range decode: not O0+O1+O2+O3 EWMA encoded");
    }
    
    let count = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    let range_data = &data[5..];
    
    let mut dec = RangeDecoder::new(range_data);
    let mut out = Vec::with_capacity(count);
    
    let mut o0_freqs = [1u64; 256];
    let mut o0_total: u64 = 256;
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    let mut o2_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; 256 * 256];
    let mut o2_totals: Vec<u64> = vec![256u64; 256 * 256];
    let mut o3_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o3_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    
    let mut o0_err: f64 = 0.5;
    let mut o1_err: f64 = 0.5;
    let mut o2_err: f64 = 0.5;
    let mut o3_err: f64 = 0.5;
    
    let mut prev1 = 0u8;
    let mut prev2 = 0u8;
    let mut prev3 = 0u8;
    
    for _ in 0..count {
        let o1_ctx = prev1 as usize;
        let o2_ctx = (prev1 as usize) * 256 + (prev2 as usize);
        let o3_idx = o3_hash(prev1, prev2, prev3);
        
        let o3_total = o3_totals[o3_idx];
        let o3_reliable = o3_total >= O3_MIN_SAMPLES;
        
        let inv_o0 = 1.0 / (o0_err + 0.001);
        let inv_o1 = 1.0 / (o1_err + 0.001);
        let inv_o2 = 1.0 / (o2_err + 0.001);
        let inv_o3 = if o3_reliable { 1.0 / (o3_err + 0.001) } else { 0.001 };
        let inv_sum = inv_o0 + inv_o1 + inv_o2 + inv_o3;
        
        let w0 = ((inv_o0 / inv_sum) * 100.0) as u64;
        let w1 = ((inv_o1 / inv_sum) * 100.0) as u64;
        let w2 = ((inv_o2 / inv_sum) * 100.0) as u64;
        let w3 = ((inv_o3 / inv_sum) * 100.0) as u64;
        
        let mixed_total = o0_total * w0 + o1_totals[o1_ctx] * w1 
            + o2_totals[o2_ctx] * w2 + o3_total * w3;
        let f = dec.get_freq(mixed_total.max(1));
        
        // Find symbol
        let mut sym = 0u8;
        let mut cum = 0u64;
        
        for s in 0..256 {
            let o0_cum = o0_freqs[..s].iter().sum::<u64>();
            let o1_cum = o1_freqs[o1_ctx][..s].iter().sum::<u64>();
            let o2_cum = o2_freqs[o2_ctx][..s].iter().sum::<u64>();
            let o3_cum = o3_freqs[o3_idx][..s].iter().sum::<u64>();
            
            let next_cum = o0_cum * w0 + o1_cum * w1 + o2_cum * w2 + o3_cum * w3
                + o0_freqs[s] * w0 + o1_freqs[o1_ctx][s] * w1 
                + o2_freqs[o2_ctx][s] * w2 + o3_freqs[o3_idx][s] * w3;
            
            if cum <= f && f < next_cum {
                sym = s as u8;
                break;
            }
            cum = next_cum;
        }
        
        let o0_cum = o0_freqs[..sym as usize].iter().sum::<u64>();
        let o1_cum = o1_freqs[o1_ctx][..sym as usize].iter().sum::<u64>();
        let o2_cum = o2_freqs[o2_ctx][..sym as usize].iter().sum::<u64>();
        let o3_cum = o3_freqs[o3_idx][..sym as usize].iter().sum::<u64>();
        
        let dec_cum = o0_cum * w0 + o1_cum * w1 + o2_cum * w2 + o3_cum * w3;
        let dec_freq = o0_freqs[sym as usize] * w0 + o1_freqs[o1_ctx][sym as usize] * w1 
            + o2_freqs[o2_ctx][sym as usize] * w2 
            + o3_freqs[o3_idx][sym as usize] * w3;
        
        dec.decode(dec_cum, dec_freq.max(1), mixed_total.max(1));
        
        out.push(sym);
        
        // Update errors
        let total = mixed_total.max(1);
        let actual_prob = dec_freq as f64 / total as f64;
        let symbol_prob = 1.0 / 256.0;
        let o0_err_delta = (actual_prob - symbol_prob).abs();
        let o1_err_delta = (actual_prob - o1_freqs[o1_ctx][sym as usize] as f64 / o1_totals[o1_ctx] as f64).abs();
        let o2_err_delta = (actual_prob - o2_freqs[o2_ctx][sym as usize] as f64 / o2_totals[o2_ctx].max(1) as f64).abs();
        let o3_err_delta = if o3_reliable {
            let o3_count_val = o3_freqs[o3_idx][sym as usize];
            let o3_total_val = o3_totals[o3_idx];
            (actual_prob - o3_count_val as f64 / o3_total_val.max(1) as f64).abs()
        } else {
            0.5
        };
        
        o0_err = (1.0 - EWMA_ALPHA) * o0_err + EWMA_ALPHA * o0_err_delta;
        o1_err = (1.0 - EWMA_ALPHA) * o1_err + EWMA_ALPHA * o1_err_delta;
        o2_err = (1.0 - EWMA_ALPHA) * o2_err + EWMA_ALPHA * o2_err_delta;
        o3_err = (1.0 - EWMA_ALPHA) * o3_err + EWMA_ALPHA * o3_err_delta;
        
        // Update models
        o0_freqs[sym as usize] += 1;
        o0_total += 1;
        o1_freqs[o1_ctx][sym as usize] += 1;
        o1_totals[o1_ctx] += 1;
        o2_freqs[o2_ctx][sym as usize] += 1;
        o2_totals[o2_ctx] += 1;
        
        o3_freqs[o3_idx][sym as usize] += 1;
        o3_totals[o3_idx] += 1;
        
        prev3 = prev2;
        prev2 = prev1;
        prev1 = sym;
    }
    
    Ok(out)
}

/// Encode data with adaptive O0+O1+O2+O3+O4+O5 EWMA mixing (sparse tables).
pub fn range_encode_bytes_order_ewma5(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        let mut out = Vec::new();
        out.push(8); // flag: O0+O1+O2+O3+O4+O5 EWMA
        out.push(0); out.push(0); out.push(0); out.push(0);
        return out;
    }
    
    let mut enc = RangeEncoder::new();
    
    // O0 model
    let mut o0_freqs = [1u64; 256];
    let mut o0_total: u64 = 256;
    
    // O1 model
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    
    // O2 model
    let mut o2_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; 256 * 256];
    let mut o2_totals: Vec<u64> = vec![256u64; 256 * 256];
    
    // O3/O4/O5 models - sparse fixed tables (65K entries each)
    let mut o3_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o3_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o4_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o4_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o5_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o5_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    
    // EWMA error tracking
    let mut o0_err: f64 = 0.5;
    let mut o1_err: f64 = 0.5;
    let mut o2_err: f64 = 0.5;
    let mut o3_err: f64 = 0.5;
    let mut o4_err: f64 = 0.5;
    let mut o5_err: f64 = 0.5;
    
    let mut prev1 = 0u8;
    let mut prev2 = 0u8;
    let mut prev3 = 0u8;
    let mut prev4 = 0u8;
    let mut prev5 = 0u8;
    
    for &b in data {
        let sym = b as usize;
        let o1_ctx = prev1 as usize;
        let o2_ctx = (prev1 as usize) * 256 + (prev2 as usize);
        let o3_idx = o3_hash(prev1, prev2, prev3);
        let o4_idx = o4_hash(prev1, prev2, prev3, prev4);
        let o5_idx = o5_hash(prev1, prev2, prev3, prev4, prev5);
        
        let o3_total = o3_totals[o3_idx];
        let o4_total = o4_totals[o4_idx];
        let o5_total = o5_totals[o5_idx];
        
        let o3_reliable = o3_total >= O3_MIN_SAMPLES;
        let o4_reliable = o4_total >= O4_MIN_SAMPLES;
        let o5_reliable = o5_total >= O5_MIN_SAMPLES;
        
        // Compute inverse-error weights
        let inv_o0 = 1.0 / (o0_err + 0.001);
        let inv_o1 = 1.0 / (o1_err + 0.001);
        let inv_o2 = 1.0 / (o2_err + 0.001);
        let inv_o3 = if o3_reliable { 1.0 / (o3_err + 0.001) } else { 0.001 };
        let inv_o4 = if o4_reliable { 1.0 / (o4_err + 0.001) } else { 0.001 };
        let inv_o5 = if o5_reliable { 1.0 / (o5_err + 0.001) } else { 0.001 };
        let inv_sum = inv_o0 + inv_o1 + inv_o2 + inv_o3 + inv_o4 + inv_o5;
        
        let w0 = ((inv_o0 / inv_sum) * 100.0) as u64;
        let w1 = ((inv_o1 / inv_sum) * 100.0) as u64;
        let w2 = ((inv_o2 / inv_sum) * 100.0) as u64;
        let w3 = ((inv_o3 / inv_sum) * 100.0) as u64;
        let w4 = ((inv_o4 / inv_sum) * 100.0) as u64;
        let w5 = ((inv_o5 / inv_sum) * 100.0) as u64;
        
        // Cumulative frequencies
        let o0_cum: u64 = o0_freqs[..sym].iter().sum();
        let o1_cum: u64 = o1_freqs[o1_ctx][..sym].iter().sum();
        let o2_cum: u64 = o2_freqs[o2_ctx][..sym].iter().sum();
        let o3_cum: u64 = o3_freqs[o3_idx][..sym].iter().sum();
        let o4_cum: u64 = o4_freqs[o4_idx][..sym].iter().sum();
        let o5_cum: u64 = o5_freqs[o5_idx][..sym].iter().sum();
        
        let mixed_cum = o0_cum * w0 + o1_cum * w1 + o2_cum * w2 + o3_cum * w3 + o4_cum * w4 + o5_cum * w5;
        
        let o0_count = o0_freqs[sym];
        let o1_count = o1_freqs[o1_ctx][sym];
        let o2_count = o2_freqs[o2_ctx][sym];
        let o3_count = o3_freqs[o3_idx][sym];
        let o4_count = o4_freqs[o4_idx][sym];
        let o5_count = o5_freqs[o5_idx][sym];
        
        let mixed_count = o0_count * w0 + o1_count * w1 + o2_count * w2 + o3_count * w3 + o4_count * w4 + o5_count * w5;
        let mixed_total = o0_total * w0 + o1_totals[o1_ctx] * w1 
            + o2_totals[o2_ctx] * w2 + o3_total * w3 + o4_total * w4 + o5_total * w5;
        
        // Encode
        let total = mixed_total.max(1);
        let cum = mixed_cum.min(total - 1);
        let freq = mixed_count.max(1).min(total - cum);
        
        enc.encode(cum, freq, total);
        
        // Update errors
        let actual_prob = freq as f64 / total as f64;
        let symbol_prob = 1.0 / 256.0;
        let o0_err_delta = (actual_prob - symbol_prob).abs();
        let o1_err_delta = (actual_prob - o1_freqs[o1_ctx][sym] as f64 / o1_totals[o1_ctx] as f64).abs();
        let o2_err_delta = (actual_prob - o2_freqs[o2_ctx][sym] as f64 / o2_totals[o2_ctx] as f64).abs();
        let o3_err_delta = if o3_reliable {
            (actual_prob - o3_count as f64 / o3_total as f64).abs()
        } else { 0.5 };
        let o4_err_delta = if o4_reliable {
            (actual_prob - o4_count as f64 / o4_total as f64).abs()
        } else { 0.5 };
        let o5_err_delta = if o5_reliable {
            (actual_prob - o5_count as f64 / o5_total as f64).abs()
        } else { 0.5 };
        
        o0_err = (1.0 - EWMA_ALPHA) * o0_err + EWMA_ALPHA * o0_err_delta;
        o1_err = (1.0 - EWMA_ALPHA) * o1_err + EWMA_ALPHA * o1_err_delta;
        o2_err = (1.0 - EWMA_ALPHA) * o2_err + EWMA_ALPHA * o2_err_delta;
        o3_err = (1.0 - EWMA_ALPHA) * o3_err + EWMA_ALPHA * o3_err_delta;
        o4_err = (1.0 - EWMA_ALPHA) * o4_err + EWMA_ALPHA * o4_err_delta;
        o5_err = (1.0 - EWMA_ALPHA) * o5_err + EWMA_ALPHA * o5_err_delta;
        
        // Update all models
        o0_freqs[sym] += 1;
        o0_total += 1;
        o1_freqs[o1_ctx][sym] += 1;
        o1_totals[o1_ctx] += 1;
        o2_freqs[o2_ctx][sym] += 1;
        o2_totals[o2_ctx] += 1;
        o3_freqs[o3_idx][sym] += 1;
        o3_totals[o3_idx] += 1;
        o4_freqs[o4_idx][sym] += 1;
        o4_totals[o4_idx] += 1;
        o5_freqs[o5_idx][sym] += 1;
        o5_totals[o5_idx] += 1;
        
        prev5 = prev4;
        prev4 = prev3;
        prev3 = prev2;
        prev2 = prev1;
        prev1 = b;
    }
    
    let mut out = Vec::new();
    out.push(8); // flag: O0+O1+O2+O3+O4+O5 EWMA
    
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend(enc.flush());
    out
}

/// Decode data encoded with O0+O1+O2+O3+O4+O5 EWMA mixing (sparse tables).
pub fn range_decode_bytes_order_ewma5(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 5 {
        return Err("Range decode: data too short");
    }
    
    let flag = data[0];
    if flag != 8 {
        return Err("Range decode: not O0+O1+O2+O3+O4+O5 EWMA encoded");
    }
    
    let count = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    let range_data = &data[5..];
    
    let mut dec = RangeDecoder::new(range_data);
    let mut out = Vec::with_capacity(count);
    
    let mut o0_freqs = [1u64; 256];
    let mut o0_total: u64 = 256;
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    let mut o2_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; 256 * 256];
    let mut o2_totals: Vec<u64> = vec![256u64; 256 * 256];
    let mut o3_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o3_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o4_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o4_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o5_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o5_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    
    let mut o0_err: f64 = 0.5;
    let mut o1_err: f64 = 0.5;
    let mut o2_err: f64 = 0.5;
    let mut o3_err: f64 = 0.5;
    let mut o4_err: f64 = 0.5;
    let mut o5_err: f64 = 0.5;
    
    let mut prev1 = 0u8;
    let mut prev2 = 0u8;
    let mut prev3 = 0u8;
    let mut prev4 = 0u8;
    let mut prev5 = 0u8;
    
    for _ in 0..count {
        let o1_ctx = prev1 as usize;
        let o2_ctx = (prev1 as usize) * 256 + (prev2 as usize);
        let o3_idx = o3_hash(prev1, prev2, prev3);
        let o4_idx = o4_hash(prev1, prev2, prev3, prev4);
        let o5_idx = o5_hash(prev1, prev2, prev3, prev4, prev5);
        
        let o3_total = o3_totals[o3_idx];
        let o4_total = o4_totals[o4_idx];
        let o5_total = o5_totals[o5_idx];
        
        let o3_reliable = o3_total >= O3_MIN_SAMPLES;
        let o4_reliable = o4_total >= O4_MIN_SAMPLES;
        let o5_reliable = o5_total >= O5_MIN_SAMPLES;
        
        let inv_o0 = 1.0 / (o0_err + 0.001);
        let inv_o1 = 1.0 / (o1_err + 0.001);
        let inv_o2 = 1.0 / (o2_err + 0.001);
        let inv_o3 = if o3_reliable { 1.0 / (o3_err + 0.001) } else { 0.001 };
        let inv_o4 = if o4_reliable { 1.0 / (o4_err + 0.001) } else { 0.001 };
        let inv_o5 = if o5_reliable { 1.0 / (o5_err + 0.001) } else { 0.001 };
        let inv_sum = inv_o0 + inv_o1 + inv_o2 + inv_o3 + inv_o4 + inv_o5;
        
        let w0 = ((inv_o0 / inv_sum) * 100.0) as u64;
        let w1 = ((inv_o1 / inv_sum) * 100.0) as u64;
        let w2 = ((inv_o2 / inv_sum) * 100.0) as u64;
        let w3 = ((inv_o3 / inv_sum) * 100.0) as u64;
        let w4 = ((inv_o4 / inv_sum) * 100.0) as u64;
        let w5 = ((inv_o5 / inv_sum) * 100.0) as u64;
        
        let mixed_total = o0_total * w0 + o1_totals[o1_ctx] * w1 
            + o2_totals[o2_ctx] * w2 + o3_total * w3 + o4_total * w4 + o5_total * w5;
        let f = dec.get_freq(mixed_total.max(1));
        
        // Find symbol
        let mut sym = 0u8;
        let mut cum = 0u64;
        
        for s in 0..256 {
            let o0_cum = o0_freqs[..s].iter().sum::<u64>();
            let o1_cum = o1_freqs[o1_ctx][..s].iter().sum::<u64>();
            let o2_cum = o2_freqs[o2_ctx][..s].iter().sum::<u64>();
            let o3_cum = o3_freqs[o3_idx][..s].iter().sum::<u64>();
            let o4_cum = o4_freqs[o4_idx][..s].iter().sum::<u64>();
            let o5_cum = o5_freqs[o5_idx][..s].iter().sum::<u64>();
            
            let next_cum = o0_cum * w0 + o1_cum * w1 + o2_cum * w2 + o3_cum * w3 + o4_cum * w4 + o5_cum * w5
                + o0_freqs[s] * w0 + o1_freqs[o1_ctx][s] * w1 + o2_freqs[o2_ctx][s] * w2 
                + o3_freqs[o3_idx][s] * w3 + o4_freqs[o4_idx][s] * w4 + o5_freqs[o5_idx][s] * w5;
            
            if cum <= f && f < next_cum {
                sym = s as u8;
                break;
            }
            cum = next_cum;
        }
        
        let o0_cum = o0_freqs[..sym as usize].iter().sum::<u64>();
        let o1_cum = o1_freqs[o1_ctx][..sym as usize].iter().sum::<u64>();
        let o2_cum = o2_freqs[o2_ctx][..sym as usize].iter().sum::<u64>();
        let o3_cum = o3_freqs[o3_idx][..sym as usize].iter().sum::<u64>();
        let o4_cum = o4_freqs[o4_idx][..sym as usize].iter().sum::<u64>();
        let o5_cum = o5_freqs[o5_idx][..sym as usize].iter().sum::<u64>();
        
        let dec_cum = o0_cum * w0 + o1_cum * w1 + o2_cum * w2 + o3_cum * w3 + o4_cum * w4 + o5_cum * w5;
        let dec_freq = o0_freqs[sym as usize] * w0 + o1_freqs[o1_ctx][sym as usize] * w1 
            + o2_freqs[o2_ctx][sym as usize] * w2 + o3_freqs[o3_idx][sym as usize] * w3
            + o4_freqs[o4_idx][sym as usize] * w4 + o5_freqs[o5_idx][sym as usize] * w5;
        
        dec.decode(dec_cum, dec_freq.max(1), mixed_total.max(1));
        
        out.push(sym);
        
        // Update errors
        let total = mixed_total.max(1);
        let actual_prob = dec_freq as f64 / total as f64;
        let symbol_prob = 1.0 / 256.0;
        let o0_err_delta = (actual_prob - symbol_prob).abs();
        let o1_err_delta = (actual_prob - o1_freqs[o1_ctx][sym as usize] as f64 / o1_totals[o1_ctx] as f64).abs();
        let o2_err_delta = (actual_prob - o2_freqs[o2_ctx][sym as usize] as f64 / o2_totals[o2_ctx].max(1) as f64).abs();
        let o3_err_delta = if o3_reliable {
            let o3_count_val = o3_freqs[o3_idx][sym as usize];
            let o3_total_val = o3_totals[o3_idx];
            (actual_prob - o3_count_val as f64 / o3_total_val.max(1) as f64).abs()
        } else { 0.5 };
        let o4_err_delta = if o4_reliable {
            let o4_count_val = o4_freqs[o4_idx][sym as usize];
            let o4_total_val = o4_totals[o4_idx];
            (actual_prob - o4_count_val as f64 / o4_total_val.max(1) as f64).abs()
        } else { 0.5 };
        let o5_err_delta = if o5_reliable {
            let o5_count_val = o5_freqs[o5_idx][sym as usize];
            let o5_total_val = o5_totals[o5_idx];
            (actual_prob - o5_count_val as f64 / o5_total_val.max(1) as f64).abs()
        } else { 0.5 };
        
        o0_err = (1.0 - EWMA_ALPHA) * o0_err + EWMA_ALPHA * o0_err_delta;
        o1_err = (1.0 - EWMA_ALPHA) * o1_err + EWMA_ALPHA * o1_err_delta;
        o2_err = (1.0 - EWMA_ALPHA) * o2_err + EWMA_ALPHA * o2_err_delta;
        o3_err = (1.0 - EWMA_ALPHA) * o3_err + EWMA_ALPHA * o3_err_delta;
        o4_err = (1.0 - EWMA_ALPHA) * o4_err + EWMA_ALPHA * o4_err_delta;
        o5_err = (1.0 - EWMA_ALPHA) * o5_err + EWMA_ALPHA * o5_err_delta;
        
        // Update models
        o0_freqs[sym as usize] += 1;
        o0_total += 1;
        o1_freqs[o1_ctx][sym as usize] += 1;
        o1_totals[o1_ctx] += 1;
        o2_freqs[o2_ctx][sym as usize] += 1;
        o2_totals[o2_ctx] += 1;
        o3_freqs[o3_idx][sym as usize] += 1;
        o3_totals[o3_idx] += 1;
        o4_freqs[o4_idx][sym as usize] += 1;
        o4_totals[o4_idx] += 1;
        o5_freqs[o5_idx][sym as usize] += 1;
        o5_totals[o5_idx] += 1;
        
        prev5 = prev4;
        prev4 = prev3;
        prev3 = prev2;
        prev2 = prev1;
        prev1 = sym;
    }
    
    Ok(out)
}

/// Encode data with adaptive O0+O1+O2+O3+O4+O5+O6+O7 EWMA mixing (sparse tables).
pub fn range_encode_bytes_order_ewma7(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        let mut out = Vec::new();
        out.push(9); // flag: O0+O1+O2+O3+O4+O5+O6+O7 EWMA
        out.push(0); out.push(0); out.push(0); out.push(0);
        return out;
    }
    
    let mut enc = RangeEncoder::new();
    
    // O0 model
    let mut o0_freqs = [1u64; 256];
    let mut o0_total: u64 = 256;
    
    // O1 model
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    
    // O2 model
    let mut o2_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; 256 * 256];
    let mut o2_totals: Vec<u64> = vec![256u64; 256 * 256];
    
    // O3-O7 models - sparse fixed tables (65K entries each)
    let mut o3_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o3_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o4_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o4_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o5_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o5_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o6_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o6_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o7_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o7_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    
    // EWMA error tracking
    let mut o0_err: f64 = 0.5;
    let mut o1_err: f64 = 0.5;
    let mut o2_err: f64 = 0.5;
    let mut o3_err: f64 = 0.5;
    let mut o4_err: f64 = 0.5;
    let mut o5_err: f64 = 0.5;
    let mut o6_err: f64 = 0.5;
    let mut o7_err: f64 = 0.5;
    
    // History registers (all init to 0 for first bytes)
    let mut prev1 = 0u8;
    let mut prev2 = 0u8;
    let mut prev3 = 0u8;
    let mut prev4 = 0u8;
    let mut prev5 = 0u8;
    let mut prev6 = 0u8;
    let mut prev7 = 0u8;
    
    for &b in data {
        let sym = b as usize;
        let o1_ctx = prev1 as usize;
        let o2_ctx = (prev1 as usize) * 256 + (prev2 as usize);
        let o3_idx = o3_hash(prev1, prev2, prev3);
        let o4_idx = o4_hash(prev1, prev2, prev3, prev4);
        let o5_idx = o5_hash(prev1, prev2, prev3, prev4, prev5);
        let o6_idx = o6_hash(prev1, prev2, prev3, prev4, prev5, prev6);
        let o7_idx = o7_hash(prev1, prev2, prev3, prev4, prev5, prev6, prev7);
        
        let o3_total = o3_totals[o3_idx];
        let o4_total = o4_totals[o4_idx];
        let o5_total = o5_totals[o5_idx];
        let o6_total = o6_totals[o6_idx];
        let o7_total = o7_totals[o7_idx];
        
        let o3_reliable = o3_total >= O3_MIN_SAMPLES;
        let o4_reliable = o4_total >= O4_MIN_SAMPLES;
        let o5_reliable = o5_total >= O5_MIN_SAMPLES;
        let o6_reliable = o6_total >= O6_MIN_SAMPLES;
        let o7_reliable = o7_total >= O7_MIN_SAMPLES;
        
        // Compute inverse-error weights
        let inv_o0 = 1.0 / (o0_err + 0.001);
        let inv_o1 = 1.0 / (o1_err + 0.001);
        let inv_o2 = 1.0 / (o2_err + 0.001);
        let inv_o3 = if o3_reliable { 1.0 / (o3_err + 0.001) } else { 0.001 };
        let inv_o4 = if o4_reliable { 1.0 / (o4_err + 0.001) } else { 0.001 };
        let inv_o5 = if o5_reliable { 1.0 / (o5_err + 0.001) } else { 0.001 };
        let inv_o6 = if o6_reliable { 1.0 / (o6_err + 0.001) } else { 0.001 };
        let inv_o7 = if o7_reliable { 1.0 / (o7_err + 0.001) } else { 0.001 };
        let inv_sum = inv_o0 + inv_o1 + inv_o2 + inv_o3 + inv_o4 + inv_o5 + inv_o6 + inv_o7;
        
        let w0 = ((inv_o0 / inv_sum) * 100.0) as u64;
        let w1 = ((inv_o1 / inv_sum) * 100.0) as u64;
        let w2 = ((inv_o2 / inv_sum) * 100.0) as u64;
        let w3 = ((inv_o3 / inv_sum) * 100.0) as u64;
        let w4 = ((inv_o4 / inv_sum) * 100.0) as u64;
        let w5 = ((inv_o5 / inv_sum) * 100.0) as u64;
        let w6 = ((inv_o6 / inv_sum) * 100.0) as u64;
        let w7 = ((inv_o7 / inv_sum) * 100.0) as u64;
        
        // Cumulative frequencies
        let o0_cum: u64 = o0_freqs[..sym].iter().sum();
        let o1_cum: u64 = o1_freqs[o1_ctx][..sym].iter().sum();
        let o2_cum: u64 = o2_freqs[o2_ctx][..sym].iter().sum();
        let o3_cum: u64 = o3_freqs[o3_idx][..sym].iter().sum();
        let o4_cum: u64 = o4_freqs[o4_idx][..sym].iter().sum();
        let o5_cum: u64 = o5_freqs[o5_idx][..sym].iter().sum();
        let o6_cum: u64 = o6_freqs[o6_idx][..sym].iter().sum();
        let o7_cum: u64 = o7_freqs[o7_idx][..sym].iter().sum();
        
        let mixed_cum = o0_cum*w0 + o1_cum*w1 + o2_cum*w2 + o3_cum*w3 + o4_cum*w4 + o5_cum*w5 + o6_cum*w6 + o7_cum*w7;
        
        let o0_count = o0_freqs[sym];
        let o1_count = o1_freqs[o1_ctx][sym];
        let o2_count = o2_freqs[o2_ctx][sym];
        let o3_count = o3_freqs[o3_idx][sym];
        let o4_count = o4_freqs[o4_idx][sym];
        let o5_count = o5_freqs[o5_idx][sym];
        let o6_count = o6_freqs[o6_idx][sym];
        let o7_count = o7_freqs[o7_idx][sym];
        
        let mixed_count = o0_count*w0 + o1_count*w1 + o2_count*w2 + o3_count*w3 + o4_count*w4 + o5_count*w5 + o6_count*w6 + o7_count*w7;
        let mixed_total = o0_total*w0 + o1_totals[o1_ctx]*w1 + o2_totals[o2_ctx]*w2 
            + o3_total*w3 + o4_total*w4 + o5_total*w5 + o6_total*w6 + o7_total*w7;
        
        // Encode
        let total = mixed_total.max(1);
        let cum = mixed_cum.min(total - 1);
        let freq = mixed_count.max(1).min(total - cum);
        
        enc.encode(cum, freq, total);
        
        // Update errors
        let actual_prob = freq as f64 / total as f64;
        let symbol_prob = 1.0 / 256.0;
        let o0_err_delta = (actual_prob - symbol_prob).abs();
        let o1_err_delta = (actual_prob - o1_freqs[o1_ctx][sym] as f64 / o1_totals[o1_ctx] as f64).abs();
        let o2_err_delta = (actual_prob - o2_freqs[o2_ctx][sym] as f64 / o2_totals[o2_ctx] as f64).abs();
        let o3_err_delta = if o3_reliable { (actual_prob - o3_count as f64 / o3_total as f64).abs() } else { 0.5 };
        let o4_err_delta = if o4_reliable { (actual_prob - o4_count as f64 / o4_total as f64).abs() } else { 0.5 };
        let o5_err_delta = if o5_reliable { (actual_prob - o5_count as f64 / o5_total as f64).abs() } else { 0.5 };
        let o6_err_delta = if o6_reliable { (actual_prob - o6_count as f64 / o6_total as f64).abs() } else { 0.5 };
        let o7_err_delta = if o7_reliable { (actual_prob - o7_count as f64 / o7_total as f64).abs() } else { 0.5 };
        
        o0_err = (1.0 - EWMA_ALPHA) * o0_err + EWMA_ALPHA * o0_err_delta;
        o1_err = (1.0 - EWMA_ALPHA) * o1_err + EWMA_ALPHA * o1_err_delta;
        o2_err = (1.0 - EWMA_ALPHA) * o2_err + EWMA_ALPHA * o2_err_delta;
        o3_err = (1.0 - EWMA_ALPHA) * o3_err + EWMA_ALPHA * o3_err_delta;
        o4_err = (1.0 - EWMA_ALPHA) * o4_err + EWMA_ALPHA * o4_err_delta;
        o5_err = (1.0 - EWMA_ALPHA) * o5_err + EWMA_ALPHA * o5_err_delta;
        o6_err = (1.0 - EWMA_ALPHA) * o6_err + EWMA_ALPHA * o6_err_delta;
        o7_err = (1.0 - EWMA_ALPHA) * o7_err + EWMA_ALPHA * o7_err_delta;
        
        // Update all models
        o0_freqs[sym] += 1; o0_total += 1;
        o1_freqs[o1_ctx][sym] += 1; o1_totals[o1_ctx] += 1;
        o2_freqs[o2_ctx][sym] += 1; o2_totals[o2_ctx] += 1;
        o3_freqs[o3_idx][sym] += 1; o3_totals[o3_idx] += 1;
        o4_freqs[o4_idx][sym] += 1; o4_totals[o4_idx] += 1;
        o5_freqs[o5_idx][sym] += 1; o5_totals[o5_idx] += 1;
        o6_freqs[o6_idx][sym] += 1; o6_totals[o6_idx] += 1;
        o7_freqs[o7_idx][sym] += 1; o7_totals[o7_idx] += 1;
        
        // Shift history
        prev7 = prev6; prev6 = prev5; prev5 = prev4;
        prev4 = prev3; prev3 = prev2; prev2 = prev1; prev1 = b;
    }
    
    let mut out = Vec::new();
    out.push(9); // flag: O0+O1+O2+O3+O4+O5+O6+O7 EWMA
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend(enc.flush());
    out
}

/// Decode data encoded with O0+O1+O2+O3+O4+O5+O6+O7 EWMA mixing (sparse tables).
pub fn range_decode_bytes_order_ewma7(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 5 {
        return Err("Range decode: data too short");
    }
    let flag = data[0];
    if flag != 9 {
        return Err("Range decode: not O0+O1+O2+O3+O4+O5+O6+O7 EWMA encoded");
    }
    
    let count = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    let range_data = &data[5..];
    
    let mut dec = RangeDecoder::new(range_data);
    let mut out = Vec::with_capacity(count);
    
    let mut o0_freqs = [1u64; 256];
    let mut o0_total: u64 = 256;
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    let mut o2_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; 256 * 256];
    let mut o2_totals: Vec<u64> = vec![256u64; 256 * 256];
    let mut o3_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o3_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o4_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o4_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o5_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o5_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o6_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o6_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    let mut o7_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; O3_TABLE_SIZE];
    let mut o7_totals: Vec<u64> = vec![256u64; O3_TABLE_SIZE];
    
    let mut o0_err: f64 = 0.5; let mut o1_err: f64 = 0.5; let mut o2_err: f64 = 0.5;
    let mut o3_err: f64 = 0.5; let mut o4_err: f64 = 0.5; let mut o5_err: f64 = 0.5;
    let mut o6_err: f64 = 0.5; let mut o7_err: f64 = 0.5;
    
    let mut prev1 = 0u8; let mut prev2 = 0u8; let mut prev3 = 0u8; let mut prev4 = 0u8;
    let mut prev5 = 0u8; let mut prev6 = 0u8; let mut prev7 = 0u8;
    
    for _ in 0..count {
        let o1_ctx = prev1 as usize;
        let o2_ctx = (prev1 as usize) * 256 + (prev2 as usize);
        let o3_idx = o3_hash(prev1, prev2, prev3);
        let o4_idx = o4_hash(prev1, prev2, prev3, prev4);
        let o5_idx = o5_hash(prev1, prev2, prev3, prev4, prev5);
        let o6_idx = o6_hash(prev1, prev2, prev3, prev4, prev5, prev6);
        let o7_idx = o7_hash(prev1, prev2, prev3, prev4, prev5, prev6, prev7);
        
        let o3_total = o3_totals[o3_idx]; let o4_total = o4_totals[o4_idx];
        let o5_total = o5_totals[o5_idx]; let o6_total = o6_totals[o6_idx];
        let o7_total = o7_totals[o7_idx];
        
        let o3_reliable = o3_total >= O3_MIN_SAMPLES;
        let o4_reliable = o4_total >= O4_MIN_SAMPLES;
        let o5_reliable = o5_total >= O5_MIN_SAMPLES;
        let o6_reliable = o6_total >= O6_MIN_SAMPLES;
        let o7_reliable = o7_total >= O7_MIN_SAMPLES;
        
        let inv_o0 = 1.0 / (o0_err + 0.001);
        let inv_o1 = 1.0 / (o1_err + 0.001);
        let inv_o2 = 1.0 / (o2_err + 0.001);
        let inv_o3 = if o3_reliable { 1.0 / (o3_err + 0.001) } else { 0.001 };
        let inv_o4 = if o4_reliable { 1.0 / (o4_err + 0.001) } else { 0.001 };
        let inv_o5 = if o5_reliable { 1.0 / (o5_err + 0.001) } else { 0.001 };
        let inv_o6 = if o6_reliable { 1.0 / (o6_err + 0.001) } else { 0.001 };
        let inv_o7 = if o7_reliable { 1.0 / (o7_err + 0.001) } else { 0.001 };
        let inv_sum = inv_o0 + inv_o1 + inv_o2 + inv_o3 + inv_o4 + inv_o5 + inv_o6 + inv_o7;
        
        let w0 = ((inv_o0 / inv_sum) * 100.0) as u64;
        let w1 = ((inv_o1 / inv_sum) * 100.0) as u64;
        let w2 = ((inv_o2 / inv_sum) * 100.0) as u64;
        let w3 = ((inv_o3 / inv_sum) * 100.0) as u64;
        let w4 = ((inv_o4 / inv_sum) * 100.0) as u64;
        let w5 = ((inv_o5 / inv_sum) * 100.0) as u64;
        let w6 = ((inv_o6 / inv_sum) * 100.0) as u64;
        let w7 = ((inv_o7 / inv_sum) * 100.0) as u64;
        
        let mixed_total = o0_total*w0 + o1_totals[o1_ctx]*w1 + o2_totals[o2_ctx]*w2
            + o3_total*w3 + o4_total*w4 + o5_total*w5 + o6_total*w6 + o7_total*w7;
        let f = dec.get_freq(mixed_total.max(1));
        
        // Find symbol
        let mut sym = 0u8;
        let mut cum = 0u64;
        for s in 0..256 {
            let o0_cum = o0_freqs[..s].iter().sum::<u64>();
            let o1_cum = o1_freqs[o1_ctx][..s].iter().sum::<u64>();
            let o2_cum = o2_freqs[o2_ctx][..s].iter().sum::<u64>();
            let o3_cum = o3_freqs[o3_idx][..s].iter().sum::<u64>();
            let o4_cum = o4_freqs[o4_idx][..s].iter().sum::<u64>();
            let o5_cum = o5_freqs[o5_idx][..s].iter().sum::<u64>();
            let o6_cum = o6_freqs[o6_idx][..s].iter().sum::<u64>();
            let o7_cum = o7_freqs[o7_idx][..s].iter().sum::<u64>();
            
            let next_cum = o0_cum*w0 + o1_cum*w1 + o2_cum*w2 + o3_cum*w3 + o4_cum*w4 + o5_cum*w5 + o6_cum*w6 + o7_cum*w7
                + o0_freqs[s]*w0 + o1_freqs[o1_ctx][s]*w1 + o2_freqs[o2_ctx][s]*w2
                + o3_freqs[o3_idx][s]*w3 + o4_freqs[o4_idx][s]*w4 + o5_freqs[o5_idx][s]*w5
                + o6_freqs[o6_idx][s]*w6 + o7_freqs[o7_idx][s]*w7;
            
            if cum <= f && f < next_cum { sym = s as u8; break; }
            cum = next_cum;
        }
        
        let o0_cum = o0_freqs[..sym as usize].iter().sum::<u64>();
        let o1_cum = o1_freqs[o1_ctx][..sym as usize].iter().sum::<u64>();
        let o2_cum = o2_freqs[o2_ctx][..sym as usize].iter().sum::<u64>();
        let o3_cum = o3_freqs[o3_idx][..sym as usize].iter().sum::<u64>();
        let o4_cum = o4_freqs[o4_idx][..sym as usize].iter().sum::<u64>();
        let o5_cum = o5_freqs[o5_idx][..sym as usize].iter().sum::<u64>();
        let o6_cum = o6_freqs[o6_idx][..sym as usize].iter().sum::<u64>();
        let o7_cum = o7_freqs[o7_idx][..sym as usize].iter().sum::<u64>();
        
        let dec_cum = o0_cum*w0 + o1_cum*w1 + o2_cum*w2 + o3_cum*w3 + o4_cum*w4 + o5_cum*w5 + o6_cum*w6 + o7_cum*w7;
        let dec_freq = o0_freqs[sym as usize]*w0 + o1_freqs[o1_ctx][sym as usize]*w1 + o2_freqs[o2_ctx][sym as usize]*w2
            + o3_freqs[o3_idx][sym as usize]*w3 + o4_freqs[o4_idx][sym as usize]*w4 + o5_freqs[o5_idx][sym as usize]*w5
            + o6_freqs[o6_idx][sym as usize]*w6 + o7_freqs[o7_idx][sym as usize]*w7;
        
        dec.decode(dec_cum, dec_freq.max(1), mixed_total.max(1));
        out.push(sym);
        
        // Update errors
        let total = mixed_total.max(1);
        let actual_prob = dec_freq as f64 / total as f64;
        let symbol_prob = 1.0 / 256.0;
        let o0_err_delta = (actual_prob - symbol_prob).abs();
        let o1_err_delta = (actual_prob - o1_freqs[o1_ctx][sym as usize] as f64 / o1_totals[o1_ctx] as f64).abs();
        let o2_err_delta = (actual_prob - o2_freqs[o2_ctx][sym as usize] as f64 / o2_totals[o2_ctx].max(1) as f64).abs();
        let o3_err_delta = if o3_reliable { let c = o3_freqs[o3_idx][sym as usize]; let t = o3_totals[o3_idx]; (actual_prob - c as f64 / t.max(1) as f64).abs() } else { 0.5 };
        let o4_err_delta = if o4_reliable { let c = o4_freqs[o4_idx][sym as usize]; let t = o4_totals[o4_idx]; (actual_prob - c as f64 / t.max(1) as f64).abs() } else { 0.5 };
        let o5_err_delta = if o5_reliable { let c = o5_freqs[o5_idx][sym as usize]; let t = o5_totals[o5_idx]; (actual_prob - c as f64 / t.max(1) as f64).abs() } else { 0.5 };
        let o6_err_delta = if o6_reliable { let c = o6_freqs[o6_idx][sym as usize]; let t = o6_totals[o6_idx]; (actual_prob - c as f64 / t.max(1) as f64).abs() } else { 0.5 };
        let o7_err_delta = if o7_reliable { let c = o7_freqs[o7_idx][sym as usize]; let t = o7_totals[o7_idx]; (actual_prob - c as f64 / t.max(1) as f64).abs() } else { 0.5 };
        
        o0_err = (1.0 - EWMA_ALPHA) * o0_err + EWMA_ALPHA * o0_err_delta;
        o1_err = (1.0 - EWMA_ALPHA) * o1_err + EWMA_ALPHA * o1_err_delta;
        o2_err = (1.0 - EWMA_ALPHA) * o2_err + EWMA_ALPHA * o2_err_delta;
        o3_err = (1.0 - EWMA_ALPHA) * o3_err + EWMA_ALPHA * o3_err_delta;
        o4_err = (1.0 - EWMA_ALPHA) * o4_err + EWMA_ALPHA * o4_err_delta;
        o5_err = (1.0 - EWMA_ALPHA) * o5_err + EWMA_ALPHA * o5_err_delta;
        o6_err = (1.0 - EWMA_ALPHA) * o6_err + EWMA_ALPHA * o6_err_delta;
        o7_err = (1.0 - EWMA_ALPHA) * o7_err + EWMA_ALPHA * o7_err_delta;
        
        // Update models
        o0_freqs[sym as usize] += 1; o0_total += 1;
        o1_freqs[o1_ctx][sym as usize] += 1; o1_totals[o1_ctx] += 1;
        o2_freqs[o2_ctx][sym as usize] += 1; o2_totals[o2_ctx] += 1;
        o3_freqs[o3_idx][sym as usize] += 1; o3_totals[o3_idx] += 1;
        o4_freqs[o4_idx][sym as usize] += 1; o4_totals[o4_idx] += 1;
        o5_freqs[o5_idx][sym as usize] += 1; o5_totals[o5_idx] += 1;
        o6_freqs[o6_idx][sym as usize] += 1; o6_totals[o6_idx] += 1;
        o7_freqs[o7_idx][sym as usize] += 1; o7_totals[o7_idx] += 1;
        
        prev7 = prev6; prev6 = prev5; prev5 = prev4;
        prev4 = prev3; prev3 = prev2; prev2 = prev1; prev1 = sym;
    }
    
    Ok(out)
}

pub fn range_encode_bytes_order_ewma(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        let mut out = Vec::new();
        out.push(6); // flag: O0+O1+O2 EWMA
        out.push(0); out.push(0); out.push(0); out.push(0);
        return out;
    }
    
    let mut enc = RangeEncoder::new();
    
    // O0 model
    let mut o0_freqs = [1u64; 256];
    let mut o0_total: u64 = 256;
    
    // O1 model
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    
    // O2 model
    let mut o2_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; 256 * 256];
    let mut o2_totals: Vec<u64> = vec![256u64; 256 * 256];
    
    // EWMA error tracking (lower = better model)
    let mut o0_err: f64 = 0.5;
    let mut o1_err: f64 = 0.5;
    let mut o2_err: f64 = 0.5;
    
    let mut prev1 = 0u8;
    let mut prev2 = 0u8;
    
    for &b in data {
        let sym = b as usize;
        let o1_ctx = prev1 as usize;
        let o2_ctx = (prev1 as usize) * 256 + (prev2 as usize);
        
        // Compute inverse-error weights (higher = better model = more weight)
        let inv_o0 = 1.0 / (o0_err + 0.001);
        let inv_o1 = 1.0 / (o1_err + 0.001);
        let inv_o2 = 1.0 / (o2_err + 0.001);
        let inv_sum = inv_o0 + inv_o1 + inv_o2;
        
        // Convert to integer weights (scaled by 100 for precision)
        let w0 = ((inv_o0 / inv_sum) * 100.0) as u64;
        let w1 = ((inv_o1 / inv_sum) * 100.0) as u64;
        let w2 = ((inv_o2 / inv_sum) * 100.0) as u64;
        
        // Compute cumulative frequencies with weighting
        let o0_cum: u64 = o0_freqs[..sym].iter().sum();
        let o1_cum: u64 = o1_freqs[o1_ctx][..sym].iter().sum();
        let o2_cum: u64 = o2_freqs[o2_ctx][..sym].iter().sum();
        
        let mixed_cum = o0_cum * w0 + o1_cum * w1 + o2_cum * w2;
        
        let o0_count = o0_freqs[sym];
        let o1_count = o1_freqs[o1_ctx][sym];
        let o2_count = o2_freqs[o2_ctx][sym];
        
        let mixed_count = o0_count * w0 + o1_count * w1 + o2_count * w2;
        let mixed_total = o0_total * w0 + o1_totals[o1_ctx] * w1 + o2_totals[o2_ctx] * w2;
        
        // Encode
        let total = mixed_total.max(1);
        let cum = mixed_cum.min(total - 1);
        let freq = mixed_count.max(1).min(total - cum);
        
        enc.encode(cum, freq, total);
        
        // Compute actual probability and update EWMA errors
        let actual_prob = freq as f64 / total as f64;
        let symbol_prob = 1.0 / 256.0;
        let o0_err_delta = (actual_prob - symbol_prob).abs();
        let o1_err_delta = (actual_prob - o1_freqs[o1_ctx][sym] as f64 / (o1_totals[o1_ctx] as f64).max(1.0)).abs();
        let o2_err_delta = (actual_prob - o2_freqs[o2_ctx][sym] as f64 / o2_totals[o2_ctx].max(1) as f64).abs();
        
        o0_err = (1.0 - EWMA_ALPHA) * o0_err + EWMA_ALPHA * o0_err_delta;
        o1_err = (1.0 - EWMA_ALPHA) * o1_err + EWMA_ALPHA * o1_err_delta;
        o2_err = (1.0 - EWMA_ALPHA) * o2_err + EWMA_ALPHA * o2_err_delta;
        
        // Update all models
        o0_freqs[sym] += 1;
        o0_total += 1;
        o1_freqs[o1_ctx][sym] += 1;
        o1_totals[o1_ctx] += 1;
        o2_freqs[o2_ctx][sym] += 1;
        o2_totals[o2_ctx] += 1;
        
        prev2 = prev1;
        prev1 = b;
    }
    
    let mut out = Vec::new();
    out.push(6); // flag: O0+O1+O2 EWMA
    
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_le_bytes());
    
    out.extend(enc.flush());
    out
}

/// Decode data encoded with O0+O1+O2 EWMA mixing.
pub fn range_decode_bytes_order_ewma(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 5 {
        return Err("Range decode: data too short");
    }
    
    let flag = data[0];
    if flag != 6 {
        return Err("Range decode: not O0+O1+O2 EWMA encoded");
    }
    
    let count = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    let range_data = &data[5..];
    
    let mut dec = RangeDecoder::new(range_data);
    let mut out = Vec::with_capacity(count);
    
    let mut o0_freqs = [1u64; 256];
    let mut o0_total: u64 = 256;
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    let mut o2_freqs: Vec<[u64; 256]> = vec![[1u64; 256]; 256 * 256];
    let mut o2_totals: Vec<u64> = vec![256u64; 256 * 256];
    
    let mut o0_err: f64 = 0.5;
    let mut o1_err: f64 = 0.5;
    let mut o2_err: f64 = 0.5;
    
    let mut prev1 = 0u8;
    let mut prev2 = 0u8;
    
    for _ in 0..count {
        let o1_ctx = prev1 as usize;
        let o2_ctx = (prev1 as usize) * 256 + (prev2 as usize);
        
        let inv_o0 = 1.0 / (o0_err + 0.001);
        let inv_o1 = 1.0 / (o1_err + 0.001);
        let inv_o2 = 1.0 / (o2_err + 0.001);
        let inv_sum = inv_o0 + inv_o1 + inv_o2;
        
        let w0 = ((inv_o0 / inv_sum) * 100.0) as u64;
        let w1 = ((inv_o1 / inv_sum) * 100.0) as u64;
        let w2 = ((inv_o2 / inv_sum) * 100.0) as u64;
        
        let mixed_total = o0_total * w0 + o1_totals[o1_ctx] * w1 + o2_totals[o2_ctx] * w2;
        let f = dec.get_freq(mixed_total.max(1));
        
        // Find symbol
        let mut sym = 0u8;
        let mut cum = 0u64;
        
        for s in 0..256 {
            let o0_cum = o0_freqs[..s].iter().sum::<u64>();
            let o1_cum = o1_freqs[o1_ctx][..s].iter().sum::<u64>();
            let o2_cum = o2_freqs[o2_ctx][..s].iter().sum::<u64>();
            
            let next_cum = o0_cum * w0 + o1_cum * w1 + o2_cum * w2
                + o0_freqs[s] * w0 + o1_freqs[o1_ctx][s] * w1 + o2_freqs[o2_ctx][s] * w2;
            
            if cum <= f && f < next_cum {
                sym = s as u8;
                break;
            }
            cum = next_cum;
        }
        
        let o0_cum = o0_freqs[..sym as usize].iter().sum::<u64>();
        let o1_cum = o1_freqs[o1_ctx][..sym as usize].iter().sum::<u64>();
        let o2_cum = o2_freqs[o2_ctx][..sym as usize].iter().sum::<u64>();
        
        let dec_cum = o0_cum * w0 + o1_cum * w1 + o2_cum * w2;
        let dec_freq = o0_freqs[sym as usize] * w0 + o1_freqs[o1_ctx][sym as usize] * w1 + o2_freqs[o2_ctx][sym as usize] * w2;
        
        dec.decode(dec_cum, dec_freq.max(1), mixed_total.max(1));
        
        out.push(sym);
        
        // Update errors
        let total = mixed_total.max(1);
        let actual_prob = dec_freq as f64 / total as f64;
        let symbol_prob = 1.0 / 256.0;
        let o0_err_delta = (actual_prob - symbol_prob).abs();
        let o1_err_delta = (actual_prob - o1_freqs[o1_ctx][sym as usize] as f64 / (o1_totals[o1_ctx] as f64).max(1.0)).abs();
        let o2_err_delta = (actual_prob - o2_freqs[o2_ctx][sym as usize] as f64 / o2_totals[o2_ctx].max(1) as f64).abs();
        
        o0_err = (1.0 - EWMA_ALPHA) * o0_err + EWMA_ALPHA * o0_err_delta;
        o1_err = (1.0 - EWMA_ALPHA) * o1_err + EWMA_ALPHA * o1_err_delta;
        o2_err = (1.0 - EWMA_ALPHA) * o2_err + EWMA_ALPHA * o2_err_delta;
        
        // Update models
        o0_freqs[sym as usize] += 1;
        o0_total += 1;
        o1_freqs[o1_ctx][sym as usize] += 1;
        o1_totals[o1_ctx] += 1;
        o2_freqs[o2_ctx][sym as usize] += 1;
        o2_totals[o2_ctx] += 1;
        
        prev2 = prev1;
        prev1 = sym;
    }
    
    Ok(out)
}

/// Adaptive order-0/1 mixing range coder.
/// Blends order-0 and order-1 probability estimates using weighted integer arithmetic.
/// O1 frequencies get WEIGHT multiplier for better text compression.
const MIX_WEIGHT: u64 = 2; // O1 counts are weighted 2x

pub fn range_encode_bytes_order_mix(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        let mut out = Vec::new();
        out.push(3); // flag: mixed order
        out.push(0); out.push(0); out.push(0); out.push(0); // length = 0
        return out;
    }
    
    let mut enc = RangeEncoder::new();
    
    // Order-0 model: uniform start with smoothing
    let mut o0_freqs = [1u64; 256];
    let mut o0_total: u64 = 256;
    
    // Order-1 model: [context][symbol] = frequency
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    
    let mut prev = 0u8;
    
    for &b in data {
        let sym = b as usize;
        let ctx = prev as usize;
        
        // Integer-weighted mixing: o0_freq + o1_freq * WEIGHT
        // This is deterministic and avoids floating-point errors
        let mixed_cum: u64 = o0_freqs[..sym].iter().sum::<u64>() 
            + o1_freqs[ctx][..sym].iter().sum::<u64>() * MIX_WEIGHT;
        let mixed_count = o0_freqs[sym] + o1_freqs[ctx][sym] * MIX_WEIGHT;
        let mixed_total = o0_total + o1_totals[ctx] * MIX_WEIGHT;
        
        // Encode with range coder
        let total = mixed_total.max(1);
        let cum = mixed_cum.min(total - 1);
        let freq = mixed_count.max(1).min(total - cum);
        
        enc.encode(cum, freq, total);
        
        // Update both models
        o0_freqs[sym] += 1;
        o0_total += 1;
        o1_freqs[ctx][sym] += 1;
        o1_totals[ctx] += 1;
        
        prev = b;
    }
    
    let mut out = Vec::new();
    out.push(3); // flag: mixed order
    
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_le_bytes());
    
    out.extend(enc.flush());
    out
}

/// Decode data encoded with adaptive order mixing.
pub fn range_decode_bytes_order_mix(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 5 {
        return Err("Range decode: data too short");
    }
    
    let flag = data[0];
    if flag != 3 {
        return Err("Range decode: not mixed order encoded");
    }
    
    let count = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    let range_data = &data[5..];
    
    let mut dec = RangeDecoder::new(range_data);
    let mut out = Vec::with_capacity(count);
    
    let mut o0_freqs = [1u64; 256];
    let mut o0_total: u64 = 256;
    let mut o1_freqs = [[1u64; 256]; 256];
    let mut o1_totals = [256u64; 256];
    
    let mut prev = 0u8;
    
    for _ in 0..count {
        let ctx = prev as usize;
        
        // Integer-weighted mixing: o0_freq + o1_freq * WEIGHT
        let mixed_total = o0_total + o1_totals[ctx] * MIX_WEIGHT;
        let f = dec.get_freq(mixed_total.max(1));
        
        // Find symbol matching cumulative frequency f
        let mut sym = 0u8;
        let mut cum = 0u64;
        
        for s in 0..256 {
            let o0_cum = o0_freqs[..s].iter().sum::<u64>();
            let o1_cum = o1_freqs[ctx][..s].iter().sum::<u64>();
            let next_cum = o0_cum + o1_cum * MIX_WEIGHT + o0_freqs[s] + o1_freqs[ctx][s] * MIX_WEIGHT;
            
            if cum <= f && f < next_cum {
                sym = s as u8;
                break;
            }
            cum = next_cum;
        }
        
        // Decode update
        let o0_cum = o0_freqs[..sym as usize].iter().sum::<u64>();
        let o1_cum = o1_freqs[ctx][..sym as usize].iter().sum::<u64>();
        let dec_cum = o0_cum + o1_cum * MIX_WEIGHT;
        let dec_freq = o0_freqs[sym as usize] + o1_freqs[ctx][sym as usize] * MIX_WEIGHT;
        
        dec.decode(dec_cum, dec_freq.max(1), mixed_total.max(1));
        
        out.push(sym);
        
        o0_freqs[sym as usize] += 1;
        o0_total += 1;
        o1_freqs[ctx][sym as usize] += 1;
        o1_totals[ctx] += 1;
        
        prev = sym;
    }
    
    Ok(out)
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
    
    #[test]
    fn test_range_encode_bytes_empty() {
        let encoded = range_encode_bytes(&[]);
        // Empty data still produces flush bits; count prefix is 0
        let decoded = range_decode_bytes(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, Vec::<u8>::new());
    }
    
    #[test]
    fn test_range_encode_bytes_simple() {
        let data = b"hello world";
        let encoded = range_encode_bytes(data);
        let decoded = range_decode_bytes(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_range_encode_bytes_all_bytes() {
        let data: Vec<u8> = (0u8..=255).collect();
        let encoded = range_encode_bytes(&data);
        let decoded = range_decode_bytes(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_range_encode_bytes_skewed() {
        // Skewed data: mostly zeros, a few non-zero
        let mut data = vec![0u8; 100];
        data.push(1);
        data.push(2);
        let encoded = range_encode_bytes(&data);
        let decoded = range_decode_bytes(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_range_encode_bytes_roundtrip() {
        let data = b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.";
        let encoded = range_encode_bytes(data);
        let decoded = range_decode_bytes(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    // Order-1 context model tests
    
    #[test]
    fn test_order1_roundtrip_empty() {
        let encoded = range_encode_bytes_order1(&[]);
        let decoded = range_decode_bytes_order1(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, Vec::<u8>::new());
    }
    
    #[test]
    fn test_order1_roundtrip_simple() {
        let data = b"hello world";
        let encoded = range_encode_bytes_order1(data);
        let decoded = range_decode_bytes_order1(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order1_roundtrip_repetitive() {
        // Order-1 should excel at repetitive patterns
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = range_encode_bytes_order1(&data);
        let decoded = range_decode_bytes_order1(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order1_vs_order0() {
        // Compare order-1 vs order-0 compression
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        
        let enc_order0 = range_encode_bytes(&data);
        let enc_order1 = range_encode_bytes_order1(&data);
        
        println!("Order-0: {} bytes", enc_order0.len());
        println!("Order-1: {} bytes", enc_order1.len());
        println!("Improvement: {}%", 100.0 * (1.0 - enc_order1.len() as f64 / enc_order0.len() as f64));
        
        // Order-1 should be smaller
        assert!(enc_order1.len() < enc_order0.len(), "Order-1 should compress better");
    }
    
    // Order-2 context model tests
    
    #[test]
    fn test_order2_roundtrip_empty() {
        let encoded = range_encode_bytes_order2(&[]);
        let decoded = range_decode_bytes_order2(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, Vec::<u8>::new());
    }
    
    #[test]
    fn test_order2_roundtrip_simple() {
        let data = b"hello world";
        let encoded = range_encode_bytes_order2(data);
        let decoded = range_decode_bytes_order2(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order2_roundtrip_repetitive() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = range_encode_bytes_order2(&data);
        let decoded = range_decode_bytes_order2(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order2_vs_order1() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        
        let enc_order1 = range_encode_bytes_order1(&data);
        let enc_order2 = range_encode_bytes_order2(&data);
        
        println!("Order-1: {} bytes", enc_order1.len());
        println!("Order-2: {} bytes", enc_order2.len());
        println!("Order-2 vs O1: {:.1}%", 100.0 * (1.0 - enc_order2.len() as f64 / enc_order1.len() as f64));
        
        // Order-2 may or may not be better depending on data
    }
    
    // Order-mix (O0+O1) tests
    
    #[test]
    fn test_order_mix_roundtrip_simple() {
        let data = b"hello world";
        let encoded = range_encode_bytes_order_mix(data);
        let decoded = range_decode_bytes_order_mix(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_mix_roundtrip_repetitive() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = range_encode_bytes_order_mix(&data);
        let decoded = range_decode_bytes_order_mix(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_mix_vs_order1() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        
        let enc_order1 = range_encode_bytes_order1(&data);
        let enc_mix = range_encode_bytes_order_mix(&data);
        
        println!("Order-1: {} bytes", enc_order1.len());
        println!("Order-mix: {} bytes", enc_mix.len());
        println!("Mix vs O1: {:.1}%", 100.0 * (1.0 - enc_mix.len() as f64 / enc_order1.len() as f64));
    }
    
    #[test]
    fn test_order_mix_vs_order0() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        
        let enc_order0 = range_encode_bytes(&data);
        let enc_mix = range_encode_bytes_order_mix(&data);
        
        println!("Order-0: {} bytes", enc_order0.len());
        println!("Order-mix: {} bytes", enc_mix.len());
        println!("Mix vs O0: {:.1}%", 100.0 * (1.0 - enc_mix.len() as f64 / enc_order0.len() as f64));
    }
    
    // Order-1+2 mix tests
    
    #[test]
    fn test_order12_mix_roundtrip_simple() {
        let data = b"hello world";
        let encoded = range_encode_bytes_order12_mix(data);
        let decoded = range_decode_bytes_order12_mix(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order12_mix_roundtrip_repetitive() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = range_encode_bytes_order12_mix(&data);
        let decoded = range_decode_bytes_order12_mix(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order12_mix_vs_order2() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        
        let enc_o2 = range_encode_bytes_order2(&data);
        let enc_o12 = range_encode_bytes_order12_mix(&data);
        
        println!("Order-2: {} bytes", enc_o2.len());
        println!("Order-1+2 mix: {} bytes", enc_o12.len());
        println!("O1+2 vs O2: {:.1}%", 100.0 * (1.0 - enc_o12.len() as f64 / enc_o2.len() as f64));
    }
    
    #[test]
    fn test_order12_mix_vs_order_mix() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        
        let enc_o01 = range_encode_bytes_order_mix(&data);
        let enc_o12 = range_encode_bytes_order12_mix(&data);
        
        println!("Order-0+1 mix: {} bytes", enc_o01.len());
        println!("Order-1+2 mix: {} bytes", enc_o12.len());
    }
    
    // O0+O1+O2 EWMA tests
    
    #[test]
    fn test_order_ewma_roundtrip_simple() {
        let data = b"hello world";
        let encoded = range_encode_bytes_order_ewma(data);
        let decoded = range_decode_bytes_order_ewma(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma_roundtrip_repetitive() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = range_encode_bytes_order_ewma(&data);
        let decoded = range_decode_bytes_order_ewma(&encoded).expect("range decode should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma_vs_order_mix() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        
        let enc_mix = range_encode_bytes_order_mix(&data);
        let enc_ewma = range_encode_bytes_order_ewma(&data);
        
        println!("Order-0+1 mix: {} bytes", enc_mix.len());
        println!("Order-EWMA (O0+O1+O2): {} bytes", enc_ewma.len());
        println!("EWMA vs mix: {:.1}%", 100.0 * (1.0 - enc_ewma.len() as f64 / enc_mix.len() as f64));
    }
    
    // === Order-3 EWMA tests (sparse table) ===
    
    #[test]
    fn test_order_ewma3_roundtrip_simple() {
        let data = b"hello world";
        let encoded = range_encode_bytes_order_ewma3(data);
        let decoded = range_decode_bytes_order_ewma3(&encoded).expect("range decode O3 should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma3_roundtrip_repetitive() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = range_encode_bytes_order_ewma3(&data);
        let decoded = range_decode_bytes_order_ewma3(&encoded).expect("range decode O3 should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma3_roundtrip_empty() {
        let data: Vec<u8> = vec![];
        let encoded = range_encode_bytes_order_ewma3(&data);
        let decoded = range_decode_bytes_order_ewma3(&encoded).expect("range decode O3 empty should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma3_roundtrip_random() {
        // Generate deterministic pseudo-random data via LCG
        let seed: u64 = 42;
        let mut data = Vec::with_capacity(500);
        let mut h = seed;
        for _ in 0..500 {
            h = h.wrapping_mul(6364136223846793005).wrapping_add(1);
            data.push((h >> 40) as u8);
        }
        
        let encoded = range_encode_bytes_order_ewma3(&data);
        let decoded = range_decode_bytes_order_ewma3(&encoded).expect("range decode O3 random should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma3_vs_ewma2() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        
        let enc_ewma2 = range_encode_bytes_order_ewma(&data);
        let enc_ewma3 = range_encode_bytes_order_ewma3(&data);
        
        println!("Order-EWMA (O0+O1+O2): {} bytes", enc_ewma2.len());
        println!("Order-EWMA (O0+O1+O2+O3): {} bytes", enc_ewma3.len());
        println!("O3 vs O2: {:.1}%", 100.0 * (1.0 - enc_ewma3.len() as f64 / enc_ewma2.len() as f64));
    }
    
    // === Order-5 EWMA tests (O0+O1+O2+O3+O4+O5 with sparse tables) ===
    
    #[test]
    fn test_order_ewma5_roundtrip_simple() {
        let data = b"hello world";
        let encoded = range_encode_bytes_order_ewma5(data);
        let decoded = range_decode_bytes_order_ewma5(&encoded).expect("range decode O5 should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma5_roundtrip_repetitive() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = range_encode_bytes_order_ewma5(&data);
        let decoded = range_decode_bytes_order_ewma5(&encoded).expect("range decode O5 should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma5_roundtrip_empty() {
        let data: Vec<u8> = vec![];
        let encoded = range_encode_bytes_order_ewma5(&data);
        let decoded = range_decode_bytes_order_ewma5(&encoded).expect("range decode O5 empty should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma5_roundtrip_random() {
        let seed: u64 = 42;
        let mut data = Vec::with_capacity(500);
        let mut h = seed;
        for _ in 0..500 {
            h = h.wrapping_mul(6364136223846793005).wrapping_add(1);
            data.push((h >> 40) as u8);
        }
        
        let encoded = range_encode_bytes_order_ewma5(&data);
        let decoded = range_decode_bytes_order_ewma5(&encoded).expect("range decode O5 random should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma5_vs_ewma3() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        
        let enc_ewma3 = range_encode_bytes_order_ewma3(&data);
        let enc_ewma5 = range_encode_bytes_order_ewma5(&data);
        
        println!("Order-EWMA (O0+O1+O2+O3): {} bytes", enc_ewma3.len());
        println!("Order-EWMA (O0+O1+O2+O3+O4+O5): {} bytes", enc_ewma5.len());
        println!("O5 vs O3: {:.1}%", 100.0 * (1.0 - enc_ewma5.len() as f64 / enc_ewma3.len() as f64));
    }
    
    // === Order-7 EWMA tests (O0..O7 with sparse tables) ===
    
    #[test]
    fn test_order_ewma7_roundtrip_simple() {
        let data = b"hello world";
        let encoded = range_encode_bytes_order_ewma7(data);
        let decoded = range_decode_bytes_order_ewma7(&encoded).expect("range decode O7 should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma7_roundtrip_repetitive() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        let encoded = range_encode_bytes_order_ewma7(&data);
        let decoded = range_decode_bytes_order_ewma7(&encoded).expect("range decode O7 should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma7_roundtrip_empty() {
        let data: Vec<u8> = vec![];
        let encoded = range_encode_bytes_order_ewma7(&data);
        let decoded = range_decode_bytes_order_ewma7(&encoded).expect("range decode O7 empty should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma7_roundtrip_random() {
        let seed: u64 = 42;
        let mut data = Vec::with_capacity(500);
        let mut h = seed;
        for _ in 0..500 {
            h = h.wrapping_mul(6364136223846793005).wrapping_add(1);
            data.push((h >> 40) as u8);
        }
        let encoded = range_encode_bytes_order_ewma7(&data);
        let decoded = range_decode_bytes_order_ewma7(&encoded).expect("range decode O7 random should succeed");
        assert_eq!(decoded, data);
    }
    
    #[test]
    fn test_order_ewma7_vs_ewma5() {
        let data: Vec<u8> = b"The quick brown fox jumps over the lazy dog. ".iter()
            .cycle().take(1000).copied().collect();
        
        let enc_ewma5 = range_encode_bytes_order_ewma5(&data);
        let enc_ewma7 = range_encode_bytes_order_ewma7(&data);
        
        println!("Order-EWMA (O0..O5): {} bytes", enc_ewma5.len());
        println!("Order-EWMA (O0..O7): {} bytes", enc_ewma7.len());
        println!("O7 vs O5: {:.1}%", 100.0 * (1.0 - enc_ewma7.len() as f64 / enc_ewma5.len() as f64));
    }
}
