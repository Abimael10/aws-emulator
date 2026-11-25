// SPDX-License-Identifier: CC0-1.0

//! How this service states the framework's boundary refusal of a request.
//!
//! The words are not written here. `service_common::validation_message` is the
//! cross-service home for them — the `N validation error(s) detected:` envelope, the
//! member paths and every requirement sentence — and this module makes only the one
//! choice that belongs to a service: WHICH of those renderings this service answers.
//!
//! The rendering this service's refusals take: a clause names the member and the
//! requirement its value broke and NOTHING the request carried — `Value at
//! '<member path>' failed to satisfy constraint: <requirement>` — whichever member and
//! whichever constraint class. The requirement is the model's (a `@length` window, a
//! `@range` bound, a pattern, an enumerated type's value set); the value is the
//! caller's, who already holds it, and is not part of the sentence. A member that was
//! not in the request at all has no value to withhold and keeps the `null` an absent
//! member is stated with, which is the shape of the request rather than a value anyone
//! sent.
//!
//! Nothing here is keyed to an operation, to a member path, to a constraint class or to
//! a submitted value, so one rule covers every constrained member of every operation
//! alike: the page sizes and record counts a `@range` floors, and the stream ARNs, shard
//! ids, shard iterators and sequence numbers a `@length` window admits.
//!
//! The one thing this module decides about what goes INSIDE a clause is the ORDER an
//! enumerated type's value set is listed in: this front door holds the admissible values
//! in a table keyed on each value's own text and lists them as that table is walked —
//! ascending bucket of the value's string hash
//! ([`service_common::validation_message::in_hashed_enum_order`], the fleet's one home for
//! the derivation) — which is neither the order the model declares the type's members in
//! nor lexicographic. The order is computed from the values themselves, so it belongs to
//! the enumerated TYPE rather than to a member, an operation or a depth: one type is listed
//! alike wherever it is carried, and a type this model gains needs nothing added here.
//!
//! Two refusals are worded earlier still and never reach here: a request that OMITS the
//! identifier it addresses — a stream ARN, a shard iterator — draws that identifier's
//! own sentence ([`super::absent_identifier_refusal`]), and a `ListStreams` table name
//! the model's own constraints do not admit draws one sentence stating the length window
//! a name has to fit ([`super::table_name_refusal`]), with no envelope around either.

use smithy_server_runtime::{Refusal, Refused, Violation};

/// How this service states `refusal`, or `None` to keep the framework's wording.
///
/// `None` covers what is not an input-constraint refusal: an authentication refusal is a
/// different failure, with a code and a status that are not this service's to change.
pub(crate) fn refused(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    // An enumerated type's admissible values are listed in the order this front door's own
    // table of them is walked rather than in the order the schema presented them, so each
    // violation is re-ordered BEFORE the envelope words it: only the values move, and every
    // other violation class comes back untouched. Doing it on the one refusal this service
    // states is what makes the order hold for every enum of this model.
    let stated = Refusal::Invalid(
        violations
            .iter()
            .map(service_common::validation_message::in_hashed_enum_order)
            .collect(),
    );
    service_common::validation_message::aws_validation_refusal_withholding_violations(
        &stated,
        states_no_value,
    )
}

