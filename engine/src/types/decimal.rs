//! An exact decimal number: an integer (`unscaled`) divided by ten to the power `scale`. Up to 38 digits, like most SQL databases, so the unscaled
//! part always fits an `i128` with room to spare.

use crate::error::{Error, Result};
use std::cmp::Ordering;

pub const MAX_DIGITS: u32 = 38;
pub const MAX_SCALE: u8 = 38;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decimal {
    pub unscaled: i128,
    pub scale: u8,
}

fn pow10(n: u32) -> i128 {
    10i128.pow(n)
}

impl Decimal {
    pub fn new(unscaled: i128, scale: u8) -> Result<Self> {
        if scale > MAX_SCALE {
            return Err(Error::Invalid(format!(
                "a decimal's scale is at most {MAX_SCALE}"
            )));
        }
        if unscaled.unsigned_abs() >= pow10(MAX_DIGITS) as u128 {
            return Err(Error::Invalid(format!(
                "a decimal has at most {MAX_DIGITS} digits"
            )));
        }
        Ok(Self { unscaled, scale })
    }

    /// The same number with no trailing zeros in the fraction: 1.50 becomes 1.5, 100 stays 100. Equal numbers have equal normal forms.
    pub fn normalized(self) -> Self {
        let (mut u, mut s) = (self.unscaled, self.scale);
        while s > 0 && u % 10 == 0 {
            u /= 10;
            s -= 1;
        }
        if u == 0 {
            s = 0;
        }
        Self {
            unscaled: u,
            scale: s,
        }
    }

    /// Sign, decimal exponent and significant digits: the number is `0.d1 d2 ... dn * 10^exponent` with `d1 != 0` and no trailing zero digit.
    /// Zero has sign 0, exponent 0 and no digits. Two decimals are numerically equal exactly when their canonical forms are.
    pub fn canonical(&self) -> (i8, i32, Vec<u8>) {
        if self.unscaled == 0 {
            return (0, 0, vec![]);
        }
        let sign = if self.unscaled < 0 { -1 } else { 1 };
        let mut digits: Vec<u8> = self
            .unscaled
            .unsigned_abs()
            .to_string()
            .bytes()
            .map(|b| b - b'0')
            .collect();
        let exponent = digits.len() as i32 - self.scale as i32;
        while digits.last() == Some(&0) {
            digits.pop();
        }
        (sign, exponent, digits)
    }

    /// Numeric comparison (`1.5` equals `1.50`).
    pub fn cmp_numeric(&self, other: &Self) -> Ordering {
        let (sa, ea, da) = self.canonical();
        let (sb, eb, db) = other.canonical();
        if sa != sb {
            return sa.cmp(&sb);
        }
        if sa == 0 {
            return Ordering::Equal;
        }
        let magnitude = ea.cmp(&eb).then_with(|| da.cmp(&db)); // a shorter digit string that is a prefix is the smaller number
        if sa > 0 {
            magnitude
        } else {
            magnitude.reverse()
        }
    }

    /// Builds a decimal from a canonical form (the inverse of `canonical`).
    pub fn from_canonical(sign: i8, exponent: i32, digits: &[u8]) -> Result<Self> {
        if sign == 0 || digits.is_empty() {
            return Ok(Self {
                unscaled: 0,
                scale: 0,
            });
        }
        if digits.len() as u32 > MAX_DIGITS || digits.iter().any(|d| *d > 9) || digits[0] == 0 {
            return Err(Error::Corrupt("a decimal's digits".into()));
        }
        let mut u: i128 = 0;
        for d in digits {
            u = u * 10 + *d as i128;
        }
        // value = 0.digits * 10^exponent = u * 10^(exponent - n)
        let shift = exponent - digits.len() as i32;
        let (mut unscaled, scale) = if shift >= 0 {
            let s = shift as u32;
            if digits.len() as u32 + s > MAX_DIGITS {
                return Err(Error::Corrupt("a decimal is too large".into()));
            }
            (u * pow10(s), 0u8)
        } else {
            let s = (-shift) as u32;
            if s > MAX_SCALE as u32 {
                return Err(Error::Corrupt("a decimal's scale".into()));
            }
            (u, s as u8)
        };
        if sign < 0 {
            unscaled = -unscaled;
        }
        Ok(Self { unscaled, scale })
    }
}

impl std::fmt::Display for Decimal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let neg = self.unscaled < 0;
        let digits = self.unscaled.unsigned_abs().to_string();
        let s = self.scale as usize;
        let body = if s == 0 {
            digits
        } else if digits.len() > s {
            format!(
                "{}.{}",
                &digits[..digits.len() - s],
                &digits[digits.len() - s..]
            )
        } else {
            format!("0.{}{}", "0".repeat(s - digits.len()), digits)
        };
        write!(f, "{}{body}", if neg { "-" } else { "" })
    }
}
