// SPDX-License-Identifier: CC0-1.0

//! What a policy statement's `Condition` block may name: the condition operators the IAM
//! policy language DEFINES, and how a clause naming anything else is read.
//!
//! An IoT policy document is an IAM-grammar JSON policy
//! ([`super::validate_policy_document`]), and a statement's `Condition` is a map keyed by a
//! condition OPERATOR — the comparison the clause performs — whose own set the language
//! fixes. A clause keyed by a name outside that set states a comparison nothing can
//! perform, so the document describes no authorization decision at all and is refused
//! rather than stored and evaluated.
//!
//! The operator names come from the IAM User Guide's condition-operator reference (*IAM
//! JSON policy elements: Condition operators*): the string, numeric, date, boolean, binary,
//! IP-address, ARN and existence comparisons, each of which may be written under a set
//! qualifier ([`SET_QUALIFIERS`]) and with the suffix that makes it hold for a key the
//! request carries no value for ([`IF_EXISTS_SUFFIX`]) — except the existence operator,
//! which that suffix is not defined for because it already asks about presence.
//!
//! This is the set the LANGUAGE defines, which is a different question from which of them
//! an evaluator can decide: what this emulator's policy evaluation decides is
//! `service_common::iam_authorization`'s own reading, and a document may legitimately name
//! an operator that evaluation leaves undecided.

/// The condition operators the IAM policy language defines, in the families its reference
/// groups them in: string, numeric, date, boolean, binary, IP-address, ARN and existence
/// comparisons.
pub const CONDITION_OPERATORS: &[&str] = &[
    // String comparisons.
    "StringEquals",
    "StringNotEquals",
    "StringEqualsIgnoreCase",
    "StringNotEqualsIgnoreCase",
    "StringLike",
    "StringNotLike",
    // Numeric comparisons.
    "NumericEquals",
    "NumericNotEquals",
    "NumericLessThan",
    "NumericLessThanEquals",
    "NumericGreaterThan",
    "NumericGreaterThanEquals",
    // Date comparisons.
    "DateEquals",
    "DateNotEquals",
    "DateLessThan",
    "DateLessThanEquals",
    "DateGreaterThan",
    "DateGreaterThanEquals",
    // The boolean comparison.
    "Bool",
    // The binary comparison.
    "BinaryEquals",
    // Address comparisons.
    "IpAddress",
    "NotIpAddress",
    // ARN comparisons.
    "ArnEquals",
    "ArnNotEquals",
    "ArnLike",
    "ArnNotLike",
    // The existence comparison, which asks about a key's PRESENCE rather than its value.
    NULL_OPERATOR,
];

/// The existence operator, which the `IfExists` suffix is NOT defined for: it already asks
/// whether the key is present, so the two together name no comparison.
pub const NULL_OPERATOR: &str = "Null";

/// The qualifiers a clause over a MULTI-VALUED key may be written under, ahead of the
/// operator itself (`ForAllValues:StringEquals`).
pub const SET_QUALIFIERS: &[&str] = &["ForAllValues:", "ForAnyValue:"];

/// The suffix that makes a comparison hold for a key the request carries no value for
/// (`StringEqualsIfExists`), defined for every operator but [`NULL_OPERATOR`].
pub const IF_EXISTS_SUFFIX: &str = "IfExists";

/// Whether `clause` names a condition operator the policy language defines — the operator
/// itself, optionally under one [`SET_QUALIFIERS`] qualifier and optionally carrying
/// [`IF_EXISTS_SUFFIX`].
///
/// The comparison folds case, so a document spelling an operator in another casing names
/// the same comparison: what makes a clause unreadable is naming no operator at all, not
/// how it is capitalised.
#[must_use]
pub fn names_condition_operator(clause: &str) -> bool {
    let (operator, if_exists) = comparison(clause);
    // `NullIfExists` is the one combination the language leaves undefined.
    if if_exists && operator.eq_ignore_ascii_case(NULL_OPERATOR) {
        return false;
    }
    CONDITION_OPERATORS
        .iter()
        .any(|defined| defined.eq_ignore_ascii_case(operator))
}

