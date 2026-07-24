//! Cross-platform compatibility for the 96-bit `.NET` `System.Decimal`.
//!
//! `KartRider`'s P5136 data path parses XML through invariant-culture
//! `NumberStyles.Any`, performs decimal addition and multiplication, and only
//! then casts to its wire-domain integer and float fields. Native Rust float
//! arithmetic can produce different final `f32` bits, so this module keeps the
//! relevant decimal representation and rounding rules explicit.

const DOTNET_DECIMAL_MAX_MANTISSA: u128 = (1_u128 << 96) - 1;
const DOTNET_DECIMAL_POWERS_10: [f64; 29] = [
    1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22, 1e23, 1e24, 1e25, 1e26, 1e27, 1e28,
];

/// A finite `.NET` decimal represented by a 96-bit mantissa and scale.
///
/// This intentionally exposes only operations used by the P5136 compatibility
/// layer. Arithmetic returns `None` when `System.Decimal` would overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DotNetDecimal {
    mantissa: u128,
    scale: u32,
    negative: bool,
}

impl DotNetDecimal {
    pub const ZERO: Self = Self {
        mantissa: 0,
        scale: 0,
        negative: false,
    };

    pub const ONE: Self = Self {
        mantissa: 1,
        scale: 0,
        negative: false,
    };

    /// Constructs a decimal from the same three logical parts exposed by
    /// `.NET`'s decimal bit layout.
    ///
    /// Returns `None` for a mantissa wider than 96 bits or a scale above 28.
    #[must_use]
    pub const fn from_parts(mantissa: u128, scale: u32, negative: bool) -> Option<Self> {
        if mantissa > DOTNET_DECIMAL_MAX_MANTISSA || scale > 28 {
            None
        } else {
            Some(Self {
                mantissa,
                scale,
                negative,
            })
        }
    }

    /// Converts a finite `f32` using the same seven-significant-digit shaping
    /// used by an explicit C# `float` to `decimal` conversion.
    ///
    /// Returns `None` when the source is non-finite or exceeds the decimal
    /// range.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn from_f32(value: f32) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }

        let exponent = i32::from(((value.to_bits() >> 23) & 0xff) as u8) - 126;
        if exponent < -94 {
            return Some(Self::ZERO);
        }
        if exponent > 96 {
            return None;
        }

        let negative = value < 0.0;
        let mut scaled = f64::from(if negative { -value } else { value });
        let mut power = 6 - ((exponent * 19_728) >> 16);
        if power >= 0 {
            power = power.min(28);
            scaled *= DOTNET_DECIMAL_POWERS_10[power as usize];
        } else if power != -1 || scaled >= 1e7 {
            scaled /= DOTNET_DECIMAL_POWERS_10[(-power) as usize];
        } else {
            power = 0;
        }
        if scaled < 1e6 && power < 28 {
            scaled *= 10.0;
            power += 1;
        }

        let mut mantissa = scaled.round_ties_even() as u32;
        if mantissa == 0 {
            return Some(Self::ZERO);
        }

        if power < 0 {
            let multiplier = 10_u128.pow((-power) as u32);
            let mantissa = u128::from(mantissa).checked_mul(multiplier)?;
            if mantissa > DOTNET_DECIMAL_MAX_MANTISSA {
                return None;
            }
            return Some(Self {
                mantissa,
                scale: 0,
                negative,
            });
        }

