// SPDX-License-Identifier: CC0-1.0

//! Which values this service ADMITS as a phone number, for the members whose shape the
//! model leaves unconstrained.
//!
//! The model constrains one phone-number shape and leaves the others plain: `PhoneNumberString`
//! is `length 0..20, pattern ^(\+[0-9]{8,}|[0-9]{0,9})$`
//! (`smithy_query(query="shape",shape="PhoneNumber,PhoneNumberString")`, which reports
//! `PhoneNumber` as a plain sensitive string). That published pattern is the service's own
//! statement of what a phone number may look like, so it is the admission rule for every
//! phone-number member — two forms, an international one written with a `+` and at least
//! [`MIN_INTERNATIONAL_DIGITS`] digits, and a national or short-code one written as digits
//! alone, up to [`MAX_NATIONAL_DIGITS`] of them.
//!
//! It is deliberately WIDER than the destination rule
//! ([`super::destination_number::destination_number_fault`]): a number an SMS message could be
//! CARRIED to has to address a country E.164 assigns and to be written at an E.164 length, and
//! the members that name such a number say so ("the destination phone number to verify … that
//! you can send SMS messages to", the SMS sandbox trio's `PhoneNumber`). The opt-out pair's
//! `phoneNumber` names no destination — "the phone number for which you want to check the opt
//! out status" — so the operation that ACTS on its record admits every value the published
//! pattern admits. The narrower rule stays where a message is sent; this one decides whether a
//! value is a number the record may be written for.
//!
//! Wider again is [`is_written_as_phone_number`]: whether a value is a phone number AT ALL,
//! which is all an operation that only REPORTS on this account's record asks of its member.
//! The three judgements NEST — written ⊇ published ⊇ destination — so the operations of one
//! number never disagree about whether it is one.

use super::*;

/// The fewest digits the published pattern takes after the `+` of an international number:
/// the `\+[0-9]{8,}` branch of `PhoneNumberString`'s pattern.
pub const MIN_INTERNATIONAL_DIGITS: usize = 8;

/// The most digits the published pattern takes in a number written WITHOUT a `+`: the
/// `[0-9]{0,9}` branch of `PhoneNumberString`'s pattern, which is what admits a national
/// number and a short code.
pub const MAX_NATIONAL_DIGITS: usize = 9;

/// The most characters a phone number carries in all: `PhoneNumberString`'s own
/// `length 0..20`.
pub const MAX_PHONE_NUMBER_CHARS: usize = 20;

/// Whether `value` is a phone number in a form this service admits — the published
/// `PhoneNumberString` pattern, bounded by its own length.
///
/// A value carrying no digit at all is NOT one: the members this rule governs are `@required`,
/// so a value that names no number is the absent input the pattern's empty branch stands for
/// rather than a number the record could be read for.
pub fn is_phone_number_form(value: &str) -> bool {
    if value.is_empty() || value.chars().count() > MAX_PHONE_NUMBER_CHARS {
        return false;
    }
    match value.strip_prefix('+') {
        Some(digits) => {
            digits.len() >= MIN_INTERNATIONAL_DIGITS
                && digits.bytes().all(|byte| byte.is_ascii_digit())
        }
        None => {
            value.len() <= MAX_NATIONAL_DIGITS && value.bytes().all(|byte| byte.is_ascii_digit())
        }
    }
}

/// The message a phone-number member holding `value` is refused with, or `None` when `value`
/// is a form this service admits ([`is_phone_number_form`]).
///
/// The sentence is the one SNS states for a phone-number parameter it cannot read
/// ([`invalid_phone_number_message`]), whichever branch of the pattern the value missed.
pub fn phone_number_form_refusal(value: &str) -> Option<String> {
    (!is_phone_number_form(value)).then(invalid_phone_number_message)
}

/// The characters a phone number is conventionally WRITTEN with beside its digits: the
/// grouping separators a caller's number arrives carrying when it was typed or copied
/// rather than normalised.
pub const PHONE_NUMBER_SEPARATORS: &str = " -.()";

/// How many digits `value` carries when it is WRITTEN as a phone number — an optional
/// leading `+`, then digits and [`PHONE_NUMBER_SEPARATORS`] and nothing else — or `None`
/// when it carries a character no phone number is written with.
///
/// The WIDEST of this service's phone-number judgements: it decides whether a value is a
/// phone number AT ALL, and leaves how many digits one needs to its callers, because that
/// is what differs between a number a message is carried to and a number a record is
/// merely kept under.
pub fn written_phone_number_digits(value: &str) -> Option<usize> {
    let body = value.strip_prefix('+').unwrap_or(value);
    body.chars()
        .all(|character| character.is_ascii_digit() || PHONE_NUMBER_SEPARATORS.contains(character))
        .then(|| body.chars().filter(char::is_ascii_digit).count())
}

/// Whether `value` is written as a phone number at all: [`written_phone_number_digits`]
/// reads at least one digit off it, and it is no longer than a phone number gets
/// ([`MAX_PHONE_NUMBER_CHARS`]).
///
/// What an operation that only REPORTS on this account's record of a number is held to: it
/// states a property of the record rather than acting on the number, so it refuses only a
/// value that could not be a phone number anywhere — a word, a URL, an extension suffix, a
/// value carrying no digit — and reports on every value that could be one, including the
/// national and separator-written forms the published pattern's own branches do not take.
///
/// Wider than [`is_phone_number_form`], which is wider again than
/// [`super::destination_number::destination_number_fault`]: a value either of the narrower
/// rules admits is written as a phone number too, so the three judgements nest and no
/// operation refuses a value that the one beside it acts on.
pub fn is_written_as_phone_number(value: &str) -> bool {
    value.chars().count() <= MAX_PHONE_NUMBER_CHARS
        && written_phone_number_digits(value).is_some_and(|digits| digits > 0)
}

/// The message a phone-number member holding `value` is refused with when the value is not
/// written as a phone number at all ([`is_written_as_phone_number`]), or `None` when it is.
///
/// The sentence is the one SNS states for a phone-number parameter it cannot read
/// ([`invalid_phone_number_message`]) — the same wording whichever judgement refused the
/// value, since all of them fault the FORM of the member.
pub fn written_phone_number_refusal(value: &str) -> Option<String> {
    (!is_written_as_phone_number(value)).then(invalid_phone_number_message)
}
