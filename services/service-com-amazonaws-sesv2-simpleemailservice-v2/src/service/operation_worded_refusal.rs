// SPDX-License-Identifier: CC0-1.0

//! The input refusal this service words per OPERATION, stated ahead of the framework's own
//! constraint walk.
//!
//! The walk at the request boundary reports a value outside a modelled constraint
//! STRUCTURALLY — the member's path and the rule it broke — and this service words that
//! report ONCE for every operation (`crate::state::boundary_refusal_message`, stated on the
//! `Service::override_refusal` seam). One operation does not word it that way:
//! `PutConfigurationSetSuppressionOptions` answers a suppression reason outside
//! `SuppressionListReason`'s value set with a sentence about what the member MEANS
//! (`crate::state::invalid_suppression_reason`), while every other operation carrying the
//! same member — `PutAccountSuppressionAttributes`, `PutTenantSuppressionAttributes`,
//! `CreateConfigurationSet` — answers the constraint report.
//!
//! A refusal that depends on WHICH operation carried the member cannot be stated on the
//! refusal seam, which is handed the violations without the operation, and it cannot be
//! stated by the handler either, because the walk refuses the request before any handler
//! runs. So it is stated here, in front of the walk, on the request the authorization
//! wrapper has already resolved an operation for.
//!
//! Only the sentence differs from what the walk would have answered: the status (400), the
//! modelled error code the walk's own refusal carries, the envelope it is rendered in, and
//! the fact that the request reaches no handler and changes nothing are all the boundary
//! refusal's own.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::ProtocolError;
use smithy_server_runtime::{Refused, Service};

/// The operation whose suppression-reason refusal is its own, as the generated routing
/// table names it.
const SUPPRESSION_OPTIONS_OPERATION: &str = "PutConfigurationSetSuppressionOptions";

/// The member that operation carries the reasons in, as the request spells it on the wire.
const SUPPRESSED_REASONS_MEMBER: &str = "SuppressedReasons";

/// The error code this service answers an input it cannot accept with — the modelled
/// `BadRequestException` ("the input you provided is invalid"), which the model binds to
/// every operation and which the boundary refusal names too. Restated because a chosen
/// refusal replaces the code and the message TOGETHER.
const BAD_REQUEST: &str = "BadRequestException";

/// How `operation` answers `req`, when the request carries a value that operation words the
/// refusal of itself; `None` for every other request, which the framework's own constraint
/// walk decides.
pub(super) fn refusal(svc: &dyn Service, req: &RtRequest, operation: &str) -> Option<RtResponse> {
    if operation != SUPPRESSION_OPTIONS_OPERATION {
        return None;
    }
    let reason = unmodelled_suppression_reason(req.body().bytes()?)?;
    let stated = Refused {
        code: BAD_REQUEST.to_string(),
        message: crate::state::invalid_suppression_reason(&reason),
    };
    // Rendered through the refusal channel the framework renders its own boundary refusal
    // in, so the status and the envelope stay the protocol's and only the sentence is this
    // operation's.
    Some(
        svc.definition()
            .refusal_response(ProtocolError::Invalid(Vec::new()), Some(stated), req),
    )
}

/// The first value of the request's `SuppressedReasons` that is outside
/// `SuppressionListReason`'s modelled value set, or `None` when every value it carries is
/// one of them.
///
/// Read off the body AS SUBMITTED, because the refusal states the rejected value verbatim
/// and the deserialized input never reaches a handler. A body that is not a JSON object, a
/// member that is not a list and an element that is not a string are left alone: the
/// deserializer and the constraint walk answer those, in their own words.
fn unmodelled_suppression_reason(body: &[u8]) -> Option<String> {
    let document: ::serde_json::Value = ::serde_json::from_slice(body).ok()?;
    document
        .get(SUPPRESSED_REASONS_MEMBER)?
        .as_array()?
        .iter()
        .filter_map(::serde_json::Value::as_str)
        .find(|value| !crate::ops::types::SUPPRESSION_LIST_REASON_VALUES.contains(value))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a value outside the modelled set is picked out, and the FIRST such value is the
    /// one the refusal states.
    #[test]
    fn the_first_reason_outside_the_modelled_set_is_the_one_reported() {
        for (case, body, expected) in [
            ("every reason modelled", r#"{"SuppressedReasons":["BOUNCE","COMPLAINT"]}"#, None),
            ("no reasons at all", r#"{"SuppressionScope":"ACCOUNT"}"#, None),
            ("the empty list", r#"{"SuppressedReasons":[]}"#, None),
            (
                "one unmodelled reason",
                r#"{"SuppressedReasons":["INVALID"]}"#,
                Some("INVALID"),
            ),
            (
                "a modelled reason first",
                r#"{"SuppressedReasons":["BOUNCE","SPAM"]}"#,
                Some("SPAM"),
            ),
            (
                "two unmodelled reasons",
                r#"{"SuppressedReasons":["bounce","SPAM"]}"#,
                Some("bounce"),
            ),
            (
                "a member that is not a list",
                r#"{"SuppressedReasons":"BOUNCE"}"#,
                None,
            ),
            ("no JSON at all", "not json", None),
        ] {
            let found = unmodelled_suppression_reason(body.as_bytes());
            assert_eq!(
                found.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {found:?}"
            );
        }
    }
}