        let mut removable = power.min(6);
        if removable >= 4 && mantissa.is_multiple_of(10_000) {
            mantissa /= 10_000;
            power -= 4;
            removable -= 4;
        }
        if removable >= 2 && mantissa.is_multiple_of(100) {
            mantissa /= 100;
            power -= 2;
            removable -= 2;
        }
        if removable >= 1 && mantissa.is_multiple_of(10) {
            mantissa /= 10;
            power -= 1;
        }
        Some(Self {
            mantissa: u128::from(mantissa),
            scale: power as u32,
            negative,
        })
    }

    /// Parses the invariant-culture decimal grammar accepted by
    /// `decimal.TryParse(value, NumberStyles.Any, InvariantCulture)`.
    ///
    /// Invalid or out-of-range inputs return `None`, allowing callers to apply
    /// the same field-specific fallback used by the C# server.
    #[must_use]
    pub fn parse_invariant(value: &str) -> Option<Self> {
        parse_invariant_decimal(value)
    }

    /// Multiplies two decimals with `.NET` scale reduction and ties-to-even
    /// rounding.
    #[must_use]
    pub fn checked_mul(self, other: Self) -> Option<Self> {
        let product = U256::multiply_u128(self.mantissa, other.mantissa);
        Self::from_wide(
            product,
            self.scale + other.scale,
            self.negative ^ other.negative,
        )
    }

    /// Adds two decimals with `.NET` scale alignment and overflow behavior.
    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        let scale = self.scale.max(other.scale);
        let left = U256::from_u128(self.mantissa).checked_mul_power_of_ten(scale - self.scale)?;
        let right =
            U256::from_u128(other.mantissa).checked_mul_power_of_ten(scale - other.scale)?;
        let (magnitude, negative) = if self.negative == other.negative {
            (left.checked_add(right)?, self.negative)
        } else {
            match left.magnitude_cmp(right) {
                std::cmp::Ordering::Less => (right.checked_sub(left)?, other.negative),
                std::cmp::Ordering::Equal => return Some(Self::ZERO),
                std::cmp::Ordering::Greater => (left.checked_sub(right)?, self.negative),
            }
        };
        Self::from_wide(magnitude, scale, negative)
    }

    /// Converts this decimal to the exactly rounded `f32` used by an explicit
    /// C# cast.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    pub fn to_f32(self) -> f32 {
        if self.mantissa == 0 {
            return if self.negative { -0.0 } else { 0.0 };
        }

        let numerator = U256::from_u128(self.mantissa);
        let denominator = U256::from_u128(10_u128.pow(self.scale));
        let mut exponent = numerator.bit_length() as i32 - denominator.bit_length() as i32;
        let below_power = if exponent >= 0 {
            numerator.magnitude_cmp(
                denominator
                    .checked_shift_left(exponent as u32)
                    .expect("decimal exponent comparison fits U256"),
            ) == std::cmp::Ordering::Less
        } else {
            numerator
                .checked_shift_left((-exponent) as u32)
                .expect("decimal exponent comparison fits U256")
                .magnitude_cmp(denominator)
                == std::cmp::Ordering::Less
        };
        if below_power {
            exponent -= 1;
        }

        let significand_shift = 23 - exponent;
        let (scaled_numerator, scaled_denominator) = if significand_shift >= 0 {
            (
                numerator
                    .checked_shift_left(significand_shift as u32)
                    .expect("decimal-to-f32 numerator fits U256"),
                denominator,
            )
        } else {
            (
                numerator,
                denominator
                    .checked_shift_left((-significand_shift) as u32)
                    .expect("decimal-to-f32 denominator fits U256"),
            )
        };
        let (mut significand, remainder) =
            scaled_numerator.div_rem_u32_quotient(scaled_denominator);
        let doubled_remainder = remainder
            .checked_mul_u64(2)
            .expect("decimal-to-f32 remainder fits U256");
        if doubled_remainder.magnitude_cmp(scaled_denominator) == std::cmp::Ordering::Greater
            || (doubled_remainder == scaled_denominator && !significand.is_multiple_of(2))
        {
            significand += 1;
        }
        if significand == 1 << 24 {
            significand >>= 1;
            exponent += 1;
        }

        debug_assert!((1 << 23..1 << 24).contains(&significand));
        let biased_exponent = (exponent + 127) as u32;
        debug_assert!((1..0xff).contains(&biased_exponent));
        let sign = u32::from(self.negative) << 31;
        f32::from_bits(sign | (biased_exponent << 23) | (significand - (1 << 23)))
    }

    /// Truncates toward zero and returns `None` outside the C# `int` domain.
    #[must_use]
    pub fn to_i32(self) -> Option<i32> {
        let magnitude = self.mantissa / 10_u128.pow(self.scale);
        if self.negative {
            if magnitude == 1_u128 << 31 {
                Some(i32::MIN)
            } else {
                i32::try_from(magnitude).ok().map(|value| -value)
            }
        } else {
            i32::try_from(magnitude).ok()
        }
    }

    /// Truncates toward zero and returns `None` outside the C# `byte` domain.
    #[must_use]
    pub fn to_u8(self) -> Option<u8> {
        if self.negative && self.mantissa != 0 {
            return None;
        }
        u8::try_from(self.mantissa / 10_u128.pow(self.scale)).ok()
    }

    fn from_wide(value: U256, scale: u32, negative: bool) -> Option<Self> {
        let minimum_removed = scale.saturating_sub(28);
        for removed in minimum_removed..=scale {
            let rounded = value.round_div_power_of_ten(removed)?;
            if rounded.fits_decimal_mantissa() {
                let mantissa = rounded.to_u128();
                return if mantissa == 0 {
                    Some(Self::ZERO)
                } else {
                    Some(Self {
                        mantissa,
                        scale: scale - removed,
                        negative,
                    })
                };
            }
        }
        None
    }
}

