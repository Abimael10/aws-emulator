// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a `CreateParticipantConnection` that asks for no connection
//! type.
//!
//! `Type` targets `ConnectionTypeList`, whose `@length` minimum is 1, so an empty list is
//! refused at the request boundary before the handler runs. The framework's own report of
//! that bound states the member path, the submitted size and the bound itself; the service
//! states a single sentence about the missing connection type instead, with nothing of the
//! request in it, so a client can branch on the refusal it actually sends.
//!
//! Only this member's own minimum is reworded. A violation of another member, or another
//! kind of violation of this one, keeps the framework's report: what this service says
//! about a missing connection type is not what it says about every value.

use ::smithy_server_runtime::{Violation, ViolationKind};

/// The modelled error a refused input is reported as: `ValidationException` — "The input
/// fails to satisfy the constraints specified by Amazon Connect".
pub const VALIDATION_EXCEPTION: &str = "ValidationException";

/// What the service answers a request that asks for no connection type: entirely fixed
/// text, with no request value interpolated.
pub const MISSING_CONNECTION_TYPE: &str =
    "Failed to create participant connection due to missing connection type";

/// The input member whose empty-list refusal this service words itself, as the restJson1
/// boundary names it. `CreateParticipantConnection` is the one operation of this service
/// that takes a `ConnectionTypeList`, so the member path alone identifies the refusal.
const TYPE_MEMBER: &str = "type";

/// Whether `violations` say the request asked for no connection type at all — the `Type`
/// list's own `@length` minimum, which is the refusal [`MISSING_CONNECTION_TYPE`] words.
#[must_use]
pub fn missing_connection_type(violations: &[Violation]) -> bool {
    violations.iter().any(|violation| {
        violation.path.eq_ignore_ascii_case(TYPE_MEMBER)
            && matches!(violation.kind, ViolationKind::MinLength(_))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violation(path: &str, kind: ViolationKind) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: Some("0 items".to_string()),
            sensitive: false,
        }
    }

    #[test]
    fn only_an_empty_connection_type_list_is_worded_by_this_service() {
        // The rows span the refusal this service words, the same member failing another
        // way, and the other members a request carries.
        let cases: Vec<(&str, Vec<Violation>, bool)> = vec![
            (
                "an empty Type list",
                vec![violation("type", ViolationKind::MinLength(1))],
                true,
            ),
            (
                "an empty Type list beside another member's violation",
                vec![
                    violation("participantToken", ViolationKind::MinLength(1)),
                    violation("type", ViolationKind::MinLength(1)),
                ],
                true,
            ),
            (
                "an absent Type, which asks for no connection information rather than none",
                vec![violation("type", ViolationKind::MissingRequiredMember)],
                false,
            ),
            (
                "a Type element outside the enum",
                vec![violation(
                    "type.1.member",
                    ViolationKind::EnumValue(vec!["WEBSOCKET".to_string()]),
                )],
                false,
            ),
            (
                "another member alone",
                vec![violation("participantToken", ViolationKind::MinLength(1))],
                false,
            ),
            ("no violations", Vec::new(), false),
        ];
        for (case, violations, expected) in cases {
            let worded = missing_connection_type(&violations);
            assert_eq!(
                worded, expected,
                "case {case:?}: expected {expected}, got {worded}"
            );
        }
    }

    #[test]
    fn the_sentence_carries_nothing_of_the_request() {
        // What the framework's own report of the bound would state: the member path as the
        // boundary spells it, the submitted size, and the declared bound.
        for fragment in ["'type'", "0 items", "validation error", "length"] {
            assert!(
                !MISSING_CONNECTION_TYPE.contains(fragment),
                "case {fragment:?}: the sentence must not carry it, got \
                 {MISSING_CONNECTION_TYPE:?}"
            );
        }
    }
}
