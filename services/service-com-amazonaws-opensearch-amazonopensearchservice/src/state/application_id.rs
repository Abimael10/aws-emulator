// SPDX-License-Identifier: CC0-1.0

//! The id an application of this account is ADDRESSED by: which values this service admits
//! as one, and how it refuses a value outside them.
//!
//! The surface is the SERVICE's rather than the model's. The model types the `id` member as
//! `Id` with a `@pattern` that admits lower-case alphanumerics alone, while the expression
//! this service's own refusals of that member quote admits a HYPHEN inside the id and
//! refuses a leading one (`crate::service::validation`'s table of stated requirements, which
//! records what each member whose surface the model lags is validated against). A service
//! that publishes a value as admissible resolves it: so a hyphenated id is a WELL-FORMED id
//! that names an application or names none, never a malformed request — which is why the
//! members carrying it are off the request boundary (`skip_validation` in `src/ops/types/`)
//! and every operation addressing an application by `id` applies this rule instead, before it
//! resolves the id against the store.
//!
//! Stated ONCE for the whole service, so the operations that address one application cannot
//! admit different sets of ids, and applied in each handler rather than ahead of the
//! boundary, so a request that also breaks a constraint the MODEL declares keeps that
//! member's clause (`crate::ops::attach_data_source` records that ordering).
//!
//! Two surfaces of the same resource's id are NOT this one, and neither moves off the
//! boundary:
//!
//! * the `applicationId` the CAPABILITY and migration-listing operations take: the service
//!   states its own requirements for that member — an expression quoted without anchors and a
//!   length bound of its own — and states them in ONE report together with the capability name
//!   the same request carries, which only the boundary's walk over both members can word;
//! * the `applicationId` `crate::ops::start_migration` is handed, which it validates against
//!   its own recorded expression beside its own rule, as a plain identifier rather than as the
//!   id of an application it resolves.

use crate::ops::types::ValidationException;

/// The expression this service publishes for an application id, as its refusals quote it:
/// three to thirty lower-case alphanumerics and hyphens, with no leading hyphen.
///
/// Declared here as the ONE text of that surface and read from there by
/// `crate::service::validation`'s table of stated requirements, so the expression a refusal
/// quotes and the class [`is_application_id`] admits are one statement.
pub const APPLICATION_ID_PATTERN: &str = r"^(?!-)[a-z0-9\-]{3,30}$";

/// The shortest id the surface admits.
const APPLICATION_ID_MIN: usize = 3;

/// The longest.
const APPLICATION_ID_MAX: usize = 30;

/// Whether `value` is a value this service admits as an application id: the class
/// [`APPLICATION_ID_PATTERN`] describes.
///
/// Read character by character rather than by running that expression: its leading lookahead
/// is not a construct the regular-expression engine behind this tree's `@pattern` checks
/// compiles, so the class is applied here and the expression stays the text a refusal quotes.
#[must_use]
pub fn is_application_id(value: &str) -> bool {
    let width = value.chars().count();
    (APPLICATION_ID_MIN..=APPLICATION_ID_MAX).contains(&width)
        && !value.starts_with('-')
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

/// The refusal for a value outside that surface at the input member `member`, or `None` for
/// one it admits.
///
/// The clauses, the member path and the envelope are the ones this service states for that
/// member ([`crate::service::validation::stated_surface_report`]), so a caller cannot tell
/// this refusal from the one the request boundary would have worded for the same value — only
/// which values earn it differs, which is the point.
pub fn application_id_refusal(member: &str, value: &str) -> Option<ValidationException> {
    if is_application_id(value) {
        return None;
    }
    Some(ValidationException {
        // Every member this rule holds for is one that table records a surface for, so the
        // clauses are that member's own; one it records nothing for is stated with the
        // expression declared here, which is the surface that was applied.
        message: Some(
            crate::service::validation::stated_surface_report(member, value).unwrap_or_else(
                || {
                    crate::service::validation::published_pattern_report(
                        member,
                        value,
                        APPLICATION_ID_PATTERN,
                    )
                },
            ),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the admitted class is three to thirty lower-case alphanumerics and hyphens,
    /// with no leading hyphen — at each published width bound and one past each of them.
    #[test]
    fn the_admitted_class_is_the_published_one() {
        for (case, value, admitted) in [
            ("the shortest admitted id", "abc", true),
            ("one character shorter", "ab", false),
            ("the longest admitted id", "a12345678901234567890123456789", true),
            ("one character longer", "a123456789012345678901234567890", false),
            ("a hyphen inside the id", "sim2real-noapp-50855", true),
            ("hyphens only after the first character", "a--------b", true),
            ("a leading hyphen", "-noapp", false),
            ("a trailing hyphen", "noapp-", true),
            ("an upper-case letter", "noApp", false),
            ("an underscore", "no_app", false),
            ("a dot", "no.app", false),
            ("a space", "no app", false),
            ("the empty value", "", false),
        ] {
            assert_eq!(
                is_application_id(value),
                admitted,
                "case {case}: expected is_application_id({value:?}) == {admitted}"
            );
        }
    }

    /// RULE: the class [`is_application_id`] applies IS the class
    /// [`APPLICATION_ID_PATTERN`] describes — the two statements of one surface agree on every
    /// value, so the expression a refusal quotes is the rule a value was held to.
    ///
    /// Asserted over a generated set rather than a list: every string of up to four characters
    /// over an alphabet carrying one admitted letter, one admitted digit, the hyphen and two
    /// characters the surface excludes, plus the values at and either side of both published
    /// width bounds. The expression is run by the look-around-capable engine this crate already
    /// compiles its other lookahead-bearing `@pattern` with, which is what makes the comparison
    /// possible at all.
    #[test]
    fn the_class_applied_is_the_class_the_published_expression_describes() {
        let published = ::fancy_regex::Regex::new(APPLICATION_ID_PATTERN)
            .expect("the published expression compiles");
        let alphabet = ['a', '7', '-', '_', 'Z'];
        let mut cases: Vec<String> = vec![String::new()];
        for _ in 0..4 {
            let mut grown = Vec::new();
            for held in &cases {
                for ch in alphabet {
                    let mut candidate = held.clone();
                    candidate.push(ch);
                    grown.push(candidate);
                }
            }
            cases.extend(grown);
        }
        for width in [2, 3, 4, 29, 30, 31] {
            cases.push("a".repeat(width));
            cases.push(format!("-{}", "a".repeat(width - 1)));
            cases.push(format!("a{}", "-".repeat(width - 1)));
        }

        for case in &cases {
            let matched = published
                .is_match(case)
                .expect("the published expression runs within its own budget");
            assert_eq!(
                is_application_id(case),
                matched,
                "case {case:?}: the class applied and the published expression disagree"
            );
        }
    }

    /// RULE: a value the surface admits earns no refusal, and one outside it is refused
    /// stating the member it stood at and the expression the service publishes.
    #[test]
    fn a_refusal_states_the_member_and_the_published_expression() {
        assert!(
            application_id_refusal("id", "sim2real-noapp-50855").is_none(),
            "case an admitted id: expected no refusal"
        );
        let refused = application_id_refusal("id", "no_app")
            .expect("case a value outside the surface: expected a refusal");
        let message = refused.message.unwrap_or_default();
        assert!(
            message.contains("'no_app' at 'id'") && message.contains(APPLICATION_ID_PATTERN),
            "case a value outside the surface: expected the member and the published \
             expression, got {message:?}"
        );
    }
}
