// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Four-state constants and checked bit widths.
//!
//! [`ConstBits`] stores bits least-significant first and preserves `X` and `Z`
//! rather than coercing them to Boolean values. Conversions that require a
//! two-state integer fail when an unknown or high-impedance bit is present.
//! [`Width`] is the shared scalar width type used at IR phase boundaries.

use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// One bit in `SystemVerilog`'s four-state value domain.
pub enum BitVal {
    /// Logical zero.
    Zero,
    /// Logical one.
    One,
    /// Unknown value.
    X,
    /// High-impedance value.
    Z,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Bit width shared across IR phase boundaries.
pub struct Width(pub u32);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
/// Fixed-width four-state constant in most-significant-bit-first display order.
pub struct ConstBits {
    bits: Vec<BitVal>,
}
impl ConstBits {
    /// Parses a binary string containing `0`, `1`, `x`, or `z`.
    ///
    /// The first character is the most-significant bit. Other characters are
    /// conservatively represented as unknown bits.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::WidthOverflow`] if the string contains more bits
    /// than the IR's 32-bit width representation can address.
    pub fn from_bin_str(s: &str) -> Result<Self, ValueError> {
        Self::from_bits(
            s.chars()
                .map(|c| match c {
                    '0' => BitVal::Zero,
                    '1' => BitVal::One,
                    'z' | 'Z' => BitVal::Z,
                    _ => BitVal::X,
                })
                .collect(),
        )
    }

    /// Creates a constant from bits in display order, most significant first.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::WidthOverflow`] if `bits` exceeds the IR's 32-bit
    /// width representation.
    pub fn from_bits(bits: Vec<BitVal>) -> Result<Self, ValueError> {
        u32::try_from(bits.len()).map_err(|_| ValueError::WidthOverflow)?;
        Ok(Self { bits })
    }

    /// Returns the constant width in bits.
    ///
    /// # Panics
    ///
    /// Panics only if the private bit vector bypassed the checked constructors;
    /// safe construction and deserialization preserve this invariant.
    #[must_use]
    pub fn width(&self) -> u32 {
        u32::try_from(self.bits.len()).expect("validated constant width must fit in u32")
    }

    /// Returns the bits in most-significant-bit-first display order.
    #[must_use]
    pub fn as_slice(&self) -> &[BitVal] {
        &self.bits
    }

    /// Returns a bit indexed from the least-significant end.
    #[must_use]
    pub fn bit_lsb(&self, index: u32) -> Option<BitVal> {
        let index = usize::try_from(index).ok()?;
        let position = self.bits.len().checked_sub(index.checked_add(1)?)?;
        self.bits.get(position).copied()
    }
}

impl<'de> Deserialize<'de> for ConstBits {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Representation {
            bits: Vec<BitVal>,
        }

        let representation = Representation::deserialize(deserializer)?;
        Self::from_bits(representation.bits).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Failure while constructing a four-state constant.
pub enum ValueError {
    /// The bit vector cannot be represented by the 32-bit IR width.
    WidthOverflow,
}

impl fmt::Display for ValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WidthOverflow => formatter.write_str("constant width exceeds 32-bit capacity"),
        }
    }
}

impl std::error::Error for ValueError {}
impl fmt::Display for ConstBits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.bits {
            let ch = match b {
                BitVal::Zero => '0',
                BitVal::One => '1',
                BitVal::X => 'x',
                BitVal::Z => 'z',
            };
            write!(f, "{ch}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_constants_from_the_least_significant_bit() {
        let bits = ConstBits::from_bin_str("10xz").unwrap();

        assert_eq!(bits.bit_lsb(0), Some(BitVal::Z));
        assert_eq!(bits.bit_lsb(1), Some(BitVal::X));
        assert_eq!(bits.bit_lsb(2), Some(BitVal::Zero));
        assert_eq!(bits.bit_lsb(3), Some(BitVal::One));
        assert_eq!(bits.bit_lsb(4), None);
    }
}
