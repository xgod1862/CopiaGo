// First code point of each ten-digit Unicode Decimal range in the UCD used by Python 3.12.
// Data derived from Unicode 15.0.0 DerivedNumericType.txt, © 2022 Unicode, Inc.
// https://raw.githubusercontent.com/unicode-org/unicodetools/main/unicodetools/data/ucd/15.0.0/extracted/DerivedNumericType.txt
// See Unicode-LICENSE.txt in this directory.
const DECIMAL_ZEROES: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650,
    0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60, 0x16ac0,
    0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e950,
    0x1fbf0,
];

// Matches Python's default integer string conversion limit.
const MAX_COMPONENT_DIGITS: usize = 4300;

/// Returns normalized components ordered by digit count then digits, so comparison cannot overflow.
pub(super) fn parse(text: &str) -> Option<[(usize, String); 3]> {
    let text = text.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\x1c'..='\x1f'));
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut next_part = || decimal_part(parts.next()?);
    let version = [next_part()?, next_part()?, next_part()?];
    parts.next().is_none().then_some(version)
}

fn decimal_part(part: &str) -> Option<(usize, String)> {
    if part.is_empty() {
        return None;
    }
    let mut normalized = String::new();
    for (index, digit) in part.chars().enumerate() {
        if index >= MAX_COMPONENT_DIGITS {
            return None;
        }
        let digit = ascii_digit(digit)?;
        if digit != '0' || !normalized.is_empty() {
            normalized.push(digit);
        }
    }
    if normalized.is_empty() {
        normalized.push('0');
    }
    Some((normalized.len(), normalized))
}

fn ascii_digit(digit: char) -> Option<char> {
    let point = u32::from(digit);
    let range = DECIMAL_ZEROES
        .partition_point(|&zero| zero <= point)
        .checked_sub(1)?;
    let value = point - DECIMAL_ZEROES[range];
    char::from_digit(value, 10)
}
