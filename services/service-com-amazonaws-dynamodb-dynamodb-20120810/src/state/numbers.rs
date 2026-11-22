// SPDX-License-Identifier: CC0-1.0

//! The order-preserving encoding of an `N` attribute value: how the textual form one
//! request carries becomes the key the store compares, and the rules a text has to
//! satisfy to be a number this service stores at all.
//!
//! A file of its own beside the item store rather than inside it: this is about ONE
//! value's numeric domain, not about what the store holds. The type itself stays in
//! `state.rs` beside the other key values; only what it DOES lives here.

use super::*;

impl NumberKey {
    /// Parse the textual form of an `N` attribute value, or `None` when it is not a
    /// value DynamoDB stores. Use [`Self::try_parse`] where the reason matters.
    pub fn parse(text: &str) -> Option<Self> {
        Self::try_parse(text).ok()
    }

    /// Parse the textual form of an `N` attribute value, naming the rule a refused
    /// text breaks.
    pub fn try_parse(text: &str) -> Result<Self, NumberFault> {
        let (negative, unsigned) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        let (mantissa, exponent_text) = match unsigned.find(['e', 'E']) {
            Some(at) => (&unsigned[..at], Some(&unsigned[at + 1..])),
            None => (unsigned, None),
        };
        let (integral, fractional) = match mantissa.find('.') {
            Some(at) => (&mantissa[..at], &mantissa[at + 1..]),
            None => (mantissa, ""),
        };
        if integral.is_empty() && fractional.is_empty() {
            return Err(NumberFault::NotNumeric);
        }
        if !integral.bytes().chain(fractional.bytes()).all(|b| b.is_ascii_digit()) {
            return Err(NumberFault::NotNumeric);
        }
        // WHETHER a text denotes a number at all is told apart from WHAT magnitude it
        // states: the exponent is read as the signed 64-bit integer a decimal exponent is
        // carried in ([`signed_exponent`]), and a magnitude that integer CAN state is then
        // held to the documented range as the over- or underflow its sign names — however
        // many digits the exponent is written with. Only a text whose exponent that integer
        // cannot state at all, or which is no signed run of digits, denotes no decimal value
        // and is refused as a text this service cannot convert to a number.
        let exponent = match exponent_text {
            Some(text) => signed_exponent(text)?,
            None => 0,
        };
        // The TEXT is bounded before anything is normalised: the digits a longer one
        // carries cannot change which documented value it denotes, and the parsed
        // value's accounted size is computed from its significant digits, not its
        // text. Both terms come from the request.
        if integral.len().saturating_add(fractional.len()) > MAX_NUMBER_TEXT_DIGITS {
            return Err(NumberFault::TextTooLong);
        }
        let mut digits: Vec<u8> = integral
            .bytes()
            .chain(fractional.bytes())
            .map(|b| b - b'0')
            .collect();
        // The value is 0.<digits> * 10^point before normalisation: the decimal point
        // sits after the integral digits, shifted by the exponent. Both terms come
        // from the request, so every step is checked: an exponent that leaves the
        // documented range is refused as an over- or underflow, whichever direction
        // it left it in. The shift is CHECKED, so a magnitude stated with an exponent at
        // the limit of the integer it is carried in is still reported as the over- or
        // underflow it is rather than wrapping into the range.
        let shifted = i64::try_from(integral.len())
            .ok()
            .and_then(|len| len.checked_add(exponent));
        let mut point: i64 = match shifted {
            Some(point) => point,
            None if exponent > 0 => return Err(NumberFault::Overflow),
            None => return Err(NumberFault::Underflow),
        };
        let leading_zeros = digits.iter().take_while(|digit| **digit == 0).count();
        digits.drain(..leading_zeros);
        point = match i64::try_from(leading_zeros)
            .ok()
            .and_then(|zeros| point.checked_sub(zeros))
        {
            Some(point) => point,
            None => return Err(NumberFault::Underflow),
        };
        while digits.last() == Some(&0) {
            digits.pop();
        }
        if digits.is_empty() {
            // Every spelling of zero normalises to the same value, sign included — but
            // the EXPONENT the text spelled is still held to the documented range: a
            // zero written with an exponent outside that range is a number this service
            // does not store, whatever its mantissa normalises to.
            if exponent > i64::from(MAX_EXPONENT) {
                return Err(NumberFault::Overflow);
            }
            if exponent < i64::from(MIN_EXPONENT) {
                return Err(NumberFault::Underflow);
            }
            return Ok(Self {
                negative: false,
                point: 0,
                digits,
            });
        }
        if digits.len() > MAX_SIGNIFICANT_DIGITS {
            return Err(NumberFault::Precision);
        }
        if point > i64::from(MAX_POINT) {
            return Err(NumberFault::Overflow);
        }
        if point < i64::from(MIN_POINT) {
            return Err(NumberFault::Underflow);
        }
        Ok(Self {
            negative,
            // Inside the documented range, which the two bounds above just held it to,
            // so the narrowing cannot lose a value.
            point: i32::try_from(point).map_err(|_| NumberFault::Overflow)?,
            digits,
        })
    }

