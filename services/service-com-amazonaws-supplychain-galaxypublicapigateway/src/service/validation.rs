// SPDX-License-Identifier: CC0-1.0

//! How this service words a request the framework refused before any handler ran.
//!
//! The framework walks the modelled constraints and hands [`Refusal`] to `service.rs`
//! STRUCTURALLY — one [`Violation`] per offending value, carrying the path it sits at,
//! what it failed and what it held — and leaves the wording to the service, because no
//! model records it. This module is that wording for every modelled input constraint of
//! this service: one formatter, reached by every operation's boundary validation, so no
//! operation can word a constraint refusal its own way.
//!
//! The rule it implements, as AWS Supply Chain states a rejected input:
//!
//! * the clause names the offending MEMBER PATH and the CONSTRAINT that member violated,
//!   and NOTHING of the value the caller sent. An absent member is the one exception,
//!   reported as the `null` it was — a fact of the request's shape rather than a rendering
//!   of a value. Echoing the value is wrong here whatever the member: the service's
//!   recorded constraint refusals state the member and the requirement alone, and this
//!   service's data-plane payload (`SendDataIntegrationEvent$data`) is `@sensitive`, so
//!   reproducing caller input in an error body is the wrong default for the whole service
//!   — which is also the treatment the framework already gives a `@sensitive` member, here
//!   applied to every member;
//! * the requirement is stated in AWS's own vocabulary for each constraint — a bound on a
//!   length, a bound on a value, a regular expression, an enumerated value set, the
//!   uniqueness of a collection's elements — with the bound taken from the MODEL, so the
//!   number a refusal quotes is the one the model declares and cannot drift from it;
//! * the clauses are wrapped in AWS's envelope, which counts them: one violation reads
//!   `1 validation error detected: …`, several read `N validation errors detected: …`
//!   joined by `; `.
//!
//! Nothing here is keyed to an operation, a member name or a value: the rule is applied to
//! whatever [`Violation`] the walk produces, so it holds for every constrained member of
//! every operation in this service — the `@range` on a listing's `maxResults`, the
//! `@length` on an event payload, an instance description or a tag key, the `@pattern` on
//! a `UUID` or a resource ARN, and the value set of an `eventType` alike.
//!
//! A violation class this module has no wording for declines the WHOLE refusal (`None`),
//! which keeps the framework's default sentence rather than inventing one:
//! [`ViolationKind`] is `#[non_exhaustive]`, so a class can be added upstream.

use smithy_server_runtime::{Refusal, Refused, Violation, ViolationKind};

/// The error code this service answers an input-constraint violation with: its own
/// modelled `ValidationException`, which every operation of this service declares with
/// `@httpError 400` ("The input does not satisfy the constraints specified by an AWS
/// service"). It is the code the framework's default would have named too, restated
/// because `override_refusal` replaces code and message TOGETHER.
pub(crate) const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The member paths whose PUBLISHED minimum value differs from the one their model shape
/// declares, with the published one — so a refusal states the bound the SERVICE enforces
/// rather than the bound the model understates.
///
/// One row: a listing's `maxResults`. Every page-size shape of this service carries
/// `@range(max: 20)`, and the ones whose `@range` states a minimum at all state ONE
/// (`DataIntegrationEventMaxResults`, `DataLakeNamespaceMaxResults`,
/// `DataIntegrationFlowExecutionMaxResults`), while the rest state zero
/// (`InstanceMaxResults`, `DataLakeDatasetMaxResults`, `DataIntegrationFlowMaxResults`);
/// the recorded refusal of a listing asking for fewer than one names ONE as the minimum
/// even for a member of the second group. So the published minimum page size is one for
/// every listing of this service, stated here once — beside the handler that refuses a
/// value the understated bound lets through
/// ([`crate::ops::list_instances::MIN_MAX_RESULTS`]) — rather than per operation.
const PUBLISHED_MINIMUM_VALUES: &[(&str, &str)] = &[("maxResults", "1")];

/// The minimum `path` publishes, when that is not the one the model declares.
fn published_minimum(path: &str) -> Option<&'static str> {
    PUBLISHED_MINIMUM_VALUES
        .iter()
        .find(|(member, _)| *member == path)
        .map(|(_, minimum)| *minimum)
}

