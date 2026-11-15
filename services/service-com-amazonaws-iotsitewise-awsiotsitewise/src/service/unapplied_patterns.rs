// SPDX-License-Identifier: CC0-1.0

//! The `@pattern` the boundary declares but cannot APPLY, applied.
//!
//! The framework's constraint walk compiles a member's `@pattern` with the `regex`
//! crate, whose syntax has no look-around, and it treats an expression it cannot
//! compile as matching (`pattern_matches` in
//! `vendor/smithy-schema-server/src/constraints.rs`). Several of this model's members
//! declare an expression that opens with a negative lookahead — every `CustomID`
//! reference does, to exclude the nil UUID — so the requirement the model states for
//! them never reaches a request, and a value breaking BOTH that expression and
//! another of the member's bounds is refused with only the other one. AWS states
//! every requirement the member broke.
//!
//! So the expression is applied here, by a look-around-capable engine over the
//! MODEL's own text, and the violation it produces is added to the refusal the
//! framework already built ([`applied`]). The expressions come from the input shape
//! the request is read against ([`super::input_shape`]), and the value is the one the
//! walk already captured on the violation, so nothing re-reads the request.
//!
//! That bounds what this can state to the members the walk ALREADY refused for
//! another reason: a request whose only fault is such an expression carries no
//! violation at all, and is refused by the operation that reads the member
//! ([`crate::state::custom_id_clause`]), in the same words.

use ::smithy_server_runtime::{Violation, ViolationKind};

/// The step budget one match may spend.
///
/// The engine below BACKTRACKS, so a match has no linear-time guarantee in general
/// and the work one refusal may cost is bounded here rather than left to the shape of
/// the expression. Both inputs are already tame — the expression is a model constant,
/// never request text, and the value is the echo the walk stored, which that walk has
/// already held to its own ceiling — so this is the backstop for a model expression
/// that is not: a match that exhausts it is answered, never spun on.
const BACKTRACK_LIMIT: usize = 1_000_000;