fn parse_invariant_decimal(input: &str) -> Option<DotNetDecimal> {
    let mut value = input.trim();
    if value.is_empty() || value.matches('¤').count() > 1 {
        return None;
    }

    let mut negative = false;
    let mut sign_seen = false;
    if value.starts_with('(') && value.ends_with(')') {
        negative = true;
        sign_seen = true;
        value = value[1..value.len() - 1].trim();
    } else if value.contains(['(', ')']) {
        return None;
    }

    value = trim_invariant_currency(value);
    if let Some(rest) = value.strip_prefix(['+', '-']) {
        if sign_seen {
            return None;
        }
        negative = value.starts_with('-');
        sign_seen = true;
        value = trim_invariant_currency(rest.trim_start());
    }
    if let Some(rest) = value.strip_suffix(['+', '-']) {
        if sign_seen {
            return None;
        }
        negative = value.ends_with('-');
        value = trim_invariant_currency(rest.trim_end());
    }
    if value.is_empty() {
        return None;
    }

    let mut exponent = 0_i64;
    if let Some(index) = value.find(['e', 'E']) {
        if value[index + 1..].contains(['e', 'E']) {
            return None;
        }
        exponent = value[index + 1..].trim().parse::<i64>().ok()?;
        value = value[..index].trim_end();
    }

    let mut digits = String::with_capacity(value.len());
    let mut fractional_digits = 0_i64;
    let mut decimal_seen = false;
    let mut digit_seen = false;
    for character in value.chars() {
        match character {
            '0'..='9' => {
                digit_seen = true;
                digits.push(character);
                if decimal_seen {
                    fractional_digits = fractional_digits.checked_add(1)?;
                }
            }
            '.' if !decimal_seen => decimal_seen = true,
            ',' if !decimal_seen && digit_seen => {}
            _ => return None,
        }
    }
    if !digit_seen {
        return None;
    }

    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Some(DotNetDecimal::ZERO);
    }
    let scale = fractional_digits.checked_sub(exponent)?;
    decimal_from_digits(significant, scale, negative)
}

fn trim_invariant_currency(mut value: &str) -> &str {
    loop {
        let trimmed = value.trim();
        if let Some(rest) = trimmed.strip_prefix('¤') {
            value = rest;
        } else if let Some(rest) = trimmed.strip_suffix('¤') {
            value = rest;
        } else {
            return trimmed;
        }
    }
}

fn decimal_from_digits(digits: &str, scale: i64, negative: bool) -> Option<DotNetDecimal> {
    if scale < 0 {
        let appended = usize::try_from(scale.checked_neg()?).ok()?;
        if digits.len().checked_add(appended)? > 29 {
            return None;
        }
        let mut expanded = String::with_capacity(digits.len() + appended);
        expanded.push_str(digits);
        expanded.extend(std::iter::repeat_n('0', appended));
        let mantissa = expanded.parse::<u128>().ok()?;
        return (mantissa <= DOTNET_DECIMAL_MAX_MANTISSA).then_some(DotNetDecimal {
            mantissa,
            scale: 0,
            negative,
        });
    }

    let scale = usize::try_from(scale).ok()?;
    if scale > digits.len().saturating_add(28) {
        return Some(DotNetDecimal::ZERO);
    }
    let minimum_removed = scale
        .saturating_sub(28)
        .max(digits.len().saturating_sub(29));
    if minimum_removed > scale {
        return None;
    }
    for removed in minimum_removed..=scale {
        let mantissa = rounded_decimal_digits(digits, removed)?;
        if mantissa <= DOTNET_DECIMAL_MAX_MANTISSA {
            return if mantissa == 0 {
                Some(DotNetDecimal::ZERO)
            } else {
                Some(DotNetDecimal {
                    mantissa,
                    scale: u32::try_from(scale - removed).ok()?,
                    negative,
                })
            };
        }
    }
    None
}

