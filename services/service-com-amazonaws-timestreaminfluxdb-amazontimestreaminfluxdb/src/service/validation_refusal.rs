// SPDX-License-Identifier: CC0-1.0

//! How this service states the framework's boundary refusal of a request whose values
//! break the constraints its model declares.
//!
//! The framework hands a service that refusal STRUCTURALLY — one
//! [`Violation`] per constraint the request broke — and lets the service choose the words,
//! because no model records them. Timestream for InfluxDB answers such a request
//! `400 ValidationException` (every operation that takes input declares it,
//! `smithy_query(query="operation", operation="CreateDbInstance")`) with a message built
//! from three parts, and all three differ from what the framework renders by default:
//!
//! 1. the ENVELOPE is `<N> validation error(s) detected: `, where the parenthesised `(s)`
//!    is fixed text this service emits whatever `N` is — the framework instead inflects the
//!    word itself (`1 validation error detected`, `2 validation errors detected`);
//! 2. a `@pattern` requirement quotes the expression WITHOUT the anchors a whole-value
//!    pattern is written with ([`::service_common::validation::unanchored`]). The check
//!    itself still uses the model's pattern as written: only the rendering drops them;
//! 3. a clause names the member and the requirement and states NO value — neither the
//!    value the request submitted nor the `null` the framework writes for a member that was
//!    absent. `Value at '<member path>' failed to satisfy constraint: <requirement>` is the
//!    whole clause, for every constraint class and every member shape.
//!
//! Nothing else is reworded: the member path, the requirement sentences and the separator
//! between clauses are the framework's and
//! [`::service_common::validation_message`]'s, so this service cannot drift from the rest
//! of the tree on any of them. The clause's own value half is dropped through that module's
//! [`::service_common::validation_message::valueless_clause`], the one spelling of that
//! drop in the tree.

use ::aws_smithy_schema_server::constraints::{Violation, ViolationKind};
use ::smithy_server_runtime::{Refusal, Refused};

/// The error code this service answers a constraint refusal with: the `ValidationException`
/// every operation that takes input declares (HTTP 400). Supplied with the message, so a
/// client branches on the code that matches the words it is shown.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The fixed text of the envelope, after the count: the literal parenthesised `(s)` is part
/// of it and is emitted for one violation as well as for many.
const ENVELOPE_SUFFIX: &str = " validation error(s) detected: ";

/// What separates two clauses of one message.
const CLAUSE_SEPARATOR: &str = "; ";

/// State `refusal` the way this service states it, or `None` to keep the framework's own
/// wording.
///
/// This is the whole [`::smithy_server_runtime::Service::override_refusal`] body for this
/// service. `None` for anything that is not a constraint refusal — an unauthenticated
/// caller keeps the framework's 403 code and wording — and `None` for a refusal carrying no
/// violation to state.
#[must_use]
pub fn constraint_refusal(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    let clauses: Vec<String> = violations
        .iter()
        // Bounded by the same ceiling the framework's own walk collects violations under,
        // so the length of the message is a constant of this crate rather than something
        // one request chooses.
        .take(::service_common::validation::MAX_VIOLATIONS)
        .map(requoted)
        .map(|violation| {
            ::service_common::validation_message::valueless_clause(&violation)
                .unwrap_or_else(|| violation.message())
        })
        .collect();
    if clauses.is_empty() {
        return None;
    }
    Some(Refused {
        code: VALIDATION_EXCEPTION.to_owned(),
        message: format!(
            "{}{ENVELOPE_SUFFIX}{}",
            clauses.len(),
            clauses.join(CLAUSE_SEPARATOR)
        ),
    })
}

/// `violation` with its `@pattern` requirement requoted the way this service quotes one:
/// the expression without the anchors the model writes it with. Every other class is
/// handed back untouched, so the requirement sentence stays the framework's.
fn requoted(violation: &Violation) -> Violation {
    match &violation.kind {
        ViolationKind::Pattern(pattern) => Violation {
            kind: ViolationKind::Pattern(::service_common::validation::unanchored(pattern)),
            ..violation.clone()
        },
        _ => violation.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One violation of `kind` at `path`, carrying the value a request submitted.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_owned(),
            kind,
            value: value.map(str::to_owned),
            sensitive: false,
        }
    }

    /// RULE: the envelope counts the clauses and always carries the literal `(s)`.
    #[test]
    fn the_envelope_states_the_clause_count_and_the_fixed_parenthesised_plural() {
        for (case, violations, expected_prefix) in [
            (
                "one violation",
                vec![violation("port", ViolationKind::MinValue("1024".to_owned()), Some("1"))],
                "1 validation error(s) detected: ",
            ),
            (
                "two violations",
                vec![
                    violation("port", ViolationKind::MinValue("1024".to_owned()), Some("1")),
                    violation("name", ViolationKind::MinLength(3), Some("ab")),
                ],
                "2 validation error(s) detected: ",
            ),
        ] {
            let refused = constraint_refusal(&Refusal::Invalid(violations))
                .unwrap_or_else(|| panic!("case {case:?}: a constraint refusal must be stated"));
            assert_eq!(
                refused.code, VALIDATION_EXCEPTION,
                "case {case:?}: expected the modelled code, got {:?}",
                refused.code
            );
            assert!(
                refused.message.starts_with(expected_prefix),
                "case {case:?}: expected the envelope {expected_prefix:?}, got {:?}",
                refused.message
            );
        }
    }

    /// RULE: a clause names the member and the requirement and states no value, and a
    /// `@pattern` requirement is quoted without the model's anchors.
    #[test]
    fn a_clause_states_no_value_and_quotes_a_pattern_unanchored() {
        for (case, broken, expected) in [
            (
                "a @pattern violation",
                violation(
                    "password",
                    ViolationKind::Pattern("^[a-zA-Z0-9]+$".to_owned()),
                    Some("bad-pass!1"),
                ),
                "1 validation error(s) detected: Value at 'password' failed to satisfy \
                 constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9]+",
            ),
            (
                "a @range minimum violation",
                violation(
                    "allocatedStorage",
                    ViolationKind::MinValue("20".to_owned()),
                    Some("10"),
                ),
                "1 validation error(s) detected: Value at 'allocatedStorage' failed to satisfy \
                 constraint: Member must have value greater than or equal to 20",
            ),
            (
                "an absent required member",
                violation("name", ViolationKind::MissingRequiredMember, None),
                "1 validation error(s) detected: Value at 'name' failed to satisfy constraint: \
                 Member must not be null",
            ),
        ] {
            let refused = constraint_refusal(&Refusal::Invalid(vec![broken]))
                .unwrap_or_else(|| panic!("case {case:?}: a constraint refusal must be stated"));
            assert_eq!(
                refused.message, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                refused.message
            );
        }
    }

    /// RULE: a refusal that is not about the request's values keeps the framework's wording.
    #[test]
    fn an_unauthenticated_caller_is_not_reworded() {
        assert!(
            constraint_refusal(&Refusal::Unauthenticated).is_none(),
            "case \"an unauthenticated caller\": expected the framework's own wording"
        );
    }
}
