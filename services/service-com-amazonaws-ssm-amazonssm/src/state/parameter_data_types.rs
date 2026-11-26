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
//!
//! ## The sentence a refused value is answered with
//!
//! RECORDED BEHAVIOUR (corpus `simparity-behavioral-aws-v1`, units
//! `ssm.put-parameter-typed-data-types-round-trip` and
//! `ssm.put-parameter-rejects-malformed-typed-values`): the sentences below are fixed text
//! per data type, carrying nothing from the request or from the store, and they come in two
//! shapes. A data type whose sentence states the FORM says the same thing of every value
//! outside it (`integer`, `float`, `binary`, `boolean`, the two AWS reference types); one
//! whose sentence states the DEFECT the value has says a different thing per defect (a host
//! name, an email address, the two address forms), so a caller reading it learns what to
//! change. Every form here is the one its own sentence states, so the rule and the refusal
//! cannot drift apart.
//!
//! Each sentence says whose words it is. The ones the corpus records are marked RECORDED and
//! are reproduced verbatim — one per data type whose sentence states the form, and, for the
//! defect-stating families, the defect the recording exercises. A defect the recording does
//! not carry has no recorded sentence to reproduce, so such a value is refused with the
//! CLASS alone — the opening of that family's recorded sentence, nothing worded here — and
//! this emulator states no second clause of its own.

/// The `DataType` whose value names an AWS Region.
pub const DATA_TYPE_AWS_REGION: &str = "aws:region";

/// The `DataType` whose value is an AWS account id.
pub const DATA_TYPE_AWS_ACCOUNT_ID: &str = "aws:account:id";

/// The `DataType` whose value is a truth value.
pub const DATA_TYPE_BOOLEAN: &str = "boolean";

/// The `DataType` whose value is a whole number.
///
/// Not one of the data types `PutParameter`'s documentation lists, and that is the point:
/// the member admits it, and what the service validates under it is the value's FORM, which
/// [`INTEGER_VALUE_REJECTION`] states.
pub const DATA_TYPE_INTEGER: &str = "integer";

/// The `DataType` whose value is a number that may carry a fraction.
pub const DATA_TYPE_FLOAT: &str = "float";

/// The `DataType` whose value is a binary number — the binary digits themselves, as
/// [`BINARY_VALUE_REJECTION`] states.
pub const DATA_TYPE_BINARY: &str = "binary";

/// The `DataType` whose value is a host name.
pub const DATA_TYPE_HOSTNAME: &str = "hostname";

/// The `DataType` whose value is an email address.
pub const DATA_TYPE_EMAIL: &str = "email";

/// The `DataType` whose value is an IPv4 address.
pub const DATA_TYPE_IPV4_ADDRESS: &str = "ipv4-address";

/// The `DataType` whose value is an IPv6 address.
pub const DATA_TYPE_IPV6_ADDRESS: &str = "ipv6-address";

/// The `DataType` whose value is a web address.
pub const DATA_TYPE_URL: &str = "url";

/// The `DataType` whose value is a JSON document.
pub const DATA_TYPE_JSON: &str = "json";

/// The `DataType` whose value is a JSON document that may carry comments.
pub const DATA_TYPE_JSONC: &str = "jsonc";

/// The sentence a value that is not an AWS Region is refused under
/// [`DATA_TYPE_AWS_REGION`] with: fixed text for this data type, carrying nothing from the
/// request or from the store, which names where the Regions are published. RECORDED.
pub const AWS_REGION_VALUE_REJECTION: &str = "The parameter value is not a valid AWS Region. \
     Please check https://docs.aws.amazon.com/global-infrastructure/latest/regions/aws-regions.html \
     for the list of valid regions.";

/// The sentence a value that is not an AWS account id is refused under
/// [`DATA_TYPE_AWS_ACCOUNT_ID`] with. RECORDED, as every sentence here is fixed text.
pub const AWS_ACCOUNT_ID_VALUE_REJECTION: &str = "The parameter value is not a valid AWS account ID.";

/// The sentence a value that is not a truth value is refused under [`DATA_TYPE_BOOLEAN`]
/// with, which LISTS the values the data type accepts — and is the one place that listing is
/// written, since [`BOOLEAN_VALUES`] is read from the same sentence. RECORDED.
pub const BOOLEAN_VALUE_REJECTION: &str = "The parameter value is not a valid boolean. \
     Accepted values are true, True, TRUE, false, False, FALSE, 1 and 0.";