/// The requirement a violation at `path` states, worded as this service words it. `None`
/// for a class this service has no wording for.
fn requirement(path: &str, kind: &ViolationKind) -> Option<String> {
    let text = match kind {
        ViolationKind::MissingRequiredMember => "Member must not be null".to_string(),
        ViolationKind::MinLength(min) => {
            format!("Member must have length greater than or equal to {min}")
        }
        ViolationKind::MaxLength(max) => {
            format!("Member must have length less than or equal to {max}")
        }
        ViolationKind::MinValue(min) => {
            let min = published_minimum(path).unwrap_or(min);
            format!("Member must have value greater than or equal to {min}")
        }
        ViolationKind::MaxValue(max) => {
            format!("Member must have value less than or equal to {max}")
        }
        ViolationKind::Pattern(pattern) => {
            format!("Member must satisfy regular expression pattern: {pattern}")
        }
        ViolationKind::EnumValue(allowed) => {
            format!("Member must satisfy enum value set: [{}]", allowed.join(", "))
        }
        ViolationKind::UniqueItems => "Member must have unique values".to_string(),
        _ => return None,
    };
    Some(text)
}

/// One violation as a clause of the envelope, or `None` for a class this service has no
/// wording for.
fn clause(violation: &Violation) -> Option<String> {
    let requirement = requirement(&violation.path, &violation.kind)?;
    // The value is never rendered. An absent member is reported as the `null` it was,
    // which is the shape of the request rather than a value the caller sent.
    let subject = match &violation.kind {
        ViolationKind::MissingRequiredMember => "Value null",
        _ => "Value",
    };
    Some(format!(
        "{subject} at '{}' failed to satisfy constraint: {requirement}",
        violation.path
    ))
}

/// The `ValidationException` message for a refused request: AWS's envelope over one clause
/// per violation, in the order the walk found them.
///
/// `None` declines the whole refusal — see [`clause`].
pub(crate) fn envelope(violations: &[Violation]) -> Option<String> {
    let mut clauses: Vec<String> = Vec::with_capacity(violations.len());
    for violation in violations {
        clauses.push(clause(violation)?);
    }
    match clauses.len() {
        0 => None,
        1 => Some(format!("1 validation error detected: {}", clauses[0])),
        n => Some(format!(
            "{n} validation errors detected: {}",
            clauses.join("; ")
        )),
    }
}

/// How this service states `refusal`, or `None` to keep the framework's wording.
/// Only an input-constraint refusal is reworded. An authentication refusal is a different
/// failure with a different code and status, and nothing about it is this service's to
/// word.
pub(crate) fn refused(refusal: &Refusal) -> Option<Refused> {
    match refusal {
        Refusal::Invalid(violations) => Some(Refused {
            code: VALIDATION_EXCEPTION.to_string(),
            message: envelope(violations)?,
        }),
        _ => None,
    }
}

