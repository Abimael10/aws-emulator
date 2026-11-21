// SPDX-License-Identifier: CC0-1.0

//! WHICH values a member whose `@pattern` ALTERNATES whole-value branches admits, and the
//! refusal a request carrying another one gets.
//!
//! A Smithy `@pattern` is a FIND rather than a total match, and a model says "the whole value
//! has to match" by writing the anchor pair around the expression. An expression that
//! ALTERNATES at its top level is then anchored only at its two ENDS — `^(a)|()$` reads
//! "starts with `a`, or ends with nothing at all" — so the framework's own constraint walk
//! admits every value for it: the second branch matches the empty run at the end of any
//! value at all. AWS judges such an alternation against the WHOLE value, so a value no branch
//! matches end to end is a client-side validation failure there while the boundary walk here
//! lets it through.
//!
//! This module states that reading for the one such expression this model declares on INPUT
//! members: the "an ARN, or nothing at all" pattern its `ResourceArn`
//! (`DeadLetterConfig$TargetArn`) and `KMSKeyArn` (the function's own key member, a durable
//! configuration's, a package's and an event source mapping's) shapes carry
//! (`smithy_query(query="shape", shape="ResourceArn,KMSKeyArn")`). Every operation that
//! accepts one of those members asks [`arn_or_empty_refusal`], and asks it on the REQUEST,
//! before anything is read or written: a value that could never be valid is then the
//! validation error it is, rather than whatever refusal the addressed resource's current
//! state would have produced.
//!
//! The clause is the framework's own rather than a sentence spelled here — the member path in
//! its spelling, the requirement from its own renderer and its envelope around them — so a
//! value refused here reads exactly like one the walk catches at the boundary. The CHECK is
//! the model's own expression too, compiled by the regular-expression crate this crate now
//! declares ([`WHOLE_VALUE`]) rather than read by hand.

use ::aws_smithy_schema_server::constraints::{envelope, path, Violation, ViolationKind};
use ::service_common::validation::unanchored;

/// The `@pattern` of every member this module judges: an alternation of two WHOLE-VALUE
/// branches, an ARN or nothing at all.
pub const ARN_OR_EMPTY_PATTERN: &str = r"^(arn:(aws[a-zA-Z-]*)?:[a-z0-9-.]+:.*)|()$";

/// [`ARN_OR_EMPTY_PATTERN`] as the expression a WHOLE value has to match, compiled once.
///
/// The expression is the MODEL's, read rather than restated: the anchor pair the model wrote
/// around the alternation is dropped the way every clause of this service drops it
/// (`service_common::validation::unanchored`) and the whole-value match is then stated by
/// anchoring the alternation as a group — so the branches a value is judged against are the
/// model's own text, and what a refusal quotes is the same expression the check ran.
///
/// A pattern that fails to compile is treated as MATCHING, which is the policy the
/// framework's own constraint walk applies: no value is refused because an expression could
/// not be read.
static WHOLE_VALUE: ::std::sync::LazyLock<Option<::regex::Regex>> =
    ::std::sync::LazyLock::new(|| {
        ::regex::Regex::new(&format!("^(?:{})$", unanchored(ARN_OR_EMPTY_PATTERN))).ok()
    });

/// Whether [`ARN_OR_EMPTY_PATTERN`] matches `value` END TO END: nothing at all, or an ARN —
/// `arn:`, a partition that is either absent or `aws` followed by letters and hyphens, the
/// vendor namespace written in lowercase alphanumerics, dots and hyphens, and then the rest
/// of the ARN on one line.
#[must_use]
pub fn is_arn_or_empty(value: &str) -> bool {
    WHOLE_VALUE
        .as_ref()
        .is_none_or(|expression| expression.is_match(value))
}

/// The refusal a request carrying a value outside [`ARN_OR_EMPTY_PATTERN`] gets, or `None`
/// when every member it states carries a value the pattern admits.
///
/// Each entry is one member of the request: the chain of Smithy member names that REACHES it
/// — the member alone, or the structure's member and then the member inside it — and the
/// value the request carried for it, `None` for a member it left out. A member the request
/// omits states nothing to judge, and the pattern admits the EMPTY value, so a member stated
/// empty is admitted: that is how a caller clears one.
///
/// Every offending member is reported, in the order the entries are stated, because AWS
/// states one clause per broken constraint inside one envelope.
#[must_use]
pub fn arn_or_empty_refusal(members: &[(&[&str], Option<&str>)]) -> Option<String> {
    let violations: Vec<Violation> = members
        .iter()
        .filter_map(|(chain, value)| {
            let value = (*value)?;
            (!is_arn_or_empty(value)).then(|| Violation {
                path: member_path(chain),
                kind: ViolationKind::Pattern(unanchored(ARN_OR_EMPTY_PATTERN)),
                // The CHECK above ran against the value the caller SENT; only the clause's
                // echo of it is bounded, at this service's own ceiling, so one refusal
                // cannot be sized by the request.
                value: Some(
                    ::service_common::validation::quoted_echo_up_to(
                        value,
                        crate::service::LARGEST_QUOTED_VALUE_BYTES,
                    )
                    .into_owned(),
                ),
                sensitive: false,
            })
        })
        .collect();
    envelope(&violations)
}

