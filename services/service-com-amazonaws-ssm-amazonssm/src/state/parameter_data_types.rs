// SPDX-License-Identifier: CC0-1.0

//! What a parameter's `DataType` says about its VALUE, and the sentence a value that does
//! not satisfy its data type is refused with.
//!
//! `ParameterDataType` is a plain string carrying a length (0..128) and NO enum and no
//! pattern (`smithy_query(query="shape",shape="ParameterDataType")`), so every data-type
//! string of that length is a value the member admits: `PutParameter`'s own documentation
//! LISTS the data types it describes ([`super::DOCUMENTED_DATA_TYPES`]) rather than closing
//! the set the wire takes. What the service holds a put to is therefore the VALUE against
//! the data type the request declares — a data type names the form the value has to be in —
//! and not the data type's own spelling.
//!
//! A data type can also be one the service serves only on an ADVANCED parameter
//! ([`ADVANCED_ONLY_DATA_TYPES`]): the tier a put resolves to decides whether it may be
//! declared at all, and a put that resolves to any other tier is refused with the sentence
//! naming that data type ([`advanced_only_message`]).
//!
//! Two kinds of value validation follow from that, and they are refused differently:
//!
//! * the form a data type states of the value, decided HERE from the value alone, which
//!   refuses the put outright with that data type's own sentence — nothing is written and no
//!   version is allocated;
//! * `aws:ec2:image`, whose value is checked against a catalogue that lives in another
//!   service and whose documented asynchrony makes the put answer success either way
//!   ([`super::parameter_image_validation`]).
//!
//! A data type this service states no value form for — `text`, which holds a value to
//! nothing beyond the value's own constraints, `aws:ssm:integration`, and any other string
//! a caller declares — accepts whatever value the put carries.

/// The `DataType` whose value is a whole number.
///
/// Not one of the data types `PutParameter`'s documentation lists, and that is the point:
/// the member admits it, and what the service validates under it is the value's FORM, which
/// [`INTEGER_VALUE_REJECTION`] states in full.
pub const DATA_TYPE_INTEGER: &str = "integer";

/// The sentence a value that is not a whole number is refused under
/// [`DATA_TYPE_INTEGER`] with: fixed text, carrying nothing from the request or from the
/// store, which states the form the value has to be in.
///
/// The form this module decides is the one the sentence states, so the refusal and the rule
/// cannot drift apart: an optional minus sign, then digits, with no leading zeros, no plus
/// sign, no decimal point and no whitespace.
pub const INTEGER_VALUE_REJECTION: &str = "The parameter value is not a valid integer. A \
     value must be an optional minus sign followed by digits, with no leading zeros, no plus \
     sign, no decimal point and no whitespace.";

/// Why `value` is not a value of `data_type`, or `None` when the data type states no form
/// for it or the value is in that form.
///
/// The decision is made from the value alone, so a put is refused before anything is read or
/// written. `data_type` is the one IN FORCE — the request's, else the one the parameter being
/// updated already carries — because the form a value has to be in is a property of the
/// parameter's data type rather than of one request's members.
pub fn value_rejection(data_type: &str, value: &str) -> Option<String> {
    match data_type {
        DATA_TYPE_INTEGER => {
            (!is_integer(value)).then(|| INTEGER_VALUE_REJECTION.to_string())
        }
        _ => None,
    }
}

/// Whether `value` is the whole number [`INTEGER_VALUE_REJECTION`] describes.
///
/// "An optional minus sign followed by digits, with no leading zeros, no plus sign, no
/// decimal point and no whitespace": one optional `-`, then at least one ASCII digit and
/// nothing else — which admits `0` but no longer run of digits that starts with one, and
/// rejects a plus sign, a decimal point, whitespace, an exponent, a thousands separator and
/// a digit of any other script, each of which is not an ASCII digit.
fn is_integer(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty()
        && digits.bytes().all(|digit| digit.is_ascii_digit())
        && (digits.len() == 1 || !digits.starts_with('0'))
}

/// The `DataType` whose value is a web address.
pub const DATA_TYPE_URL: &str = "url";

/// The data types the service serves only on an ADVANCED parameter.
///
/// A data type here is one a standard parameter may not declare at all: the tier a put
/// resolves to is what decides it, and the refusal names the data type
/// ([`advanced_only_message`]). The listing is the data types this service answers that
/// sentence for; every other data type is served in whichever tier the put resolves to.
pub const ADVANCED_ONLY_DATA_TYPES: &[&str] = &[DATA_TYPE_URL];

/// The sentence a put declaring an advanced-only data type outside the advanced tier is
/// refused with: fixed text around the data type the request declared, which is the one
/// value of the request it carries.
pub fn advanced_only_message(data_type: &str) -> String {
    format!("The {data_type} data type is only supported for advanced parameters.")
}

