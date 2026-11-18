// SPDX-License-Identifier: CC0-1.0

//! Which member of an error body this service reports a failure's text in: `Message`.
//!
//! # The rule
//!
//! Every error shape this model declares carries its text in a member spelled `Message`, and
//! none of them gives it a `@jsonName`
//! (`smithy_query(query="shape",shape="com.amazonaws.transfer#AccessDeniedException,com.amazonaws.transfer#ConflictException,com.amazonaws.transfer#InternalServiceError,com.amazonaws.transfer#InvalidNextTokenException,com.amazonaws.transfer#InvalidRequestException,com.amazonaws.transfer#ResourceExistsException,com.amazonaws.transfer#ResourceNotFoundException,com.amazonaws.transfer#ServiceUnavailableException,com.amazonaws.transfer#ThrottlingException")`),
//! and an `awsJson1_1` member with no `@jsonName` goes on the wire under its Smithy name. So
//! `Message` is where one of this service's own errors carries its text, whatever produced
//! it:
//!
//! ```json
//! HTTP/1.1 400 Bad Request
//! content-type: application/x-amz-json-1.1
//!
//! {"__type":"InvalidRequestException","Message":"Invalid LoggingRole ARN"}
//! ```
//!
//! An error a HANDLER returns is serialized from the model's own shape, so it already carries
//! that member. The body the FRAMEWORK renders for a request it refused before any handler
//! ran does not: that body is built from a synthetic shape whose own member is spelled
//! `message`, and the JSON error renderer writes that spelling even when the service names
//! one of its MODELLED error shapes as the code — which [`super`]'s refusal wording does for
//! a value outside an ARN-shaped `@pattern`. A client, or an SDK deserializing the modelled
//! exception the code names, then finds no `Message` member at all and surfaces the refusal
//! with no text though its status and its code are right, and the body is not a valid
//! rendering of a shape whose `Message` the model REQUIRES.
//!
//! This layer therefore reports that text in `Message` on every error body this service
//! answers whose `__type` names one of those shapes, wherever it was produced. The renaming
//! itself is `service-common`'s, so this file states only the two member names and the shapes
//! the rule covers.
//!
//! What is deliberately left alone: the status, the `__type` discriminator, every other
//! member and the whole success path; a body that already carries `Message` — every error a
//! handler serialized — is returned untouched; and a failure whose class this model does NOT
//! declare keeps the member the framework wrote its text in. That last part is what this
//! service's constraint refusals depend on: they are answered under `ValidationException`,
//! which this model does not declare
//! (`smithy_query(query="shape",shape="com.amazonaws.transfer#ValidationException")` finds no
//! such shape), and their text stays in the `message` member the framework writes, which is
//! where the recorded refusals of this service carry it.
//!
//! The structural home for the rule is the framework's error renderer, which writes the
//! synthetic shape's `message` for every service; that renderer is in the agent-immutable
//! `vendor/` tree, so this service states it at the one seam that can see a whole rendered
//! response — an operation wrapper handed back from `Service::override_operation`, whose
//! plumbing is `service_common::restated_refusal`'s. A failure answered before any operation
//! is served (a request naming no modelled operation, a transport failure, a pre-dispatch
//! authentication refusal) is therefore not reached from here.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use service_common::restated_refusal::RestatedRefusalOperation;

/// The body member this service's own errors carry their text in — the member name those
/// error shapes declare; see the module header for the `smithy_query` call it comes from.
const MODELLED_MESSAGE_MEMBER: &str = "Message";

/// The body member the FRAMEWORK's own refusal renderer writes that text in: the member of
/// the synthetic shape it builds a pre-handler refusal from.
const RENDERED_MESSAGE_MEMBER: &str = "message";

/// Every error shape this model declares that carries a text member, by the wire code
/// (`__type`) it renders under — the same `smithy_query` call the module header cites, which
/// is where this list comes from and the only thing that may extend it.
///
/// The rename is keyed on THIS set rather than on the status, so a failure whose class this
/// service does not model — the scaffold's own 501, the `ValidationException` a constraint
/// refusal is answered under — keeps the member the framework wrote its text in.
/// `ThrottlingException` is absent because it declares no text member at all (its only member
/// is `RetryAfterSeconds`), so a body naming it has no text to move.
const MODELLED_ERROR_SHAPES: [&str; 8] = [
    "AccessDeniedException",
    "ConflictException",
    "InternalServiceError",
    "InvalidNextTokenException",
    "InvalidRequestException",
    "ResourceExistsException",
    "ResourceNotFoundException",
    "ServiceUnavailableException",
];

