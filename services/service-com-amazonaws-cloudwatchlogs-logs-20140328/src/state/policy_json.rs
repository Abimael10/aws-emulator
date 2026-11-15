// SPDX-License-Identifier: CC0-1.0

//! The refusal CloudWatch Logs answers a data protection `policyDocument` it cannot PARSE as
//! JSON at all.
//!
//! The document is "the data protection policy, in JSON", so a document that is not JSON
//! never reaches the rules the member's own documentation states about what a policy SAYS:
//! the service reports its JSON parser's own diagnostic instead, under a prefix of its own
//! ([`INVALID_POLICY_JSON_PREFIX`]). The diagnostic names what the parse stopped on, what
//! could have been there instead, and WHERE in the submitted document that was — the only
//! diagnostic a caller holding a 30,720-character document has — and it names nothing of the
//! document itself, which this service does not echo back.
//!
//! The readings are not this service's: where a parse stopped, and what it stopped on, are
//! the same questions about the same document wherever a service takes an opaque JSON
//! member, so they come from [`::service_common::json_parse_fault`]. Only the SENTENCE below
//! is this service's own.

use ::service_common::json_parse_fault as fault;

/// What CloudWatch Logs says before its parser's diagnostic, for a policy document it could
/// not read.
const INVALID_POLICY_JSON_PREFIX: &str = "Invalid policy JSON: ";

/// `Some(message)` when `document` is not JSON, carrying the parser's diagnostic of why, and
/// `None` when it parses — a document that parses and is not the object a policy is expressed
/// as breaks a rule about the POLICY, which is its caller's sentence rather than the parser's.
#[must_use]
pub fn unreadable_policy_json_message(document: &str) -> Option<String> {
    let error = ::serde_json::from_str::<::serde_json::Value>(document).err()?;
    // A bare word where a value belongs is a token the parser cannot recognise, reported with
    // the classes a value could have been written as and the position that word BEGINS at;
    // every other fault names what the parse stopped on, at the position it stopped
    // ([`fault::unexpected_input`], [`fault::stopped_at`]).
    let (said, line, column) = match fault::unrecognized_token(document, &error) {
        Some((token, line, column)) => (
            format!(
                "Unrecognized token '{token}': was expecting ({})",
                fault::VALUE_CLASSES
            ),
            line,
            column,
        ),
        None => {
            let (line, column) = fault::stopped_at(document, &error);
            (fault::unexpected_input(document, &error), line, column)
        }
    };
    Some(format!(
        "{INVALID_POLICY_JSON_PREFIX}{said}{}",
        fault::source_location(line, column)
    ))
}

#[cfg(test)]
mod tests {
    use super::unreadable_policy_json_message;

    /// The clause a diagnostic about a value position carries, as the shared reading states
    /// the classes a JSON value may be written as.
    const EXPECTED_A_VALUE: &str = ": was expecting (JSON String, Number, Array, Object or \
                                    token 'null', 'true' or 'false')";

    /// RULE: a document that is not JSON is refused with the parser's diagnostic under this
    /// service's prefix, naming where the parse stopped and nothing of the document; a
    /// document that parses is not refused here at all.
    #[test]
    fn the_refusal_carries_the_parsers_diagnostic_and_position() {
        for (case, document, said) in [
            (
                "a bare word at the start",
                "this is not json",
                Some((format!("Unrecognized token 'this'{EXPECTED_A_VALUE}"), 1, 1)),
            ),
            (
                "a word a literal starts like",
                "not json",
                Some((format!("Unrecognized token 'not'{EXPECTED_A_VALUE}"), 1, 1)),
            ),
            (
                "a word where a member's value belongs",
                "{\"Statement\": oops}",
                Some((format!("Unrecognized token 'oops'{EXPECTED_A_VALUE}"), 1, 15)),
            ),
            (
                "a word on a later line",
                "\n\nfalsy",
                Some((format!("Unrecognized token 'falsy'{EXPECTED_A_VALUE}"), 3, 1)),
            ),
            (
                "an unquoted member name",
                "{Statement: []}",
                Some((
                    "Unexpected character ('S' (code 83)): was expecting double-quote to start \
                     field name"
                        .to_string(),
                    1,
                    2,
                )),
            ),
            (
                "a document that ends early",
                "{\"Statement\":",
                Some((
                    "Unexpected end-of-input: expected a value".to_string(),
                    1,
                    14,
                )),
            ),
            ("a document that parses", "{\"Statement\": []}", None),
            ("a literal parses", "null", None),
        ] {
            let actual = unreadable_policy_json_message(document);
            match said {
                Some((stated, line, column)) => {
                    let message = actual.clone().unwrap_or_else(|| {
                        panic!("case {case:?}: expected a refusal, got {actual:?}")
                    });
                    let expected = format!(
                        "Invalid policy JSON: {stated}\n at [Source: REDACTED \
                         (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: \
                         {line}, column: {column}]"
                    );
                    assert_eq!(
                        message, expected,
                        "case {case:?}: expected {expected:?}, got {message:?}"
                    );
                    assert!(
                        !message.contains(document),
                        "case {case:?}: the refusal echoes no part of the document, got \
                         {message:?}"
                    );
                }
                None => assert!(
                    actual.is_none(),
                    "case {case:?}: expected no refusal, got {actual:?}"
                ),
            }
        }
    }
}
