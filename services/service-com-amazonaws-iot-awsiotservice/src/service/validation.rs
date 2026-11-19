// SPDX-License-Identifier: CC0-1.0

//! How this service words a request the framework refused before any handler ran.
//!
//! AWS IoT answers a request its validation layer will not accept with the error shape
//! this service's front door names for one — `InvalidRequestException`, the legacy IoT
//! exception family, rather than the generic constraint shape the framework picks — and
//! with AWS's own envelope over one clause per constraint the request broke:
//! `N validation error(s) detected: Value '<value>' at '<member path>' failed to satisfy
//! constraint: <requirement>`. The envelope and the clauses are
//! [`::service_common::validation_message::aws_validation_refusal_withholding_violations`]'s
//! — the member paths and every requirement are the framework's, so this service cannot
//! drift from them — and only the error code is this service's own.
//!
//! The other thing chosen here is the VALUE half of a clause, per constraint CLASS:
//!
//! * a member the request did not carry has no value to quote, and AWS states that clause
//!   with none at all — `Value at '<member path>' failed to satisfy constraint: Member must
//!   not be null` — where the framework writes the `null` it read;
//! * a member outside the `@pattern` of a THING-REGISTRY NAME — the one expression the
//!   registry's name types share ([`REGISTRY_NAME_EXPRESSION`]) — is stated WITH the name
//!   the request submitted, quoted, beside the expression that name had to match: the
//!   registry refuses a name it will not admit by naming it, which is the one thing such a
//!   refusal exists to carry. Every OTHER member outside its `@pattern` is stated with no
//!   value: the clause names the expression alone, which is what this service's recorded
//!   refusals of a domain-configuration name and of a transfer's target account show. The
//!   split is by the name TYPE the member targets, so it holds for every operation that
//!   takes one of those names and for every plane that takes any other member;
//! * a member outside either end of its `@length` is stated with no value as well: the
//!   clause names the member and the bound it broke, and the text that was too long or too
//!   short — which is the whole of what the caller sent for that member — is not reflected
//!   back. A value and a length bound say the same thing twice, and this service's recorded
//!   refusals of a member that broke its bound AND its expression together state neither
//!   clause with a value;
//! * a member outside either end of its `@range` is stated with no value for the same
//!   reason: the clause names the member and the numeric bound it broke, and the number that
//!   broke it is not echoed back;
//! * every other class keeps the value the request submitted — a value outside an
//!   enumerated set is quoted beside the set it had to be in — which is what the framework
//!   already states.
//!
//! The last thing chosen here is the ORDER an enumerated type's value set is listed in: the
//! front door in front of this service holds a type's admissible values in a HASH TABLE
//! keyed on each value's own text and enumerates that table, so the set is stated in
//! ascending bucket order rather than in the order the model declares the type
//! ([`::service_common::validation_message::in_hashed_enum_order`], whose own docs carry the
//! derivation). It is computed from the declared values themselves, so it belongs to the
//! enumerated TYPE: every enum of this model is listed that way and a type the model gains
//! needs nothing here.
//!
//! Nothing here is keyed to an operation, to a member path or to a submitted value, so one
//! rule covers every `@required` member, every `@length` and `@range` bound, every
//! `@pattern` and every enum of every operation alike — and the one split the classes make,
//! the registry's names against every other member, is read off the name TYPE the model
//! gives the member rather than off the request.
//!
//! The error-shape NAME this chooses is the bare one, because naming it on the wire is not
//! this seam's: the shape name a chosen refusal carries is reduced by the framework to the
//! characters a shape id holds, so the namespace AWS IoT qualifies it with cannot travel
//! through here. `ShapeNamespacedErrorType`, in front of every operation, is what writes
//! that name — for this shape wherever it comes from, so a refusal worded here and one a
//! handler answers name it identically.

use ::smithy_server_runtime::{Refusal, Refused, Violation, ViolationKind};

/// How this service states `refusal`, or `None` to keep the framework's own wording.
///
/// `None` covers both what this service does not reword (an authentication refusal is no
/// input-constraint failure, and its code and status are not this service's to change) and
/// a refusal with nothing to state.
pub(crate) fn refused(refusal: &Refusal) -> Option<Refused> {
    ::service_common::validation_message::aws_validation_refusal_withholding_violations(
        &in_stated_value_set_order(refusal),
        states_no_value,
    )
    .map(|stated| Refused {
        code: super::INVALID_REQUEST_EXCEPTION.to_owned(),
        message: stated.message,
    })
}