/// The comparison `clause` performs, and whether it is written to hold for a key the
/// request carries no value for: the operator with one [`SET_QUALIFIERS`] qualifier and
/// the [`IF_EXISTS_SUFFIX`] stripped off it.
fn comparison(clause: &str) -> (&str, bool) {
    let named = SET_QUALIFIERS
        .iter()
        .find_map(|qualifier| strip_prefix_ignore_ascii_case(clause, qualifier))
        .unwrap_or(clause);
    match strip_suffix_ignore_ascii_case(named, IF_EXISTS_SUFFIX) {
        Some(operator) => (operator, true),
        None => (named, false),
    }
}

/// The operators whose values are ADDRESSES: "IP address condition operators let you
/// construct `Condition` elements that restrict access based on comparing a key to an IPv4
/// or IPv6 address or range of IP addresses" (the IAM condition-operator reference).
pub const IP_ADDRESS_OPERATORS: &[&str] = &["IpAddress", "NotIpAddress"];

/// Whether a clause of `statement`'s `Condition` block compares a key against something
/// that is no IP ADDRESS at all.
///
/// An address comparison is only a comparison if what it states is an address: "you can use
/// a /32 prefix size to specify a single IP address", so each value is either one address or
/// one network of them, and anything else — text that is no address, a number, a nested
/// object — states a comparison nothing can perform. Both halves of the operator pair are
/// read, since a negated comparison compares the same values.
#[must_use]
pub fn condition_compares_a_non_address(statement: &::serde_json::Value) -> bool {
    let Some(clauses) = statement
        .get("Condition")
        .and_then(::serde_json::Value::as_object)
    else {
        return false;
    };
    clauses.iter().any(|(clause, keys)| {
        let (operator, _) = comparison(clause);
        if !IP_ADDRESS_OPERATORS
            .iter()
            .any(|named| named.eq_ignore_ascii_case(operator))
        {
            return false;
        }
        keys.as_object()
            .into_iter()
            .flatten()
            .any(|(_, stated)| !states_addresses(stated))
    })
}

/// Whether a clause's value states addresses: one address or network, or a list of them.
fn states_addresses(stated: &::serde_json::Value) -> bool {
    match stated {
        ::serde_json::Value::String(one) => is_an_address(one),
        ::serde_json::Value::Array(many) => many
            .iter()
            .all(|value| value.as_str().is_some_and(is_an_address)),
        _ => false,
    }
}

/// Whether `text` names an IP address or a network of them, in either IP version.
///
/// Read by the maintained address parsers rather than by matching the text, so every
/// spelling each form admits is read the way the comparison itself would read it
/// (`service_common::iam_authorization` decides such a clause through the same two).
fn is_an_address(text: &str) -> bool {
    let text = text.trim();
    text.parse::<::std::net::IpAddr>().is_ok() || text.parse::<::ipnet::IpNet>().is_ok()
}

/// The first clause of `statement`'s `Condition` block naming no condition operator, or
/// `None` for a statement whose every clause names one — including a statement carrying no
/// `Condition` at all, which states no comparison to read.
///
/// A `Condition` element that is not a map keys no clause, so there is no operator in it to
/// refuse: whether such an element is a document the service stores at all is the grammar's
/// own question ([`super::validate_policy_document`]), not this reading's.
#[must_use]
pub fn unknown_condition_type(statement: &::serde_json::Value) -> Option<&str> {
    statement
        .get("Condition")?
        .as_object()?
        .keys()
        .map(String::as_str)
        .find(|clause| !names_condition_operator(clause))
}

/// `text` without `prefix`, comparing the prefix without regard to case.
fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let (head, rest) = text.split_at_checked(prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then_some(rest)
}

/// `text` without `suffix`, comparing the suffix without regard to case.
fn strip_suffix_ignore_ascii_case<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let (rest, tail) = text.split_at_checked(text.len().checked_sub(suffix.len())?)?;
    tail.eq_ignore_ascii_case(suffix).then_some(rest)
}