/// `chain` as a validation clause spells the member it reaches: every Smithy member name in
/// the framework's own lowerCamel spelling, joined by its own separator.
fn member_path(chain: &[&str]) -> String {
    let segments: Vec<String> = chain.iter().copied().map(path::member).collect();
    path::join(&segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the alternation is matched against the WHOLE value — nothing at all, or an ARN
    /// whose partition and vendor namespace are the ones the branch writes — so a value that
    /// merely CONTAINS what a branch matches is not admitted.
    ///
    /// The rows span what the expression generalises over: the empty value, each partition
    /// form the branch admits, a vendor namespace built from each character class it names,
    /// an ARN whose remainder is empty and one carrying colons and slashes, and the ways a
    /// value falls outside — no prefix, a prefix with too few segments, a partition that is
    /// not the one named, an uppercase vendor namespace, and a line terminator in the
    /// remainder.
    #[test]
    fn the_pattern_is_matched_against_the_whole_value() {
        // (case, the value, whether the pattern admits it end to end)
        let rows: &[(&str, &str, bool)] = &[
            ("nothing at all", "", true),
            (
                "an ARN of the public partition",
                "arn:aws:sqs:us-west-2:000000000000:queue-of-failed-events",
                true,
            ),
            (
                "an ARN of a partition suffixed with letters and a hyphen",
                "arn:aws-us-gov:sns:us-gov-west-1:000000000000:topic",
                true,
            ),
            (
                "an ARN whose partition is absent, which the branch admits",
                "arn::kms:us-west-2:000000000000:key/0",
                true,
            ),
            (
                "a vendor namespace of lowercase letters, digits, dots and hyphens",
                "arn:aws:s3-outposts.9:us-west-2:000000000000:outpost/op-0",
                true,
            ),
            ("an ARN whose remainder is empty", "arn:aws:kms:", true),
            (
                "an ARN whose remainder carries colons and slashes",
                "arn:aws:kms:us-west-2:000000000000:key/11111111-2222-3333-4444-555555555555",
                true,
            ),
            ("a value that is not an ARN at all", "not-an-arn", false),
            (
                "a value that merely ENDS with what no branch matches",
                "the queue arn:aws:sqs:us-west-2:000000000000:q",
                false,
            ),
            ("a prefix with no vendor namespace", "arn:aws", false),
            ("a prefix with no remainder", "arn:aws:sqs", false),
            ("a partition that is not the one named", "arn:gcp:sqs:a:b:c", false),
            ("a vendor namespace in uppercase", "arn:aws:SQS:a:b:c", false),
            ("a vendor namespace with no characters", "arn:aws::a:b", false),
            (
                "a remainder carrying a line terminator",
                "arn:aws:sqs:us-west-2:000000000000:queue\nmore",
                false,
            ),
        ];
        for (case, value, admitted) in rows {
            let actual = is_arn_or_empty(value);
            assert_eq!(
                actual, *admitted,
                "case {case:?}: expected the pattern to admit {value:?} = {admitted}, got \
                 {actual}"
            );
        }
    }

    /// RULE: a member outside the pattern is refused with the framework's own clause — the
    /// member path in lowerCamel, the value quoted back and the expression the model
    /// publishes — and a request breaking the constraint in more than one member states one
    /// clause per member inside one envelope.
    #[test]
    fn a_refusal_states_the_member_and_the_expression() {
        let nested: &[&str] = &["DeadLetterConfig", "TargetArn"];
        let plain: &[&str] = &["KMSKeyArn"];
        let stated = arn_or_empty_refusal(&[(nested, Some("not-an-arn"))]);
        assert_eq!(
            stated.as_deref(),
            Some(
                "1 validation error detected: Value 'not-an-arn' at \
                 'deadLetterConfig.targetArn' failed to satisfy constraint: Member must \
                 satisfy regular expression pattern: \
                 (arn:(aws[a-zA-Z-]*)?:[a-z0-9-.]+:.*)|()"
            ),
            "expected the clause to name the member and the expression, got {stated:?}"
        );
        // A member the request omits, and one it states empty, state nothing to refuse.
        for (case, value) in [("an omitted member", None), ("an empty value", Some(""))] {
            let admitted = arn_or_empty_refusal(&[(plain, value)]);
            assert!(
                admitted.is_none(),
                "case {case:?}: expected no refusal, got {admitted:?}"
            );
        }
        // Two broken members are two clauses in one envelope, in the order they are stated.
        let both = arn_or_empty_refusal(&[(plain, Some("key-1")), (nested, Some("queue-1"))])
            .unwrap_or_default();
        assert!(
            both.starts_with("2 validation errors detected: Value 'key-1' at 'kMSKeyArn'")
                && both.contains("Value 'queue-1' at 'deadLetterConfig.targetArn'"),
            "expected one clause per broken member in one envelope, got {both:?}"
        );
    }
}