/// The message a HANDLER refuses one violation with, when the constraint it enforces is
/// one the model does not express (or understates) and so never reaches the boundary walk.
///
/// The same clause and the same envelope as a refusal the walk produced, so this service
/// states ONE wording for a rejected input whatever established it, and a handler cannot
/// word a constraint its own way. `path` is the member path the violation sits at, as the
/// request spells the member.
pub(crate) fn one_violation_message(path: &str, kind: ViolationKind) -> String {
    let violation = Violation {
        path: path.to_string(),
        kind,
        value: None,
        sensitive: false,
    };
    // One violation this module has wording for always renders; a class it does not word
    // is not reachable from a handler, which names the class itself.
    envelope(std::slice::from_ref(&violation)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A violation of `kind` at `path`, holding `value` — as the framework's walk reports
    /// one.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        }
    }

    /// RULE: the clause states the member path and the requirement, and never the value —
    /// whatever the violated constraint is, and whether or not the walk captured a value.
    ///
    /// The paths and bounds below are this service's own
    /// (`smithy_query(query="operation", operation="ListInstances,SendDataIntegrationEvent,CreateInstance")`),
    /// each row a different constraint class.
    #[test]
    fn no_clause_renders_the_value_the_caller_sent() {
        let rows = [
            (
                "a @range maximum",
                violation("maxResults", ViolationKind::MaxValue("20".to_string()), Some("21")),
                "Value at 'maxResults' failed to satisfy constraint: Member must have value \
                 less than or equal to 20",
            ),
            (
                "a @range minimum",
                violation("nextToken", ViolationKind::MinValue("0".to_string()), Some("-1")),
                "Value at 'nextToken' failed to satisfy constraint: Member must have value \
                 greater than or equal to 0",
            ),
            (
                // A member whose published minimum is higher than the modelled one states
                // the published bound ([`PUBLISHED_MINIMUM_VALUES`]).
                "a @range minimum the member's published bound overrides",
                violation("maxResults", ViolationKind::MinValue("0".to_string()), Some("-1")),
                "Value at 'maxResults' failed to satisfy constraint: Member must have value \
                 greater than or equal to 1",
            ),
            (
                "a @length maximum on a payload",
                violation("data", ViolationKind::MaxLength(1_048_576), Some("{\"a\":1}")),
                "Value at 'data' failed to satisfy constraint: Member must have length less \
                 than or equal to 1048576",
            ),
            (
                "a @length minimum",
                violation("eventGroupId", ViolationKind::MinLength(1), Some("")),
                "Value at 'eventGroupId' failed to satisfy constraint: Member must have length \
                 greater than or equal to 1",
            ),
            (
                "a @pattern",
                violation(
                    "instanceId",
                    ViolationKind::Pattern("^[a-f0-9]{8}$".to_string()),
                    Some("NOT-A-UUID"),
                ),
                "Value at 'instanceId' failed to satisfy constraint: Member must satisfy \
                 regular expression pattern: ^[a-f0-9]{8}$",
            ),
            (
                "an enumerated value set",
                violation(
                    "eventType",
                    ViolationKind::EnumValue(vec![
                        "scn.data.dataset".to_string(),
                        "scn.data.supplyplan".to_string(),
                    ]),
                    Some("scn.data.nothing"),
                ),
                "Value at 'eventType' failed to satisfy constraint: Member must satisfy enum \
                 value set: [scn.data.dataset, scn.data.supplyplan]",
            ),
            (
                "a collection element, by its position",
                violation("tags.1.member.key", ViolationKind::MinLength(1), Some("")),
                "Value at 'tags.1.member.key' failed to satisfy constraint: Member must have \
                 length greater than or equal to 1",
            ),
            (
                "@uniqueItems",
                violation("tagKeys", ViolationKind::UniqueItems, Some("team")),
                "Value at 'tagKeys' failed to satisfy constraint: Member must have unique \
                 values",
            ),
            (
                // The one exception: an absent member is reported as the `null` it was.
                "an absent required member",
                violation("instanceId", ViolationKind::MissingRequiredMember, None),
                "Value null at 'instanceId' failed to satisfy constraint: Member must not be \
                 null",
            ),
        ];
        for (case, violation, expected) in rows {
            let actual = clause(&violation);
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {case:?}: expected the member-and-requirement clause, got {actual:?}"
            );
        }
    }

    /// RULE: the envelope counts the clauses it carries, and joins several with `; `.
    #[test]
    fn the_envelope_counts_the_clauses() {
        let one = violation("maxResults", ViolationKind::MaxValue("20".to_string()), Some("21"));
        let other = violation("nextToken", ViolationKind::MinLength(1), Some(""));
        for (case, violations, expected) in [
            ("no violation", vec![], None),
            (
                "one violation",
                vec![one.clone()],
                Some(
                    "1 validation error detected: Value at 'maxResults' failed to satisfy \
                     constraint: Member must have value less than or equal to 20"
                        .to_string(),
                ),
            ),
            (
                "two violations",
                vec![one, other],
                Some(
                    "2 validation errors detected: Value at 'maxResults' failed to satisfy \
                     constraint: Member must have value less than or equal to 20; Value at \
                     'nextToken' failed to satisfy constraint: Member must have length greater \
                     than or equal to 1"
                        .to_string(),
                ),
            ),
        ] {
            let actual = envelope(&violations);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
