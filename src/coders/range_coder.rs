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
}