/// The sentence a value that is not a whole number is refused under [`DATA_TYPE_INTEGER`]
/// with, which states the form the value has to be in — and the form [`is_integer`] decides
/// is the one the sentence states, so the refusal and the rule cannot drift apart. RECORDED.
pub const INTEGER_VALUE_REJECTION: &str = "The parameter value is not a valid integer. A \
     value must be an optional minus sign followed by digits, with no leading zeros, no plus \
     sign, no decimal point and no whitespace.";

/// The sentence a value that is not a number is refused under [`DATA_TYPE_FLOAT`] with,
/// which states the form the value has to be in — and the form [`is_float`] decides is the
/// one the sentence states, so the refusal and the rule cannot drift apart. RECORDED.
pub const FLOAT_VALUE_REJECTION: &str = "The parameter value is not a valid float. A value \
     must be a decimal number such as 1.5, .5 or 2.5e-8, with an optional minus sign and no \
     leading zeros. NaN and Infinity are not accepted.";

/// The sentence a value that is not a binary number is refused under [`DATA_TYPE_BINARY`]
/// with, which states the form the value has to be in — and the form [`is_binary`] decides
/// is the one the sentence states, so the refusal and the rule cannot drift apart. RECORDED,
/// and it is what says the value is a binary NUMBER rather than encoded data.
pub const BINARY_VALUE_REJECTION: &str = "The parameter value is not a valid binary number. \
     A value must be one or more binary digits, 0 or 1, with no prefix, sign, separator or \
     whitespace.";

/// The sentence a value outside the HOST NAME form is refused with when the defect it has
/// is not the one the corpus records: the class alone, which is the opening of the recorded
/// sentence below and states nothing this service was not told.
///
/// A host name's refusal names WHAT is wrong with the value where the service's words for
/// that defect are known ([`hostname_rejection`] picks them); for every other defect it
/// states the class and stops, rather than wording a second clause of its own.
pub const HOSTNAME_REJECTION_OPENING: &str = "The parameter value is not a valid host name.";

/// The sentence a value carrying an underscore gets — the defect the service calls out by
/// name, since an underscore is the character a host name is most often written with and
/// never admits. RECORDED: this is the host-name defect the corpus exercises.
pub const HOSTNAME_UNDERSCORE_REJECTION: &str = "The parameter value is not a valid host \
     name. It contains an underscore, which host names do not allow.";

/// The sentence a value outside the EMAIL ADDRESS form is refused with when its defect is
/// not the recorded one: the class alone, as [`HOSTNAME_REJECTION_OPENING`] is for a host
/// name.
pub const EMAIL_REJECTION_OPENING: &str = "The parameter value is not a valid email address.";

/// The sentence a value carrying whitespace or a control character gets — neither an atom of
/// a mailbox name nor a label of a domain admits either. RECORDED: this is the email-address
/// defect the corpus exercises.
pub const EMAIL_WHITESPACE_REJECTION: &str = "The parameter value is not a valid email \
     address. It contains whitespace or a control character.";

/// The sentence a value outside the IPv4 ADDRESS form is refused with when its defect is not
/// the recorded one: the class alone, as the two families above.
pub const IPV4_REJECTION_OPENING: &str = "The parameter value is not a valid IPv4 address.";

/// The sentence a value carrying anything but the characters an address is written with
/// gets. RECORDED: this is the IPv4 defect the corpus exercises.
pub const IPV4_CHARACTER_REJECTION: &str = "The parameter value is not a valid IPv4 address. \
     It may contain only the digits 0 to 9 and dots.";

/// The sentence a value outside the IPv6 ADDRESS form is refused with when its defect is not
/// the recorded one: the class alone, as [`IPV4_REJECTION_OPENING`] is for the other address
/// form.
pub const IPV6_REJECTION_OPENING: &str = "The parameter value is not a valid IPv6 address.";

/// See [`IPV4_CHARACTER_REJECTION`], for the other address form: the characters an IPv6
/// address is written with are the hexadecimal digits, the group separator and the dots of
/// an embedded IPv4 address. RECORDED, the spellings it rules out included.
pub const IPV6_CHARACTER_REJECTION: &str = "The parameter value is not a valid IPv6 address. \
     It may contain only hexadecimal digits, colons, and the dots of an embedded IPv4 \
     address. Zone IDs, brackets and prefix lengths are not accepted.";

/// The truth values [`DATA_TYPE_BOOLEAN`] accepts, read off the sentence a value outside
/// them is refused with ([`BOOLEAN_VALUE_REJECTION`]: "accepted values are true, True, TRUE,
/// false, False, FALSE, 1 and 0") — so the listing and the refusal cannot drift apart. The
/// three spellings of each word and the two digits, and no other case of either word.
pub const BOOLEAN_VALUES: &[&str] = &["true", "True", "TRUE", "false", "False", "FALSE", "1", "0"];

