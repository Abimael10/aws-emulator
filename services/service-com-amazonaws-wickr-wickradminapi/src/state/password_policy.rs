// SPDX-License-Identifier: CC0-1.0

//! The password policy a security group imposes, and the one refusal a password
//! that does not meet it is answered with.
//!
//! `SecurityGroupSettings$passwordRequirements` is "the password complexity
//! requirements that users must follow when creating or changing passwords", and
//! `PasswordRequirements` carries a minimum length plus a required count of
//! uppercase letters, lowercase letters, numbers and symbols. The policy is
//! therefore a property of the GROUP, so every password an account of that group
//! is created with is decided against that group's own five numbers.
//!
//! The model expresses none of this rule — a password is a `SensitiveString`
//! whose `^[\S\s]*$` matches every string, so no constraint trait could have
//! refused one before a handler ran — and neither are the numbers derived from
//! the password: they are read from the group's stored policy and reported back
//! verbatim, which is why this module renders them rather than any one operation.

/// The `ErrorDetail$field` a refusal of a password carries.
///
/// What the value broke is the policy, not one member of the request, so the
/// detail names the policy; the five numbers it was measured against are stated
/// in the reason beside it.
pub const PASSWORD_POLICY_FIELD: &str = "passwordPolicy";

/// The minimum password length of the policy a security group carries until
/// something sets another.
///
/// AWS Wickr Administration Guide, "Network security groups": the password
/// policy of the security group a network is created with requires a minimum of
/// 8 characters.
pub const DEFAULT_PASSWORD_MIN_LENGTH: i32 = 8;

/// How many characters of each class that policy requires.
///
/// The same source: the default policy sets a minimum length and requires no
/// particular mix of character classes, so a password long enough meets it
/// whatever it is made of.
pub const DEFAULT_PASSWORD_CLASS_MINIMUM: i32 = 0;

/// The fixed first sentence of the summary a refused password is answered with.
const SUMMARY_HEADER: &str = "password does not meet minimum requirements: ";

/// The newline and indentation each requirement line of that summary begins
/// with, and which closes it.
const SUMMARY_LINE_PREFIX: &str = "\n      ";

/// The policy a security group carries until something sets another.
///
/// Also the policy a group whose stored settings state none imposes: a password
/// rule is not something a group can be without — `SecurityGroupSettingsRequest`
/// carries no member that could have named one, so a group created through this
/// API states no policy of its own and the accounts under it are held to this one.
pub fn default_password_requirements() -> crate::ops::types::PasswordRequirements {
    crate::ops::types::PasswordRequirements {
        min_length: Some(DEFAULT_PASSWORD_MIN_LENGTH),
        uppercase: Some(DEFAULT_PASSWORD_CLASS_MINIMUM),
        lowercase: Some(DEFAULT_PASSWORD_CLASS_MINIMUM),
        numbers: Some(DEFAULT_PASSWORD_CLASS_MINIMUM),
        symbols: Some(DEFAULT_PASSWORD_CLASS_MINIMUM),
    }
}

/// How many characters of `password` belong to each class a policy counts.
///
/// Every member of `PasswordRequirements` counts CHARACTERS, so the length is
/// counted in characters too, not bytes. Case and digit-hood are a character's
/// own, whatever its script; a symbol is a character that is neither a letter nor
/// a digit, and whitespace is not one of those special characters.
fn class_counts(password: &str) -> PasswordClassCounts {
    let mut counts = PasswordClassCounts::default();
    for character in password.chars() {
        counts.length += 1;
        if character.is_uppercase() {
            counts.uppercase += 1;
        }
        if character.is_lowercase() {
            counts.lowercase += 1;
        }
        if character.is_numeric() {
            counts.numbers += 1;
        }
        if !character.is_alphanumeric() && !character.is_whitespace() {
            counts.symbols += 1;
        }
    }
    counts
}

/// What one password is made of, as the five numbers a policy is stated in.
#[derive(Debug, Default)]
struct PasswordClassCounts {
    length: usize,
    uppercase: usize,
    lowercase: usize,
    numbers: usize,
    symbols: usize,
}

/// Whether `password` meets `requirements`.
///
/// A requirement a policy leaves unset is no requirement at all — the minimum
/// length as much as the four class counts — so a group whose policy states only
/// a minimum length turns down only passwords shorter than it, and one that
/// states nothing turns down nothing.
pub fn password_meets_requirements(
    password: &str,
    requirements: &crate::ops::types::PasswordRequirements,
) -> bool {
    let counts = class_counts(password);
    meets(counts.length, requirements.min_length)
        && meets(counts.uppercase, requirements.uppercase)
        && meets(counts.lowercase, requirements.lowercase)
        && meets(counts.numbers, requirements.numbers)
        && meets(counts.symbols, requirements.symbols)
}

/// Whether `actual` characters satisfy a required count: an absent requirement
/// requires none, and a count at the requirement satisfies it.
fn meets(actual: usize, required: Option<i32>) -> bool {
    i64::try_from(actual).unwrap_or(i64::MAX) >= i64::from(required.unwrap_or(0))
}

/// The summary a password that does not meet `requirements` is refused with.
///
/// It states the POLICY, in the order `PasswordRequirements` reads in the
/// console — the four character classes and then the minimum length — and
/// nothing about the password that was submitted: the five numbers are the
/// group's, so two refusals against the same group are worded identically
/// whichever part of the policy the password broke.
pub fn password_policy_reason(requirements: &crate::ops::types::PasswordRequirements) -> String {
    let uppercase = requirements.uppercase.unwrap_or(0);
    let lowercase = requirements.lowercase.unwrap_or(0);
    let numbers = requirements.numbers.unwrap_or(0);
    let symbols = requirements.symbols.unwrap_or(0);
    let min_length = requirements.min_length.unwrap_or(0);
    let mut reason = String::from(SUMMARY_HEADER);
    for line in [
        format!("{uppercase} uppercase letter(s), "),
        format!("{lowercase} lowercase letter(s), "),
        format!("{numbers} number(s), "),
        format!("{symbols} symbol(s), "),
        format!("minimum {min_length} letters"),
    ] {
        reason.push_str(SUMMARY_LINE_PREFIX);
        reason.push_str(&line);
    }
    reason.push_str(SUMMARY_LINE_PREFIX);
    reason
}