/// `resp` as this service answers it: a modelled error body whose text is in its `Message`
/// member, through the one renaming `service-common` states for every service whose wire
/// member differs from the one a layer wrote.
#[must_use]
pub(crate) fn with_modelled_message_member(resp: RtResponse) -> RtResponse {
    ::service_common::error_detail_member::with_renamed_detail_member(
        resp,
        &MODELLED_ERROR_SHAPES,
        RENDERED_MESSAGE_MEMBER,
        MODELLED_MESSAGE_MEMBER,
    )
}

/// `resp` as this service answers it, for a request the wrapper below served.
///
/// The words the refusal seam chose are already in the body the framework rendered, so
/// nothing is noted and nothing is restated here — only the member the text travels in is
/// this service's to state.
fn with_this_services_envelope(
    _req: &RtRequest,
    resp: RtResponse,
    _noted: Option<String>,
) -> RtResponse {
    with_modelled_message_member(resp)
}

/// The layer every modelled operation of this service is served through: the fleet's wrapper,
/// which serves the operation the generated routing table names — so the framework keeps the
/// whole deserialize → validate → handle → serialize walk and every status, code and word it
/// answers with — and then answers what that walk rendered through
/// [`with_this_services_envelope`].
///
/// A `static` so the reference `Service::override_operation` hands back outlives the call,
/// and one value for the whole process because the wrapper holds nothing per service but that
/// one function.
pub(crate) static SERVED_OPERATION_LAYER: RestatedRefusalOperation =
    RestatedRefusalOperation::new(with_this_services_envelope);

#[cfg(test)]
mod tests {
    use aws_smithy_types::body::SdkBody;
    use serde_json::Value;

    use super::*;

    /// A response at `status` carrying `body`.
    fn response(status: u16, body: &str) -> RtResponse {
        RtResponse::new(
            aws_smithy_runtime_api::http::StatusCode::try_from(status).expect("a valid status"),
            SdkBody::from(body),
        )
    }

    /// The body of `resp` as JSON.
    fn decoded(resp: &RtResponse) -> Value {
        serde_json::from_slice(resp.body().bytes().expect("an in-memory body"))
            .expect("the body is JSON")
    }

    /// RULE: one row per error shape this model declares a text member on — a refusal the
    /// framework rendered under each of the codes this service can name — so the member is a
    /// property of the error envelope rather than of one recorded body.
    #[test]
    fn every_modelled_error_body_reports_its_text_in_the_message_member() {
        for code in MODELLED_ERROR_SHAPES {
            let text = format!("{code} states its own sentence");
            let body = format!(r#"{{"__type":"{code}","message":"{text}"}}"#);
            let answered = with_modelled_message_member(response(400, &body));
            let document = decoded(&answered);
            assert_eq!(
                document.get(MODELLED_MESSAGE_MEMBER).and_then(Value::as_str),
                Some(text.as_str()),
                "case {code:?}: expected the text in {MODELLED_MESSAGE_MEMBER}, got {document}"
            );
            assert!(
                document.get(RENDERED_MESSAGE_MEMBER).is_none(),
                "case {code:?}: the text must be carried under ONE member, got {document}"
            );
            assert_eq!(
                document.get("__type").and_then(Value::as_str),
                Some(code),
                "case {code:?}: the discriminator must survive the rename, got {document}"
            );
        }
    }

    /// RULE: a body this rule does not own is answered byte for byte as its renderer wrote
    /// it — a success, the constraint refusal whose class this model does not declare, the
    /// scaffold's 501, the one modelled error carrying no text member, and a body that
    /// already carries the modelled member.
    #[test]
    fn a_body_this_rule_does_not_own_is_answered_unchanged() {
        // (case, status, the body)
        let rows: &[(&str, u16, &str)] = &[
            (
                "a success whose own members are not an error's",
                200,
                r#"{"ServerId":"s-0123456789abcdef0"}"#,
            ),
            (
                "the constraint refusal, a class this model does not declare",
                400,
                r#"{"__type":"ValidationException","message":"1 validation error detected"}"#,
            ),
            (
                "the scaffold's 501, another class this model does not declare",
                501,
                r#"{"__type":"NotImplemented","message":"not implemented in this emulator"}"#,
            ),
            (
                "the modelled error that declares no text member",
                429,
                r#"{"__type":"ThrottlingException","message":"slow down"}"#,
            ),
            (
                "a modelled error a handler already serialized",
                400,
                r#"{"__type":"InvalidRequestException","Message":"Invalid LoggingRole ARN"}"#,
            ),
        ];

        for (case, status, body) in rows {
            let answered = with_modelled_message_member(response(*status, body));
            assert_eq!(
                answered.body().bytes().expect("an in-memory body"),
                body.as_bytes(),
                "case {case:?}: expected the body unchanged"
            );
        }
    }
}