/// `refusal` with every enumerated type's value set in the order this service's front door
/// states it, and every other violation untouched.
fn in_stated_value_set_order(refusal: &Refusal) -> Refusal {
    match refusal {
        Refusal::Invalid(violations) => Refusal::Invalid(
            violations
                .iter()
                .map(::service_common::validation_message::in_hashed_enum_order)
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The expression every name type of the THING REGISTRY carries, as a clause quotes it:
/// without the whole-value anchors the model writes around it.
///
/// `ThingName`, `ThingGroupName`, `ThingTypeName` and `BillingGroupName` are one string type
/// repeated — `length 1..128, pattern ^[a-zA-Z0-9:_-]+$`
/// (`smithy_query(query="shape",shape="ThingName,ThingGroupName,ThingTypeName,BillingGroupName")`)
/// — so recognising the expression recognises a member that carries a registry NAME, on any
/// operation that takes one and wherever in the request it is bound. The anchors are already
/// off by the time a clause is stated ([`::service_common::validation::unanchored`]), which
/// is the form this names.
///
/// This text is the MODEL's, so it cannot be allowed to drift from it silently: the suite
/// `tests/all/not_found_wording/clause_value_classes.rs` drives all four of those name types
/// through the generated schemas over the wire and asserts the clause quotes exactly this
/// expression, so a model that re-spells the registry's pattern — or a change in how a
/// `@pattern` is requoted — fails there rather than quietly reclassifying the whole class.
const REGISTRY_NAME_EXPRESSION: &str = "[a-zA-Z0-9:_-]+";

/// Whether the clause for `violation` states no value: the member was ABSENT, so there is
/// no value to quote; it fell outside a `@pattern` that is not one of the registry's names,
/// whose clause names the expression alone; it fell outside its `@length` or its `@range`,
/// whose clause names the bound rather than the value that broke it; or it is one this
/// service never discloses.
///
/// What those classes have in common is that the clause already states the REQUIREMENT the
/// value broke — an expression, a length bound, a numeric bound — so quoting the value says
/// the same thing twice. The two classes stated WITH a value are the one whose requirement is
/// a SET of admissible values, which the framework already writes that way, and a THING-
/// REGISTRY name outside its expression, which the registry names when it refuses it. That
/// second one is why this is asked about the violation rather than about its member path: the
/// same member path states its value for this requirement and withholds it for the `@length`
/// bound of the very same type.
fn states_no_value(violation: &Violation) -> bool {
    let withheld_by_class = match &violation.kind {
        ViolationKind::Pattern(expression) => expression != REGISTRY_NAME_EXPRESSION,
        ViolationKind::MissingRequiredMember
        | ViolationKind::MinLength(_)
        | ViolationKind::MaxLength(_)
        | ViolationKind::MinValue(_)
        | ViolationKind::MaxValue(_) => true,
        // The classes whose requirement is a SET of admissible values, or the elements
        // themselves, are stated BESIDE the value they rejected.
        ViolationKind::EnumValue(_) | ViolationKind::UniqueItems => false,
        // `ViolationKind` is `#[non_exhaustive]`, so a class the framework gains cannot be
        // named here: until this service's recordings say otherwise it keeps the framework's
        // own value half, which is the value the request submitted.
        _ => false,
    };
    withheld_by_class || violation.sensitive
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One violation of `kind` at `path`, holding `value`.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_owned(),
            kind,
            value: value.map(str::to_owned),
            sensitive: false,
        }
    }

    /// RULE: a member the request did not carry, a member outside a `@pattern` that is not
    /// a registry name's, and a member outside its `@length` or its `@range` are each
    /// stated with NO value; a REGISTRY NAME outside its expression and every other
    /// constraint class state the value the request submitted; and a `@pattern` is quoted
    /// without the anchors a whole-value pattern wraps it in.
    #[test]
    fn an_absent_member_states_no_value_and_every_other_class_states_one() {
        // The rows span the classes the framework reports: an absent member, a value
        // outside a pattern (a registry name's and another plane's), one past each end of a
        // length bound, one past each end of a range bound and one outside an enumerated
        // set.
        let rows = [
            (
                "a member the request did not carry",
                violation("policyDocument", ViolationKind::MissingRequiredMember, None),
                "1 validation error detected: Value at 'policyDocument' failed to satisfy \
                 constraint: Member must not be null",
            ),
            (
                // A registry NAME is the one `@pattern` class stated with its value: the
                // same expression covers a thing, a thing group, a thing type and a
                // billing group, so this row stands for all four.
                "a registry name outside its expression",
                violation(
                    "thingName",
                    ViolationKind::Pattern("^[a-zA-Z0-9:_-]+$".to_owned()),
                    Some("not a thing name"),
                ),
                "1 validation error detected: Value 'not a thing name' at 'thingName' \
                 failed to satisfy constraint: Member must satisfy regular expression \
                 pattern: [a-zA-Z0-9:_-]+",
            ),
            (
                "a member of another plane outside its pattern",
                violation(
                    "policyName",
                    ViolationKind::Pattern("^[\\w+=,.@-]+$".to_owned()),
                    Some("not a policy name"),
                ),
                "1 validation error detected: Value at 'policyName' \
                 failed to satisfy constraint: Member must satisfy regular expression \
                 pattern: [\\w+=,.@-]+",
            ),
            (
                "a member past its length maximum",
                violation("policyName", ViolationKind::MaxLength(128), Some("pppp")),
                "1 validation error detected: Value at 'policyName' failed to satisfy \
                 constraint: Member must have length less than or equal to 128",
            ),
            (
                "a member short of its length minimum",
                violation("targetAwsAccount", ViolationKind::MinLength(12), Some("12345")),
                "1 validation error detected: Value at 'targetAwsAccount' failed to satisfy \
                 constraint: Member must have length greater than or equal to 12",
            ),
            (
                "a member past its range maximum",
                violation(
                    "credentialDurationSeconds",
                    ViolationKind::MaxValue("43200".to_owned()),
                    Some("46800"),
                ),
                "1 validation error detected: Value at 'credentialDurationSeconds' failed to \
                 satisfy constraint: Member must have value less than or equal to 43200",
            ),
            (
                "a member short of its range minimum",
                violation(
                    "credentialDurationSeconds",
                    ViolationKind::MinValue("900".to_owned()),
                    Some("899"),
                ),
                "1 validation error detected: Value at 'credentialDurationSeconds' failed to \
                 satisfy constraint: Member must have value greater than or equal to 900",
            ),
            (
                "a member outside its value set",
                violation(
                    "status",
                    ViolationKind::EnumValue(vec!["ACTIVE".to_owned()]),
                    Some("MAYBE"),
                ),
                "1 validation error detected: Value 'MAYBE' at 'status' failed to satisfy \
                 constraint: Member must satisfy enum value set: [ACTIVE]",
            ),
            (
                // `LogTargetType`, declared DEFAULT, THING_GROUP, CLIENT_ID, SOURCE_IP,
                // PRINCIPAL_ID, DEVICE_DEFENDER
                // (`smithy_query(query="shape",shape="LogTargetType")`), is stated in its
                // hash table's bucket order — 1, 11, 12, 12, 14, 15 — which is neither the
                // declaration order nor the alphabetical one, and where two values share a
                // bucket the later-declared one is stated first.
                "a member outside a value set of several members",
                violation(
                    "targetType",
                    ViolationKind::EnumValue(vec![
                        "DEFAULT".to_owned(),
                        "THING_GROUP".to_owned(),
                        "CLIENT_ID".to_owned(),
                        "SOURCE_IP".to_owned(),
                        "PRINCIPAL_ID".to_owned(),
                        "DEVICE_DEFENDER".to_owned(),
                    ]),
                    Some("NOT_A_REAL_TYPE"),
                ),
                "1 validation error detected: Value 'NOT_A_REAL_TYPE' at 'targetType' failed \
                 to satisfy constraint: Member must satisfy enum value set: [DEFAULT, \
                 SOURCE_IP, DEVICE_DEFENDER, PRINCIPAL_ID, THING_GROUP, CLIENT_ID]",
            ),
        ];

        for (case, violation, expected) in rows {
            let stated = refused(&Refusal::Invalid(vec![violation]))
                .unwrap_or_else(|| panic!("case {case:?}: expected a stated refusal"));
            assert_eq!(
                stated.code,
                super::super::INVALID_REQUEST_EXCEPTION,
                "case {case:?}: expected this service's own refusal class, got {:?}",
                stated.code
            );
            assert_eq!(
                stated.message, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                stated.message
            );
        }
    }

    /// RULE: a refusal that is no constraint violation keeps the framework's own wording.
    #[test]
    fn a_refusal_that_is_no_constraint_violation_is_left_alone() {
        for (case, refusal) in [
            ("no violations at all", Refusal::Invalid(Vec::new())),
            ("an unauthenticated caller", Refusal::Unauthenticated),
        ] {
            assert!(
                refused(&refusal).is_none(),
                "case {case:?}: expected the framework's own wording to stand"
            );
        }
    }
}
