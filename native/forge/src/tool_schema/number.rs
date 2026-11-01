//! Exact decimal comparison without floats, heap storage or exponent expansion.
use std::cmp::Ordering;

pub(super) struct Decimal<'a> {
    mantissa: &'a str,
    negative: bool,
    scale: i64,
    digits: usize,
    leading: usize,
    trailing: usize,
}

impl<'a> Decimal<'a> {
    /// Input must already have passed JSON number syntax validation.
    pub(super) fn new(text: &'a str) -> Self {
        let negative = text.starts_with('-');
        let text = text.strip_prefix('-').unwrap_or(text);
        let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
        // Exponents are bounded by the parser; this conversion cannot overflow.
        let exponent: i64 = exponent.parse().expect("validated decimal exponent");
        let fraction = mantissa.split_once('.').map_or(0, |(_, s)| s.len());
        let digits = mantissa.bytes().filter(u8::is_ascii_digit).count();
        let leading = mantissa
            .bytes()
            .filter(u8::is_ascii_digit)
            .take_while(|b| *b == b'0')
            .count();
        let trailing = mantissa
            .bytes()
            .rev()
            .filter(u8::is_ascii_digit)
            .take_while(|b| *b == b'0')
            .count();
        Self {
            mantissa,
            negative: negative && leading != digits,
            scale: exponent - fraction as i64,
            digits: digits - leading,
            leading,
            trailing,
        }
    }

    pub(super) fn integer(&self) -> bool {
        self.digits == 0 || self.scale + self.trailing as i64 >= 0
    }

    pub(super) fn compare(&self, other: &Self) -> Ordering {
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let order = self.magnitude(other);
        if self.negative {
            order.reverse()
        } else {
            order
        }
    }

    fn magnitude(&self, other: &Self) -> Ordering {
        if self.digits == 0 || other.digits == 0 {
            return (self.digits != 0).cmp(&(other.digits != 0));
        }
        let order = (self.scale + self.digits as i64).cmp(&(other.scale + other.digits as i64));
        if order != Ordering::Equal {
            return order;
        }
        let digits = |value: &Self| {
            value
                .mantissa
                .bytes()
                .filter(u8::is_ascii_digit)
                .skip(value.leading)
                .chain(std::iter::repeat(b'0'))
        };
        digits(self)
            .zip(digits(other))
            .take(self.digits.max(other.digits))
            .find_map(|(a, b)| (a != b).then(|| a.cmp(&b)))
            .unwrap_or(Ordering::Equal)
    }
}