/// Whether the clause for `violation` states no value at all.
///
/// Asked about the VIOLATION rather than about a member path, because the decision is a
/// property of what was reported and not of which member reported it: every value a
/// request carried is withheld, and the one clause that still states a value half is the
/// member that carried none, stated as the `null` it was.
fn states_no_value(violation: &Violation) -> bool {
    violation.value.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_server_runtime::ViolationKind;

    /// One violation of `kind` at `path`, holding `value`.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        }
    }

    /// The choice this module makes, stated over the constraint classes no input shape of
    /// this model reaches over the wire as well as the ones it does — so the rule stays
    /// stated for a model that grows one. The wire cases live in
    /// `tests/all/bound_refusal_wording.rs` and `tests/all/probe_tmp.rs`.
    #[test]
    fn every_class_states_the_requirement_and_no_submitted_value() {
        // (case, the violation, the clause the message must carry)
        let rows = [
            (
                "a length floor",
                violation("sequenceNumber", ViolationKind::MinLength(21), Some("12345")),
                "Value at 'sequenceNumber' failed to satisfy constraint: Member must have \
                 length greater than or equal to 21",
            ),
            (
                "a length ceiling",
                violation("shardIterator", ViolationKind::MaxLength(2048), Some("xy")),
                "Value at 'shardIterator' failed to satisfy constraint: Member must have \
                 length less than or equal to 2048",
            ),
            (
                "a range floor",
                violation("limit", ViolationKind::MinValue("1".to_string()), Some("0")),
                "Value at 'limit' failed to satisfy constraint: Member must have value \
                 greater than or equal to 1",
            ),
            (
                "a range ceiling, which this model declares on no member",
                violation("limit", ViolationKind::MaxValue("100".to_string()), Some("101")),
                "Value at 'limit' failed to satisfy constraint: Member must have value less \
                 than or equal to 100",
            ),
            (
                // The set is listed in the order the table of these values is walked
                // (`LATEST` sits in a lower bucket than `TRIM_HORIZON`), which is not the
                // order they were handed over in.
                "an enumerated type's value set",
                violation(
                    "shardIteratorType",
                    ViolationKind::EnumValue(vec![
                        "TRIM_HORIZON".to_string(),
                        "LATEST".to_string(),
                    ]),
                    Some("SOMETIME"),
                ),
                "Value at 'shardIteratorType' failed to satisfy constraint: Member must \
                 satisfy enum value set: [LATEST, TRIM_HORIZON]",
            ),
            (
                // The member paths an absent-member clause is still stated at are the
                // ones this service does not address a position by: a request that
                // omitted a stream ARN or a shard iterator is worded before this module
                // is reached (`super::absent_identifier_refusal`).
                "a member absent from the request, which carried no value to withhold",
                violation(
                    "shardIteratorType",
                    ViolationKind::MissingRequiredMember,
                    None,
                ),
                "Value null at 'shardIteratorType' failed to satisfy constraint: Member must \
                 not be null",
            ),
        ];

        for (case, broken, clause) in rows {
            let refusal = Refusal::Invalid(vec![broken]);
            let stated = refused(&refusal)
                .unwrap_or_else(|| panic!("case {case:?}: the refusal must be stated"));
            assert_eq!(
                stated.message,
                format!("1 validation error detected: {clause}"),
                "case {case:?}: expected the clause {clause:?}, got {:?}",
                stated.message
            );
        }
    }

    /// The ORDER an enumerated type's value set is listed in belongs to the TYPE: it is
    /// derived from the values themselves, so the same set is listed the same way whichever
    /// order the schema presented it in, under whatever member path carried the bad value.
    ///
    /// The rows differ along what the derivation generalises over: the order the set is
    /// presented in (declared, reversed, rotated) and the member path it is reported at.
    #[test]
    fn an_enum_value_set_is_listed_in_the_order_the_table_is_walked() {
        let declared = crate::ops::types::SHARD_ITERATOR_TYPE_VALUES;
        let listed = format!(
            "[{}]",
            service_common::validation_message::hashed_enum_order(declared).join(", ")
        );
        let reversed: Vec<&str> = declared.iter().rev().copied().collect();
        let mut rotated: Vec<&str> = declared.to_vec();
        rotated.rotate_left(2);
        let rows: [(&str, &str, Vec<&str>); 3] = [
            ("as the model declares it", "shardIteratorType", declared.to_vec()),
            ("reversed", "shardIteratorType", reversed),
            ("rotated, at another member path", "iteratorType", rotated),
        ];

        for (case, path, presented) in rows {
            let refusal = Refusal::Invalid(vec![violation(
                path,
                ViolationKind::EnumValue(
                    presented.iter().map(|value| (*value).to_string()).collect(),
                ),
                Some("SOMETIME"),
            )]);
            let stated = refused(&refusal)
                .unwrap_or_else(|| panic!("case {case:?}: the refusal must be stated"))
                .message;
            let expected = format!(
                "1 validation error detected: Value at '{path}' failed to satisfy constraint: \
                 Member must satisfy enum value set: {listed}"
            );
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }

    /// Several broken requirements are stated as one envelope counting them, each clause
    /// withholding its own value.
    #[test]
    fn several_broken_requirements_are_counted_in_one_envelope() {
        let refusal = Refusal::Invalid(vec![
            violation("streamArn", ViolationKind::MinLength(37), Some("arn:aws")),
            violation("sequenceNumber", ViolationKind::MinLength(21), Some("123")),
        ]);
        let stated = refused(&refusal)
            .expect("a constraint refusal is stated")
            .message;
        let expected = "2 validation errors detected: Value at 'streamArn' failed to satisfy \
                        constraint: Member must have length greater than or equal to 37; Value \
                        at 'sequenceNumber' failed to satisfy constraint: Member must have \
                        length greater than or equal to 21";
        assert_eq!(
            stated, expected,
            "case two members outside their bounds: expected {expected:?}, got {stated:?}"
        );
    }

    /// Authentication is not an input refusal, so none of this touches it.
    #[test]
    fn an_authentication_refusal_is_not_reworded() {
        assert!(
            refused(&Refusal::Unauthenticated).is_none(),
            "case an unauthenticated caller: expected the framework's 403 wording to stand"
        );
    }
}