fn rounded_decimal_digits(digits: &str, removed: usize) -> Option<u128> {
    if removed == 0 {
        return digits.parse::<u128>().ok();
    }
    if removed > digits.len() {
        return Some(0);
    }

    let retained_length = digits.len() - removed;
    let retained = &digits[..retained_length];
    let discarded = digits.as_bytes().get(retained_length..)?;
    let mut value = if retained.is_empty() {
        0
    } else {
        retained.parse::<u128>().ok()?
    };
    let round_digit = discarded[0] - b'0';
    let sticky = discarded[1..].iter().any(|digit| *digit != b'0');
    if round_digit > 5 || (round_digit == 5 && (sticky || value % 2 != 0)) {
        value = value.checked_add(1)?;
    }
    Some(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct U256([u64; 4]);

#[allow(clippy::cast_possible_truncation)]
impl U256 {
    const fn from_u128(value: u128) -> Self {
        Self([value as u64, (value >> 64) as u64, 0, 0])
    }

    fn multiply_u128(left: u128, right: u128) -> Self {
        let left = [left as u64, (left >> 64) as u64];
        let right = [right as u64, (right >> 64) as u64];
        let mut product = [0_u64; 4];

        for (left_index, left_limb) in left.into_iter().enumerate() {
            let mut carry = 0_u128;
            for (right_index, right_limb) in right.into_iter().enumerate() {
                let index = left_index + right_index;
                let value = u128::from(product[index])
                    + u128::from(left_limb) * u128::from(right_limb)
                    + carry;
                product[index] = value as u64;
                carry = value >> 64;
            }

            let mut index = left_index + 2;
            while carry != 0 {
                let value = u128::from(product[index]) + carry;
                product[index] = value as u64;
                carry = value >> 64;
                index += 1;
            }
        }
        Self(product)
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        let mut result = [0_u64; 4];
        let mut carry = false;
        for (index, output) in result.iter_mut().enumerate() {
            let (partial, first_carry) = self.0[index].overflowing_add(other.0[index]);
            let (sum, second_carry) = partial.overflowing_add(u64::from(carry));
            *output = sum;
            carry = first_carry || second_carry;
        }
        (!carry).then_some(Self(result))
    }

    fn checked_sub(self, other: Self) -> Option<Self> {
        if self.magnitude_cmp(other) == std::cmp::Ordering::Less {
            return None;
        }

        let mut result = [0_u64; 4];
        let mut borrow = false;
        for (index, output) in result.iter_mut().enumerate() {
            let (partial, first_borrow) = self.0[index].overflowing_sub(other.0[index]);
            let (difference, second_borrow) = partial.overflowing_sub(u64::from(borrow));
            *output = difference;
            borrow = first_borrow || second_borrow;
        }
        debug_assert!(!borrow);
        Some(Self(result))
    }

    fn checked_mul_u64(self, multiplier: u64) -> Option<Self> {
        let mut result = [0_u64; 4];
        let mut carry = 0_u128;
        for (limb, output) in self.0.into_iter().zip(result.iter_mut()) {
            let value = u128::from(limb) * u128::from(multiplier) + carry;
            *output = value as u64;
            carry = value >> 64;
        }
        (carry == 0).then_some(Self(result))
    }

    fn checked_mul_power_of_ten(mut self, power: u32) -> Option<Self> {
        for _ in 0..power {
            self = self.checked_mul_u64(10)?;
        }
        Some(self)
    }

    fn checked_shift_left(mut self, shift: u32) -> Option<Self> {
        for _ in 0..shift {
            self = self.checked_mul_u64(2)?;
        }
        Some(self)
    }

    fn div_rem_u64(self, divisor: u64) -> (Self, u64) {
        let mut quotient = [0_u64; 4];
        let mut remainder = 0_u128;
        for index in (0..4).rev() {
            let value = (remainder << 64) | u128::from(self.0[index]);
            quotient[index] = (value / u128::from(divisor)) as u64;
            remainder = value % u128::from(divisor);
        }
        (Self(quotient), remainder as u64)
    }

    fn round_div_power_of_ten(self, power: u32) -> Option<Self> {
        if power == 0 {
            return Some(self);
        }

        let mut quotient = self;
        let mut sticky = false;
        let mut round_digit = 0_u64;
        for digit in 0..power {
            let (next, remainder) = quotient.div_rem_u64(10);
            quotient = next;
            if digit + 1 == power {
                round_digit = remainder;
            } else {
                sticky |= remainder != 0;
            }
        }

        if round_digit > 5 || (round_digit == 5 && (sticky || quotient.0[0] & 1 != 0)) {
            quotient = quotient.checked_add(Self::from_u128(1))?;
        }
        Some(quotient)
    }

    fn div_rem_u32_quotient(self, divisor: Self) -> (u32, Self) {
        let mut lower = 0_u32;
        let mut upper = 1_u32 << 25;
        while lower + 1 < upper {
            let middle = lower + (upper - lower) / 2;
            let product = divisor
                .checked_mul_u64(u64::from(middle))
                .expect("bounded decimal-to-f32 quotient fits U256");
            if product.magnitude_cmp(self) == std::cmp::Ordering::Greater {
                upper = middle;
            } else {
                lower = middle;
            }
        }
        let product = divisor
            .checked_mul_u64(u64::from(lower))
            .expect("bounded decimal-to-f32 quotient fits U256");
        (
            lower,
            self.checked_sub(product)
                .expect("the quotient product does not exceed the dividend"),
        )
    }

    fn magnitude_cmp(self, other: Self) -> std::cmp::Ordering {
        for index in (0..4).rev() {
            match self.0[index].cmp(&other.0[index]) {
                std::cmp::Ordering::Equal => {}
                ordering => return ordering,
            }
        }
        std::cmp::Ordering::Equal
    }

    fn bit_length(self) -> u32 {
        for index in (0..4).rev() {
            if self.0[index] != 0 {
                return index as u32 * 64 + (64 - self.0[index].leading_zeros());
            }
        }
        0
    }

    fn fits_decimal_mantissa(self) -> bool {
        self.0[2] == 0 && self.0[3] == 0 && u32::try_from(self.0[1]).is_ok()
    }

    fn to_u128(self) -> u128 {
        u128::from(self.0[0]) | (u128::from(self.0[1]) << 64)
    }
}

#[cfg(test)]
mod tests {
    use super::{DOTNET_DECIMAL_MAX_MANTISSA, DotNetDecimal};

    #[test]
    fn validates_parts_and_preserves_float_conversion_goldens() {
        assert!(DotNetDecimal::from_parts(DOTNET_DECIMAL_MAX_MANTISSA, 28, false).is_some());
        assert!(DotNetDecimal::from_parts(DOTNET_DECIMAL_MAX_MANTISSA + 1, 0, false).is_none());
        assert!(DotNetDecimal::from_parts(1, 29, false).is_none());
        assert!(DotNetDecimal::from_f32(f32::MAX).is_none());
        assert!(DotNetDecimal::from_f32(f32::MIN).is_none());
        assert!(DotNetDecimal::from_f32(f32::NAN).is_none());

        let rounded = DotNetDecimal::from_f32(1.650_000_1).unwrap();
        assert_eq!(rounded, DotNetDecimal::from_parts(165, 2, false).unwrap());
    }

    #[test]
    fn invariant_parser_matches_number_styles_any_goldens() {
        for (value, expected) in [
            ("  ¤ -1,234.5e-1  ", -123.45_f32),
            ("1,,2", 12.0),
            ("1,.2", 1.2),
            ("0.00000000000000000000000000006", 1e-28),
        ] {
            assert_eq!(
                DotNetDecimal::parse_invariant(value)
                    .unwrap()
                    .to_f32()
                    .to_bits(),
                expected.to_bits()
            );
        }

        assert_eq!(
            DotNetDecimal::parse_invariant("15e-29").unwrap(),
            DotNetDecimal::from_parts(2, 28, false).unwrap()
        );
        assert_eq!(
            DotNetDecimal::parse_invariant("0.12345678901234567890123456789").unwrap(),
            DotNetDecimal::from_parts(1_234_567_890_123_456_789_012_345_679, 28, false).unwrap()
        );
        assert_eq!(
            DotNetDecimal::parse_invariant("79228162514264337593543950335").unwrap(),
            DotNetDecimal::from_parts(DOTNET_DECIMAL_MAX_MANTISSA, 0, false).unwrap()
        );

        for invalid in [
            "NaN",
            "Infinity",
            "1e100",
            ",1",
            "1.2,3",
            "¤1¤",
            "79228162514264337593543950336",
        ] {
            assert!(
                DotNetDecimal::parse_invariant(invalid).is_none(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn checked_arithmetic_and_integer_casts_match_csharp_domains() {
        let one_and_half = DotNetDecimal::parse_invariant("1.5").unwrap();
        let two = DotNetDecimal::from_parts(2, 0, false).unwrap();
        let negative_half = DotNetDecimal::from_parts(5, 1, true).unwrap();
        let result = one_and_half
            .checked_mul(two)
            .and_then(|value| value.checked_add(negative_half))
            .unwrap();
        assert_eq!(result.to_f32().to_bits(), 2.5_f32.to_bits());

        assert_eq!(
            DotNetDecimal::parse_invariant("-2147483648.9")
                .unwrap()
                .to_i32(),
            Some(i32::MIN)
        );
        assert_eq!(
            DotNetDecimal::parse_invariant("2147483648")
                .unwrap()
                .to_i32(),
            None
        );
        assert_eq!(
            DotNetDecimal::parse_invariant("255.99").unwrap().to_u8(),
            Some(255)
        );
        assert_eq!(DotNetDecimal::parse_invariant("256").unwrap().to_u8(), None);
    }
}