/// The longest a host NAME is: a DNS name is at most 253 characters (RFC 1123 §2.1).
const MAX_HOSTNAME_CHARACTERS: usize = 253;

/// The longest ONE label of a host name is (RFC 1123 §2.1).
const MAX_HOSTNAME_LABEL_CHARACTERS: usize = 63;

/// The form ONE data type states of a value: the data type, what a value of it has to
/// satisfy, and the sentence a value outside that form is refused with.
///
/// A table rather than a match arm per type, so the data types this service states a form
/// for are one listing — the thing a reader of a refusal, and
/// [`value_rejection`], both go through.
struct ValueForm {
    /// The `DataType` this form belongs to.
    data_type: &'static str,
    /// Whether a value is in the form, decided from the value alone.
    satisfied_by: fn(&str) -> bool,
    /// The sentence a value outside it is refused with.
    rejection: Rejection,
}

/// How a data type words the refusal of a value outside its form.
///
/// Two kinds, because the service words them two ways: a data type whose sentence states the
/// FORM says the same thing of every value outside it, while one whose sentence states the
/// DEFECT it found says a different thing per defect.
enum Rejection {
    /// One sentence for every value outside the form.
    Stated(&'static str),
    /// The sentence for the defect this value has.
    PerDefect(fn(&str) -> &'static str),
}

/// The value form each data type this service validates a value under states.
///
/// Every row is a data type whose NAME states the form its value is in, so the value is the
/// thing a put carrying it is held to; a data type absent from this table states no form
/// (see the module header) and takes whatever value the put carries.
const VALUE_FORMS: &[ValueForm] = &[
    ValueForm {
        data_type: DATA_TYPE_AWS_REGION,
        satisfied_by: is_aws_region,
        rejection: Rejection::Stated(AWS_REGION_VALUE_REJECTION),
    },
    ValueForm {
        data_type: DATA_TYPE_AWS_ACCOUNT_ID,
        satisfied_by: is_aws_account_id,
        rejection: Rejection::Stated(AWS_ACCOUNT_ID_VALUE_REJECTION),
    },
    ValueForm {
        data_type: DATA_TYPE_BOOLEAN,
        satisfied_by: is_boolean,
        rejection: Rejection::Stated(BOOLEAN_VALUE_REJECTION),
    },
    ValueForm {
        data_type: DATA_TYPE_INTEGER,
        satisfied_by: is_integer,
        rejection: Rejection::Stated(INTEGER_VALUE_REJECTION),
    },
    ValueForm {
        data_type: DATA_TYPE_FLOAT,
        satisfied_by: is_float,
        rejection: Rejection::Stated(FLOAT_VALUE_REJECTION),
    },
    ValueForm {
        data_type: DATA_TYPE_BINARY,
        satisfied_by: is_binary,
        rejection: Rejection::Stated(BINARY_VALUE_REJECTION),
    },
    ValueForm {
        data_type: DATA_TYPE_HOSTNAME,
        satisfied_by: is_hostname,
        rejection: Rejection::PerDefect(hostname_rejection),
    },
    ValueForm {
        data_type: DATA_TYPE_EMAIL,
        satisfied_by: is_email_address,
        rejection: Rejection::PerDefect(email_rejection),
    },
    ValueForm {
        data_type: DATA_TYPE_IPV4_ADDRESS,
        satisfied_by: is_ipv4_address,
        rejection: Rejection::PerDefect(ipv4_rejection),
    },
    ValueForm {
        data_type: DATA_TYPE_IPV6_ADDRESS,
        satisfied_by: is_ipv6_address,
        rejection: Rejection::PerDefect(ipv6_rejection),
    },
];

/// Why `value` is not a value of `data_type`, or `None` when the data type states no form
/// for it or the value is in that form.
///
/// The decision is made from the value alone, so a put is refused before anything is read or
/// written. `data_type` is the one IN FORCE — the request's, else the one the parameter being
/// updated already carries — because the form a value has to be in is a property of the
/// parameter's data type rather than of one request's members.
pub fn value_rejection(data_type: &str, value: &str) -> Option<String> {
    let form = VALUE_FORMS
        .iter()
        .find(|form| form.data_type == data_type)?;
    if (form.satisfied_by)(value) {
        return None;
    }
    Some(
        match form.rejection {
            Rejection::Stated(sentence) => sentence,
            Rejection::PerDefect(sentence_of) => sentence_of(value),
        }
        .to_string(),
    )
}

/// Whether `value` is one of the Regions AWS publishes.
///
/// Decided against the Region catalogue this emulator publishes as AWS's own public
/// parameters ([`super::global_infrastructure::PUBLISHED_REGIONS`], the list the refusal
/// points a caller at), so the Regions a typed parameter may name and the Regions a read of
/// `/aws/service/global-infrastructure/regions` reports are ONE listing.
fn is_aws_region(value: &str) -> bool {
    super::global_infrastructure::PUBLISHED_REGIONS.contains(&value)
}

/// Whether `value` is an AWS account id, read by the one reading of that form this tree
/// holds (`service_common::arn::is_account_id`) rather than by a second copy of it here.
fn is_aws_account_id(value: &str) -> bool {
    ::service_common::arn::is_account_id(value)
}

/// Whether `value` is one of the truth values [`BOOLEAN_VALUES`] lists, which is what the
/// refusal states a value of this data type has to be.
fn is_boolean(value: &str) -> bool {
    BOOLEAN_VALUES.contains(&value)
}

/// Whether `value` is a whole number: an optional minus sign, then digits, with no leading
/// zeros, no plus sign, no decimal point and no whitespace.
///
/// One optional `-`, then at least one ASCII digit and nothing else — which admits `0` but no
/// longer run of digits that starts with one, and rejects a plus sign, a decimal point,
/// whitespace, an exponent, a thousands separator and a digit of any other script, each of
/// which is not an ASCII digit.
fn is_integer(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty()
        && digits.bytes().all(|digit| digit.is_ascii_digit())
        && (digits.len() == 1 || !digits.starts_with('0'))
}

/// Whether `value` is the decimal number [`FLOAT_VALUE_REJECTION`] describes: "a decimal
/// number such as 1.5, .5 or 2.5e-8, with an optional minus sign and no leading zeros", and
/// neither of the two names that spell something which is not a number.
///
/// So: one optional `-`, a decimal mantissa ([`is_decimal`]) and an optional power-of-ten
/// exponent ([`is_exponent`]). Every character is an ASCII digit or one of that form's own,
/// which is what leaves `NaN` and `Infinity` out: they are names rather than digits.
fn is_float(value: &str) -> bool {
    let body = value.strip_prefix('-').unwrap_or(value);
    let (mantissa, exponent) = match body.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (body, None),
    };
    is_decimal(mantissa) && exponent.is_none_or(is_exponent)
}

/// Whether `text` is the MANTISSA of [`is_float`]: digits, or digits and a fraction, or a
/// fraction alone (`.5`), with no leading zeros on the whole part — which admits `0` itself
/// but no longer run of digits that starts with one — and no sign, no whitespace and no
/// separator.
fn is_decimal(text: &str) -> bool {
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text, None),
    };
    let digits_only = |part: &str| part.bytes().all(|digit| digit.is_ascii_digit());
    // A fraction that is present carries digits, and a value with no fraction at all is the
    // whole part alone — so neither `1.` nor `.` nor the empty value is one.
    let fraction_is_one = match fraction {
        Some(fraction) => !fraction.is_empty() && digits_only(fraction),
        None => !whole.is_empty(),
    };
    fraction_is_one
        && digits_only(whole)
        && (whole.len() <= 1 || !whole.starts_with('0'))
        && (!whole.is_empty() || fraction.is_some())
}

