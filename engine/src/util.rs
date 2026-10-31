use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

pub fn from_hex_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

pub fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("the operating system's random number generator failed");
    u64::from_le_bytes(b)
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the operating system's random number generator failed");
    b
}

/// A time-ordered record id: 48 bits of milliseconds, a 16-bit counter that keeps ids from one process strictly increasing, and 64 random bits.
/// 32 lowercase hex characters, so lexicographic order is creation order.
pub fn new_id() -> String {
    static LAST: Mutex<(u64, u16)> = Mutex::new((0, 0));
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let mut ms = now_ms() as u64 & 0xFFFF_FFFF_FFFF;
    let counter;
    if ms > last.0 {
        counter = (random_u64() & 0x7FFF) as u16; // start low enough that a burst cannot overflow within one millisecond
    } else {
        ms = last.0;
        if last.1 == u16::MAX {
            ms += 1; // 65k ids in one millisecond: borrow from the next one
            counter = 0;
        } else {
            counter = last.1 + 1;
        }
    }
    *last = (ms, counter);
    format!("{ms:012x}{counter:04x}{:016x}", random_u64())
}