/// `violations` with one `@pattern` violation added per member whose value does not
/// satisfy the expression `declared` records for it and whose refusal does not state
/// that expression already.
///
/// Added rather than replaced: every violation the walk found is kept, and the order a
/// member's several requirements are stated in belongs to [`super::validation`].
pub(super) fn applied(
    violations: &[Violation],
    declared: &[(String, &'static str)],
) -> Vec<Violation> {
    if declared.is_empty() {
        return violations.to_vec();
    }
    let mut restored = violations.to_vec();
    let mut stated: Vec<&str> = violations
        .iter()
        .filter_map(|violation| {
            matches!(violation.kind, ViolationKind::Pattern(_)).then_some(violation.path.as_str())
        })
        .collect();
    for violation in violations {
        // The value a clause carries is the only value in hand, and a value the walk
        // WITHHELD or ELIDED is not the one the caller sent, so no expression is
        // applied to it.
        let Some(value) = violation.value.as_deref() else {
            continue;
        };
        if violation.sensitive || is_elided(value) || stated.contains(&violation.path.as_str()) {
            continue;
        }
        let Some((_, expression)) = declared.iter().find(|(member, _)| *member == violation.path)
        else {
            continue;
        };
        if satisfies(expression, value) {
            continue;
        }
        stated.push(violation.path.as_str());
        restored.push(Violation {
            path: violation.path.clone(),
            kind: ViolationKind::Pattern((*expression).to_string()),
            value: Some(value.to_string()),
            sensitive: false,
        });
    }
    restored
}

/// Whether `value` satisfies `expression`, which is a FIND rather than a whole-value
/// match — the same reading the framework's own check applies.
///
/// An expression this engine cannot compile either, and a match that exhausts its
/// budget, are both treated as satisfied: a value is never refused because of an
/// expression nothing could evaluate, which is what the boundary does with a pattern
/// it cannot compile.
fn satisfies(expression: &str, value: &str) -> bool {
    ::fancy_regex::RegexBuilder::new(expression)
        .backtrack_limit(BACKTRACK_LIMIT)
        .build()
        .ok()
        .is_none_or(|compiled| compiled.is_match(value).unwrap_or(true))
}

/// Whether `echo` is a value the framework ELIDED rather than the value itself: it
/// closes with the `... [<n> bytes]` marker that walk appends, naming more bytes than
/// the text before the marker shows.
///
/// A longer value is stored on the violation already elided, so the text a clause
/// carries for one is a prefix of what the caller sent and satisfies — or breaks —
/// expressions the whole value does not.
fn is_elided(echo: &str) -> bool {
    let Some(digits) = echo.strip_suffix(" bytes]") else {
        return false;
    };
    let Some(at) = digits.rfind("... [") else {
        return false;
    };
    digits[at + "... [".len()..]
        .parse::<usize>()
        .is_ok_and(|named| named > at)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an expression is applied as a FIND over the value, with look-around
    /// honoured. Driven over the model's OWN `CustomID` expression — the one the
    /// framework's engine cannot compile — plus one it can, so the rows cover both.
    /// Source: `smithy_query(query="shape", shape="CustomID")` and
    /// `smithy_query(query="shape", shape="AssetModelVersionFilter")`.
    #[test]
    fn a_look_around_expression_is_applied_to_the_value() {
        const VERSION_FILTER: &str = "^(LATEST|ACTIVE)$";
        let rows: [(&str, &str, &str, bool); 8] = [
            (
                "a non-nil UUID",
                crate::state::CUSTOM_ID_PATTERN,
                "9b9b9b9b-1111-2222-3333-444444444444",
                true,
            ),
            (
                "the nil UUID the lookahead excludes",
                crate::state::CUSTOM_ID_PATTERN,
                "00000000-0000-0000-0000-000000000000",
                false,
            ),
            (
                "an externalId: alias",
                crate::state::CUSTOM_ID_PATTERN,
                "externalId:turbine_model",
                true,
            ),
            (
                "an externalId: alias closing with a separator",
                crate::state::CUSTOM_ID_PATTERN,
                "externalId:turbine_model.",
                false,
            ),
            (
                "a resource name",
                crate::state::CUSTOM_ID_PATTERN,
                "WindTurbineModel",
                false,
            ),
            ("a declared version", VERSION_FILTER, "ACTIVE", true),
            ("the other declared version", VERSION_FILTER, "LATEST", true),
            ("a version in the wrong case", VERSION_FILTER, "active", false),
        ];
        for (case, expression, value, expected) in rows {
            let actual = satisfies(expression, value);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected satisfies({value:?}) == {expected}, got {actual}"
            );
        }
    }

    /// RULE: an echo carrying the framework's elision marker is not the value the
    /// caller sent, so no expression is applied to it; anything else is.
    #[test]
    fn only_the_frameworks_own_elision_marker_marks_an_elided_echo() {
        let rows: [(&str, String, bool); 5] = [
            ("a plain value", "WindTurbineModel".to_string(), false),
            (
                "an elided value",
                format!("{}... [300 bytes]", "a".repeat(16)),
                true,
            ),
            (
                "a value ending in the marker's words but naming fewer bytes than it shows",
                format!("{}... [3 bytes]", "a".repeat(16)),
                false,
            ),
            (
                "a value ending in the marker's punctuation with no count",
                "a value... [many bytes]".to_string(),
                false,
            ),
            ("the empty value", String::new(), false),
        ];
        for (case, echo, expected) in rows {
            let actual = is_elided(&echo);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected is_elided({echo:?}) == {expected}, got {actual}"
            );
        }
    }

    /// RULE: a member whose refusal already states its expression gains no second
    /// clause for it, and a member the input shape declares no expression for gains
    /// none at all.
    #[test]
    fn one_member_states_its_expression_once() {
        let declared = vec![("hierarchyId".to_string(), crate::state::CUSTOM_ID_PATTERN)];
        let length = Violation {
            path: "hierarchyId".to_string(),
            kind: ViolationKind::MinLength(13),
            value: Some("Turbines".to_string()),
            sensitive: false,
        };
        let pattern = Violation {
            kind: ViolationKind::Pattern(crate::state::CUSTOM_ID_PATTERN.to_string()),
            ..length.clone()
        };
        let other = Violation {
            path: "maxResults".to_string(),
            kind: ViolationKind::MinValue("1".to_string()),
            value: Some("0".to_string()),
            sensitive: false,
        };
        let rows: [(&str, Vec<Violation>, usize); 4] = [
            (
                "a member outside its expression and its bound",
                vec![length.clone()],
                2,
            ),
            (
                "a member whose expression is stated already",
                vec![length.clone(), pattern],
                2,
            ),
            (
                "a member the shape declares no expression for",
                vec![other.clone()],
                1,
            ),
            (
                "a reference inside its expression, refused for something else",
                vec![Violation {
                    value: Some("9b9b9b9b-1111-2222-3333-444444444444".to_string()),
                    ..length
                }],
                1,
            ),
        ];
        for (case, violations, expected) in rows {
            let actual = applied(&violations, &declared).len();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected} violation(s), got {actual}"
            );
        }
    }
}