/// Whether `text` is the EXPONENT of [`is_float`]: an optional sign, then digits.
fn is_exponent(text: &str) -> bool {
    let digits = text
        .strip_prefix('-')
        .or_else(|| text.strip_prefix('+'))
        .unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|digit| digit.is_ascii_digit())
}

/// Whether `value` is the binary number [`BINARY_VALUE_REJECTION`] describes: "one or more
/// binary digits, 0 or 1, with no prefix, sign, separator or whitespace".
///
/// So the digits alone: no `0b` prefix, no sign, no separator, no whitespace, and not the
/// empty value, which is no digit at all.
fn is_binary(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|digit| matches!(digit, b'0' | b'1'))
}

/// Whether `value` is a host name: dot-separated labels of ASCII letters, digits and
/// hyphens, each at most [`MAX_HOSTNAME_LABEL_CHARACTERS`] long and neither opening nor
/// closing with a hyphen, over at most [`MAX_HOSTNAME_CHARACTERS`] in all (RFC 1123 §2.1).
///
/// One label is a host name — a name need not be fully qualified — and a trailing dot is
/// not part of one here, since an empty label is no label at all.
fn is_hostname(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_HOSTNAME_CHARACTERS
        && value.split('.').all(is_hostname_label)
}

/// Whether `value` is ONE label of a host name ([`is_hostname`]).
fn is_hostname_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_HOSTNAME_LABEL_CHARACTERS
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// The sentence a value that is NOT a host name ([`is_hostname`]) is refused with.
///
/// The service names WHAT is wrong with the value where its words for that defect are known:
/// the underscore the corpus records ([`HOSTNAME_UNDERSCORE_REJECTION`]). Every other defect
/// — whitespace, an empty label, a length, a hyphen at a label's edge, any other character —
/// has no recorded sentence, so the refusal states the class and stops
/// ([`HOSTNAME_REJECTION_OPENING`]) rather than wording a clause of this emulator's own.
fn hostname_rejection(value: &str) -> &'static str {
    if value.contains('_') {
        return HOSTNAME_UNDERSCORE_REJECTION;
    }
    HOSTNAME_REJECTION_OPENING
}