    /// Compare magnitudes (both treated as non-negative).
    pub(super) fn magnitude_cmp(&self, other: &Self) -> Ordering {
        match (self.digits.is_empty(), other.digits.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            // A larger decimal exponent means a larger magnitude once both are
            // normalised to 0.<digits>; equal exponents fall back to the digits,
            // whose lexicographic order IS their numeric order.
            (false, false) => self
                .point
                .cmp(&other.point)
                .then_with(|| self.digits.cmp(&other.digits)),
        }
    }

    /// The number of significant digits, which is what an `N` value's stored size is
    /// computed from.
    ///
    /// The digits are the ones the number is SPELLED with, leading zeros left out:
    /// "Leading and trailing zeroes are trimmed" (the `AttributeValue$N` numeric domain),
    /// and a decimal point is where a trailing zero can be trimmed at all — so the zeros a
    /// whole number carries BEFORE the point are digits of it (`100` spells three), while
    /// the zeros a fraction ends with are not (`1.100` spells two), and a zero between two
    /// significant digits is always one of them (`1.0001` spells five).
    ///
    /// A value whose decimal point sits just past the digits it holds is spelled with the
    /// zeros that point asks for, and counts them. A magnitude past the documented
    /// precision ceiling is not: a number that would need more digits than a stored number
    /// can hold is spelled with an EXPONENT instead of with that run of zeros, so what it
    /// is spelled with is its mantissa — which is also what keeps every `N` value inside
    /// the documented maximum width of one.
    pub(super) fn significant_digits(&self) -> i64 {
        let held = i64::try_from(self.digits.len()).unwrap_or(i64::MAX);
        let ceiling = i64::try_from(MAX_SIGNIFICANT_DIGITS).unwrap_or(i64::MAX);
        let spelled = i64::from(self.point).max(0);
        if spelled > ceiling {
            return held;
        }
        held.max(spelled)
    }

    /// The bytes storing this number is accounted for: "1 byte per two significant
    /// digits" — so an odd digit count fills the second half of a byte and pays for it —
    /// "+ 1 byte" (the Developer Guide's "Item sizes and formats"), and one more byte for
    /// a negative number, which is what makes an `N` value at the documented precision
    /// ceiling the documented 21 bytes wide at most.
    ///
    /// Zero holds no significant digits, so it is the base byte alone.
    pub(super) fn stored_bytes(&self) -> i64 {
        let digits = self.significant_digits();
        let sign = i64::from(self.negative);
        digits
            .saturating_add(1)
            .saturating_div(2)
            .saturating_add(1)
            .saturating_add(sign)
    }

    /// This number's CANONICAL text — the one form every read of it reports.
    ///
    /// An `N` value travels as a string ("Numbers are sent across the network to
    /// DynamoDB as strings, to maximize compatibility across languages and libraries.
    /// However, DynamoDB treats them as number type attributes" — `AttributeValue$N`),
    /// so what is stored is the NUMBER and what a read reports is that number rendered
    /// once, not the lexeme a writer happened to send. It is also the form a value
    /// COMPUTED by the service carries (an `ADD`ed or `SET`-arithmetic number), so one
    /// value has one spelling however the operands were written. The rendering is plain
    /// decimal over the normalised digits — never an exponent, no sign on zero, and a
    /// leading or trailing zero only where the decimal point needs one — so every
    /// spelling of one value reads back identically.
    pub fn canonical_text(&self) -> String {
        if self.digits.is_empty() {
            return "0".to_string();
        }
        let digits: String = self
            .digits
            .iter()
            .map(|digit| char::from(b'0' + digit))
            .collect();
        // The value is 0.<digits> * 10^point, so `point` is where the decimal point
        // falls among the digits: left of them all (a leading "0." and the zeros the
        // exponent asks for), right of them all (trailing zeros), or between two.
        let magnitude = match usize::try_from(self.point) {
            Err(_) | Ok(0) => {
                let zeros = usize::try_from(self.point.unsigned_abs()).unwrap_or(0);
                format!("0.{}{digits}", "0".repeat(zeros))
            }
            Ok(at) if at >= self.digits.len() => {
                format!("{digits}{}", "0".repeat(at - self.digits.len()))
            }
            Ok(at) => format!("{}.{}", &digits[..at], &digits[at..]),
        };
        if self.negative {
            format!("-{magnitude}")
        } else {
            magnitude
        }
    }
}