/// Why `data_type` may not be declared by a parameter in `tier`, or `None` when it may.
///
/// `tier` is the tier the put RESOLVED to rather than the one its `Tier` member named, so a
/// request that asked the service to choose (`Intelligent-Tiering`) is judged against the
/// tier it was served in; `data_type` is the one in force, as in [`value_rejection`].
pub fn tier_rejection(data_type: &str, tier: &str) -> Option<String> {
    (ADVANCED_ONLY_DATA_TYPES.contains(&data_type) && tier != super::TIER_ADVANCED)
        .then(|| advanced_only_message(data_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the value form a data type states is what decides a put, and a data type that
    /// states none accepts every value.
    ///
    /// The rows differ along what the rule generalises over: the data type declared (one
    /// stating a form, every data type the documentation lists, and a caller-invented one)
    /// and the shape of the value (a whole number, a signed one, zero, and each way the
    /// sentence's form can be broken).
    #[test]
    fn a_value_is_judged_against_the_form_its_data_type_states() {
        // (case, data type, value, whether it is refused)
        let rows: Vec<(&str, &str, &str, bool)> = vec![
            ("a port number", DATA_TYPE_INTEGER, "443", false),
            ("zero", DATA_TYPE_INTEGER, "0", false),
            ("a negative number", DATA_TYPE_INTEGER, "-17", false),
            ("a long run of digits", DATA_TYPE_INTEGER, "900719925474099", false),
            ("a leading zero", DATA_TYPE_INTEGER, "0443", true),
            ("a plus sign", DATA_TYPE_INTEGER, "+443", true),
            ("a decimal point", DATA_TYPE_INTEGER, "44.3", true),
            ("trailing whitespace", DATA_TYPE_INTEGER, "443 ", true),
            ("inner whitespace", DATA_TYPE_INTEGER, "4 43", true),
            ("a minus sign alone", DATA_TYPE_INTEGER, "-", true),
            ("no value at all", DATA_TYPE_INTEGER, "", true),
            ("words", DATA_TYPE_INTEGER, "four hundred", true),
            ("an exponent", DATA_TYPE_INTEGER, "4e3", true),
            // Every data type the documentation lists states no form this module decides,
            // so each takes the same value the integer form refuses.
            ("the default data type", super::super::DEFAULT_DATA_TYPE, "44.3", false),
            ("an image data type", super::super::DATA_TYPE_EC2_IMAGE, "44.3", false),
            ("an integration data type", "aws:ssm:integration", "44.3", false),
            ("a caller-invented data type", "text/plain", "44.3", false),
        ];
        for (case, data_type, value, refused) in rows {
            let rejection = value_rejection(data_type, value);
            assert_eq!(
                rejection.is_some(),
                refused,
                "case {case:?}: expected {data_type} value {value:?} to be \
                 {}, got {rejection:?}",
                if refused { "refused" } else { "accepted" }
            );
            if refused {
                assert_eq!(
                    rejection.as_deref(),
                    Some(INTEGER_VALUE_REJECTION),
                    "case {case:?}: expected the value-level sentence, got {rejection:?}"
                );
            }
        }
    }

    /// RULE: a data type the service serves only on an advanced parameter may be declared
    /// only by a put that resolved to the advanced tier.
    ///
    /// The rows differ along the tier the put resolved to (each `ParameterTier` value the
    /// model declares) and the data type declared (an advanced-only one, the default, and one
    /// no documentation names).
    #[test]
    fn an_advanced_only_data_type_is_refused_outside_the_advanced_tier() {
        // (case, data type, resolved tier, whether it is refused)
        let rows: Vec<(&str, &str, &str, bool)> = vec![
            ("a web address in the standard tier", DATA_TYPE_URL, super::super::TIER_STANDARD, true),
            ("a web address in the advanced tier", DATA_TYPE_URL, super::super::TIER_ADVANCED, false),
            // `Intelligent-Tiering` is a tier a put is NEVER served in: it asks the service
            // to resolve one ([`super::super::resolve_tier`]), so a parameter reported in it
            // is a parameter the resolution never produced.
            (
                "a web address under a tier that resolves to no parameter",
                DATA_TYPE_URL,
                super::super::TIER_INTELLIGENT_TIERING,
                true,
            ),
            ("the default data type in the standard tier", super::super::DEFAULT_DATA_TYPE, super::super::TIER_STANDARD, false),
            ("a whole number in the standard tier", DATA_TYPE_INTEGER, super::super::TIER_STANDARD, false),
            ("a caller-invented data type in the standard tier", "text/plain", super::super::TIER_STANDARD, false),
        ];
        for (case, data_type, tier, refused) in rows {
            let rejection = tier_rejection(data_type, tier);
            assert_eq!(
                rejection.is_some(),
                refused,
                "case {case:?}: expected {data_type} in {tier} to be {}, got {rejection:?}",
                if refused { "refused" } else { "accepted" }
            );
            if refused {
                assert_eq!(
                    rejection.as_deref(),
                    Some(advanced_only_message(data_type).as_str()),
                    "case {case:?}: expected the tier-level sentence, got {rejection:?}"
                );
            }
        }
    }
}