/// Whether `value` is an email address, read by the one reading of that form this tree
/// holds (`service_common::email_address`) rather than by a second copy of it here.
fn is_email_address(value: &str) -> bool {
    ::service_common::email_address::is_email_address(value)
}

/// The sentence a value that is NOT an email address ([`is_email_address`]) is refused
/// with.
///
/// As with a host name: the defect the corpus records — whitespace or a control character,
/// which the shared reading of the form classifies
/// (`service_common::email_address::address_defect`) — gets the recorded sentence, and every
/// other defect states the class and stops ([`EMAIL_REJECTION_OPENING`]).
fn email_rejection(value: &str) -> &'static str {
    use ::service_common::email_address::AddressDefect;
    match ::service_common::email_address::address_defect(value) {
        Some(
            AddressDefect::LocalControlOrWhitespace | AddressDefect::DomainControlOrWhitespace,
        ) => EMAIL_WHITESPACE_REJECTION,
        _ => EMAIL_REJECTION_OPENING,
    }
}

/// Whether `value` is an IPv4 address, read by the standard library's own parse of one —
/// dotted quad, no leading zeros, no abbreviation.
fn is_ipv4_address(value: &str) -> bool {
    value.parse::<::std::net::Ipv4Addr>().is_ok()
}

/// The sentence a value that is NOT an IPv4 address ([`is_ipv4_address`]) is refused with:
/// the recorded one for the defect the corpus exercises — a character no address is written
/// with — and the class alone for every other ([`IPV4_REJECTION_OPENING`]).
fn ipv4_rejection(value: &str) -> &'static str {
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return IPV4_CHARACTER_REJECTION;
    }
    IPV4_REJECTION_OPENING
}

/// Whether `value` is an IPv6 address, read by the standard library's own parse of one.
fn is_ipv6_address(value: &str) -> bool {
    value.parse::<::std::net::Ipv6Addr>().is_ok()
}

/// The sentence a value that is NOT an IPv6 address ([`is_ipv6_address`]) is refused with,
/// read the way [`ipv4_rejection`] reads its own: the recorded character-set sentence, which
/// is also what rules out a zone id, brackets and a prefix length, and the class alone for
/// every other defect.
fn ipv6_rejection(value: &str) -> &'static str {
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() || matches!(byte, b':' | b'.'))
    {
        return IPV6_CHARACTER_REJECTION;
    }
    IPV6_REJECTION_OPENING
}