/// The decimal exponent `text` states, or the rule it breaks.
///
/// `NotNumeric` whenever the text states no exponent a decimal value can carry: nothing
/// after the exponent marker, a sign with no digits behind it, a character that is not a
/// digit, or a run of digits stating a magnitude outside the signed 64-bit integer a decimal
/// exponent is carried in. Each of those leaves a text that denotes no decimal number, which
/// is a different answer from a number whose magnitude that integer CAN state and the
/// documented range excludes: that one is the over- or underflow its sign names, whatever
/// WIDTH the exponent is written at — ten digits or twenty, padded or not.
///
/// Leading zeros denote nothing, so the width of the run decides neither question on its
/// own — only the value its digits spell, read as the SIGNED integer the text states, so the
/// bottom of that integer's range is representable too.
fn signed_exponent(text: &str) -> Result<i64, NumberFault> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(NumberFault::NotNumeric);
    }
    // Dropped before the value is read, so an exponent spelled with a run of them states
    // the same magnitude a shorter spelling does.
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Ok(0);
    }
    // Read as the SIGNED integer the text spells, so the bottom of that integer's range is
    // representable too. A magnitude that integer cannot state is a text this service cannot
    // convert to a number at all.
    let signed = if negative {
        format!("-{significant}")
    } else {
        significant.to_string()
    };
    signed.parse::<i64>().map_err(|_| NumberFault::NotNumeric)
}

/// The exponent bounds a value a STATEMENT carries is read against: the signed 32-bit
/// integer the statement surface reads a `Parameters` entry's exponent into, which is
/// NARROWER than the one the item API reads the same text into ([`signed_exponent`]).
///
/// The two surfaces coerce a number with two different readers, so one text can be a
/// magnitude to one and no number at all to the other: an exponent of eleven digits is a
/// magnitude the item API holds to the documented range, and a parameter the statement
/// surface cannot convert to a numeric value. Nothing below the exponent differs — the
/// documented precision and exponent range, the set rules and every other value rule are the
/// same for both ([`check_statement_value`]).
const MAX_STATEMENT_EXPONENT: i64 = i32::MAX as i64;
const MIN_STATEMENT_EXPONENT: i64 = i32::MIN as i64;

/// Hold every number inside `value` to the exponent width the STATEMENT surface reads one
/// into ([`MAX_STATEMENT_EXPONENT`]), reporting a text outside it as the parameter this
/// surface cannot convert to a numeric value, echoing the text as the request wrote it.
///
/// Reaches the `N` of a scalar, every member of an `NS`, and every number nested in an `L`
/// or an `M` at any depth, because the reader belongs to the value's TYPE and not to where
/// the value sits. The walk is ITERATIVE, over an explicit stack, for the reason
/// [`attribute_value_size`]'s is: how deeply a value nests is the caller's choice, so
/// recursing on it would make stack use a function of the request.
pub fn check_statement_number_width(value: &AttributeValue) -> Result<(), RequestError> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            AttributeValue::N(text) => check_statement_exponent(text)?,
            AttributeValue::Ns(members) => {
                for member in members {
                    check_statement_exponent(member)?;
                }
            }
            AttributeValue::L(members) => pending.extend(members.iter()),
            AttributeValue::M(entries) => pending.extend(entries.values()),
            _ => {}
        }
    }
    Ok(())
}

/// One number text held to the statement surface's exponent width.
fn check_statement_exponent(text: &str) -> Result<(), RequestError> {
    let exponent = match text.find(['e', 'E']) {
        Some(at) => &text[at + 1..],
        // A text that states no exponent at all states none outside this width; whether it
        // is a number is the shared reader's question.
        None => return Ok(()),
    };
    match signed_exponent(exponent) {
        Ok(exponent) if (MIN_STATEMENT_EXPONENT..=MAX_STATEMENT_EXPONENT).contains(&exponent) => {
            Ok(())
        }
        // An exponent this surface cannot read, either because it is no signed run of digits
        // or because it states a magnitude outside the integer this surface reads one into.
        _ => Err(number_rejection(text, NumberFault::NotNumeric)),
    }
}