/// The data types the service serves only on an ADVANCED parameter.
///
/// A data type here is one a standard parameter may not declare at all: the tier a put
/// resolves to is what decides it, and the refusal names the data type
/// ([`advanced_only_message`]). The listing is the data types this service answers that
/// sentence for; every other data type is served in whichever tier the put resolves to.
pub const ADVANCED_ONLY_DATA_TYPES: &[&str] =
    &[DATA_TYPE_URL, DATA_TYPE_JSON, DATA_TYPE_JSONC];

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
    /// The rows differ along what the rule generalises over: the data type declared (each one
    /// stating a form, every data type the documentation lists, and a caller-invented one)
    /// and the shape of the value (one in the form, and each way that form can be broken).
    #[test]
    fn a_value_is_judged_against_the_form_its_data_type_states() {
        // (case, data type, value, the sentence it is refused with, if any)
        let rows: Vec<(&str, &str, &str, Option<&str>)> = vec![
            // A Region of the published catalogue, one of another partition, one that is
            // only Region-shaped, and a Region the catalogue does not publish.
            ("a commercial Region", DATA_TYPE_AWS_REGION, "us-east-1", None),
            ("a China Region", DATA_TYPE_AWS_REGION, "cn-north-1", None),
            ("a GovCloud Region", DATA_TYPE_AWS_REGION, "us-gov-west-1", None),
            (
                "a Region-shaped code nothing publishes",
                DATA_TYPE_AWS_REGION,
                "us-east-9",
                Some(AWS_REGION_VALUE_REJECTION),
            ),
            (
                "a Region's long name",
                DATA_TYPE_AWS_REGION,
                "US East (N. Virginia)",
                Some(AWS_REGION_VALUE_REJECTION),
            ),
            ("twelve digits", DATA_TYPE_AWS_ACCOUNT_ID, "123456789012", None),
            ("twelve digits opening with a zero", DATA_TYPE_AWS_ACCOUNT_ID, "012345678901", None),
            (
                "eleven digits",
                DATA_TYPE_AWS_ACCOUNT_ID,
                "12345678901",
                Some(AWS_ACCOUNT_ID_VALUE_REJECTION),
            ),
            (
                "a hyphenated account id",
                DATA_TYPE_AWS_ACCOUNT_ID,
                "1234-5678-9012",
                Some(AWS_ACCOUNT_ID_VALUE_REJECTION),
            ),
            ("true", DATA_TYPE_BOOLEAN, "true", None),
            ("TRUE", DATA_TYPE_BOOLEAN, "TRUE", None),
            ("False", DATA_TYPE_BOOLEAN, "False", None),
            ("the digit one", DATA_TYPE_BOOLEAN, "1", None),
            ("the digit zero", DATA_TYPE_BOOLEAN, "0", None),
            (
                "a spelling of true the sentence does not accept",
                DATA_TYPE_BOOLEAN,
                "tRuE",
                Some(BOOLEAN_VALUE_REJECTION),
            ),
            ("yes", DATA_TYPE_BOOLEAN, "yes", Some(BOOLEAN_VALUE_REJECTION)),
            ("a port number", DATA_TYPE_INTEGER, "443", None),
            ("zero", DATA_TYPE_INTEGER, "0", None),
            ("a negative number", DATA_TYPE_INTEGER, "-17", None),
            ("a long run of digits", DATA_TYPE_INTEGER, "900719925474099", None),
            ("a plus sign", DATA_TYPE_INTEGER, "+443", Some(INTEGER_VALUE_REJECTION)),
            ("a leading zero", DATA_TYPE_INTEGER, "0443", Some(INTEGER_VALUE_REJECTION)),
            ("a decimal point", DATA_TYPE_INTEGER, "44.3", Some(INTEGER_VALUE_REJECTION)),
            ("trailing whitespace", DATA_TYPE_INTEGER, "443 ", Some(INTEGER_VALUE_REJECTION)),
            ("a sign alone", DATA_TYPE_INTEGER, "-", Some(INTEGER_VALUE_REJECTION)),
            ("no value at all", DATA_TYPE_INTEGER, "", Some(INTEGER_VALUE_REJECTION)),
            ("words", DATA_TYPE_INTEGER, "four hundred", Some(INTEGER_VALUE_REJECTION)),
            ("an exponent", DATA_TYPE_INTEGER, "4e3", Some(INTEGER_VALUE_REJECTION)),
            ("a fraction", DATA_TYPE_FLOAT, "3.14", None),
            ("a whole number", DATA_TYPE_FLOAT, "3", None),
            ("zero", DATA_TYPE_FLOAT, "0", None),
            ("a negative fraction", DATA_TYPE_FLOAT, "-0.5", None),
            ("a fraction with no whole part", DATA_TYPE_FLOAT, ".5", None),
            ("an exponent", DATA_TYPE_FLOAT, "2.5e-8", None),
            ("an upper-case exponent", DATA_TYPE_FLOAT, "1E6", None),
            ("a plus sign", DATA_TYPE_FLOAT, "+1.5", Some(FLOAT_VALUE_REJECTION)),
            ("a leading zero", DATA_TYPE_FLOAT, "007.5", Some(FLOAT_VALUE_REJECTION)),
            ("a fraction with no digits", DATA_TYPE_FLOAT, "1.", Some(FLOAT_VALUE_REJECTION)),
            ("an infinity", DATA_TYPE_FLOAT, "Infinity", Some(FLOAT_VALUE_REJECTION)),
            ("not a number", DATA_TYPE_FLOAT, "NaN", Some(FLOAT_VALUE_REJECTION)),
            ("two decimal points", DATA_TYPE_FLOAT, "1.2.3", Some(FLOAT_VALUE_REJECTION)),
            ("an exponent with no digits", DATA_TYPE_FLOAT, "1.5e", Some(FLOAT_VALUE_REJECTION)),
            ("words", DATA_TYPE_FLOAT, "three", Some(FLOAT_VALUE_REJECTION)),
            ("a binary number", DATA_TYPE_BINARY, "1010101", None),
            ("one digit", DATA_TYPE_BINARY, "0", None),
            ("a prefixed binary number", DATA_TYPE_BINARY, "0b1010", Some(BINARY_VALUE_REJECTION)),
            ("a digit that is not binary", DATA_TYPE_BINARY, "1021", Some(BINARY_VALUE_REJECTION)),
            ("a separator", DATA_TYPE_BINARY, "1010 1010", Some(BINARY_VALUE_REJECTION)),
            ("no value at all", DATA_TYPE_BINARY, "", Some(BINARY_VALUE_REJECTION)),
            ("a qualified name", DATA_TYPE_HOSTNAME, "db.example.com", None),
            ("one label", DATA_TYPE_HOSTNAME, "localhost", None),
            ("a hyphenated label", DATA_TYPE_HOSTNAME, "my-host-01.example", None),
            ("a label opening with a hyphen", DATA_TYPE_HOSTNAME, "-host.example", Some(HOSTNAME_REJECTION_OPENING)),
            ("an empty label", DATA_TYPE_HOSTNAME, "host..example", Some(HOSTNAME_REJECTION_OPENING)),
            ("a space", DATA_TYPE_HOSTNAME, "my host", Some(HOSTNAME_REJECTION_OPENING)),
            ("an underscore", DATA_TYPE_HOSTNAME, "my_host", Some(HOSTNAME_UNDERSCORE_REJECTION)),
            ("an underscore in a qualified name", DATA_TYPE_HOSTNAME, "my_host.example.com", Some(HOSTNAME_UNDERSCORE_REJECTION)),
            ("a character no host name carries", DATA_TYPE_HOSTNAME, "host!.example", Some(HOSTNAME_REJECTION_OPENING)),
            ("a label past its length", DATA_TYPE_HOSTNAME, "this-label-is-far-too-long-to-be-one-label-of-a-host-name-at-all.example", Some(HOSTNAME_REJECTION_OPENING)),
            ("a mailbox", DATA_TYPE_EMAIL, "ops@example.com", None),
            ("a dotted local part", DATA_TYPE_EMAIL, "first.last@mail.example.com", None),
            ("no domain separator", DATA_TYPE_EMAIL, "ops.example.com", Some(EMAIL_REJECTION_OPENING)),
            ("no mailbox", DATA_TYPE_EMAIL, "@example.com", Some(EMAIL_REJECTION_OPENING)),
            ("whitespace in the mailbox name", DATA_TYPE_EMAIL, "ops team@example.com", Some(EMAIL_WHITESPACE_REJECTION)),
            ("whitespace in the domain", DATA_TYPE_EMAIL, "ops@exa mple.com", Some(EMAIL_WHITESPACE_REJECTION)),
            ("a doubled dot", DATA_TYPE_EMAIL, "ops..team@example.com", Some(EMAIL_REJECTION_OPENING)),
            ("a domain of one label", DATA_TYPE_EMAIL, "ops@example", Some(EMAIL_REJECTION_OPENING)),
            ("a character no address carries", DATA_TYPE_EMAIL, "ops(team)@example.com", Some(EMAIL_REJECTION_OPENING)),
            ("a dotted quad", DATA_TYPE_IPV4_ADDRESS, "10.0.0.1", None),
            ("the broadcast address", DATA_TYPE_IPV4_ADDRESS, "255.255.255.255", None),
            ("an octet past 255", DATA_TYPE_IPV4_ADDRESS, "10.0.0.256", Some(IPV4_REJECTION_OPENING)),
            ("three octets", DATA_TYPE_IPV4_ADDRESS, "10.0.0", Some(IPV4_REJECTION_OPENING)),
            ("an IPv6 address", DATA_TYPE_IPV4_ADDRESS, "2001:db8::1", Some(IPV4_CHARACTER_REJECTION)),
            ("a host name", DATA_TYPE_IPV4_ADDRESS, "example.com", Some(IPV4_CHARACTER_REJECTION)),
            ("a full address", DATA_TYPE_IPV6_ADDRESS, "2001:0db8:0000:0000:0000:0000:0000:0001", None),
            ("an abbreviated address", DATA_TYPE_IPV6_ADDRESS, "2001:db8::1", None),
            ("the loopback address", DATA_TYPE_IPV6_ADDRESS, "::1", None),
            ("an IPv4 address", DATA_TYPE_IPV6_ADDRESS, "10.0.0.1", Some(IPV6_REJECTION_OPENING)),
            ("two abbreviations", DATA_TYPE_IPV6_ADDRESS, "2001::db8::1", Some(IPV6_REJECTION_OPENING)),
            ("a digit that is not hexadecimal", DATA_TYPE_IPV6_ADDRESS, "2001:dg8::1", Some(IPV6_CHARACTER_REJECTION)),
            ("an address carrying a zone id", DATA_TYPE_IPV6_ADDRESS, "fe80::1%eth0", Some(IPV6_CHARACTER_REJECTION)),
            // Every data type the documentation lists states no form this module decides,
            // so each takes a value the forms above refuse.
            ("the default data type", super::super::DEFAULT_DATA_TYPE, "44.3", None),
            ("an image data type", super::super::DATA_TYPE_EC2_IMAGE, "44.3", None),
            ("an integration data type", "aws:ssm:integration", "44.3", None),
            ("a caller-invented data type", "text/plain", "44.3", None),
        ];
        for (case, data_type, value, refused) in rows {
            let rejection = value_rejection(data_type, value);
            assert_eq!(
                rejection.as_deref(),
                refused,
                "case {case:?}: expected {data_type} value {value:?} to be refused with \
                 {refused:?}, got {rejection:?}"
            );
        }
    }

    /// RULE: a data type the service serves only on an advanced parameter may be declared
    /// only by a put that resolved to the advanced tier.
    ///
    /// The rows differ along the tier the put resolved to (each `ParameterTier` value the
    /// model declares) and the data type declared (each advanced-only one, the default, and
    /// one no documentation names).
    #[test]
    fn an_advanced_only_data_type_is_refused_outside_the_advanced_tier() {
        // (case, data type, resolved tier, whether it is refused)
        let rows: Vec<(&str, &str, &str, bool)> = vec![
            ("a web address in the standard tier", DATA_TYPE_URL, super::super::TIER_STANDARD, true),
            ("a web address in the advanced tier", DATA_TYPE_URL, super::super::TIER_ADVANCED, false),
            ("a document in the standard tier", DATA_TYPE_JSON, super::super::TIER_STANDARD, true),
            ("a document in the advanced tier", DATA_TYPE_JSON, super::super::TIER_ADVANCED, false),
            ("a commented document in the standard tier", DATA_TYPE_JSONC, super::super::TIER_STANDARD, true),
            ("a commented document in the advanced tier", DATA_TYPE_JSONC, super::super::TIER_ADVANCED, false),
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

    /// RULE: the recorded sentence a defect-stating family answers is the one a value with
    /// THAT defect gets, and a value whose defect the family has no recorded sentence for is
    /// refused with the class alone — which is the opening of that recorded sentence, so a
    /// client matching on the class matches either.
    ///
    /// Driven through [`value_rejection`] over a value per family that has the recorded
    /// defect and one that has another.
    #[test]
    fn a_defect_stating_family_answers_its_recorded_sentence_or_the_class_alone() {
        // (case, data type, the value with the recorded defect, a value with another)
        let rows: Vec<(&str, &str, &str, &str, &str)> = vec![
            (
                "a host name's underscore",
                DATA_TYPE_HOSTNAME,
                "my_host",
                "my host",
                HOSTNAME_UNDERSCORE_REJECTION,
            ),
            (
                "an email address's whitespace",
                DATA_TYPE_EMAIL,
                "ops team@example.com",
                "ops.example.com",
                EMAIL_WHITESPACE_REJECTION,
            ),
            (
                "an IPv4 address's character set",
                DATA_TYPE_IPV4_ADDRESS,
                "example.com",
                "10.0.0.256",
                IPV4_CHARACTER_REJECTION,
            ),
            (
                "an IPv6 address's character set",
                DATA_TYPE_IPV6_ADDRESS,
                "2001:dg8::1",
                "2001::db8::1",
                IPV6_CHARACTER_REJECTION,
            ),
        ];
        for (case, data_type, recorded, other, sentence) in rows {
            let answered = value_rejection(data_type, recorded);
            assert_eq!(
                answered.as_deref(),
                Some(sentence),
                "case {case:?}: expected the recorded sentence for {recorded:?}, got {answered:?}"
            );
            let class = value_rejection(data_type, other);
            // The class is the recorded sentence up to and including its first full stop:
            // what is left once the defect clause is dropped.
            let opening = sentence.find(". ").map_or(sentence, |at| &sentence[..=at]);
            assert_eq!(
                class.as_deref(),
                Some(opening),
                "case {case:?}: expected the class alone for {other:?}, got {class:?}"
            );
        }
    }

    /// RULE: the truth values the boolean data type accepts are the ones its refusal lists,
    /// so the listing and the sentence cannot drift apart.
    #[test]
    fn every_accepted_truth_value_is_named_by_the_sentence() {
        for value in BOOLEAN_VALUES {
            assert!(
                BOOLEAN_VALUE_REJECTION.contains(*value),
                "case {value:?}: expected the sentence to name the accepted value, got \
                 {BOOLEAN_VALUE_REJECTION:?}"
            );
            assert!(
                value_rejection(DATA_TYPE_BOOLEAN, value).is_none(),
                "case {value:?}: expected the accepted value to pass, got {:?}",
                value_rejection(DATA_TYPE_BOOLEAN, value)
            );
        }
    }
}
