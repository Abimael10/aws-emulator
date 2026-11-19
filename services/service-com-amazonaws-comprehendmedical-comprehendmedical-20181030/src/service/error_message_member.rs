// SPDX-License-Identifier: CC0-1.0

//! Which member of an error body this service reports a failure's sentence in: `Message`.
//!
//! # The rule
//!
//! Every error shape this model declares carries its sentence in a member spelled
//! `Message`, and none of them gives that member a `@jsonName`
//! (`smithy_query(query="shape",shape="InternalServerException,InvalidRequestException,ResourceNotFoundException,TooManyRequestsException,ValidationException,InvalidEncodingException,ServiceUnavailableException,TextSizeLimitExceededException")`).
//! An `awsJson1_1` member with no `@jsonName` goes on the wire under its Smithy name, so
//! `Message` is where one of this service's own errors reports its sentence, whichever
//! layer produced it:
//!
//! ```json
//! HTTP/1.1 400 Bad Request
//! content-type: application/x-amz-json-1.1
//!
//! {"__type":"ResourceNotFoundException","Message":"Could not find specified job."}
//! ```
//!
//! An error a HANDLER returns is serialized from the model's own shape, so it already
//! carries that member. The body the FRAMEWORK renders for a request it refused before any
//! handler ran does not: that body comes from a synthetic shape whose member is spelled
//! `message`, and the JSON error renderer writes that spelling even when the service names
//! one of its MODELLED error shapes as the code — which [`super::validation`] does for
//! every value outside a modelled constraint. A client, or an SDK deserializing the
//! exception the code names, then finds no `Message` member at all: the status and the code
//! are right and the request carries no reason, which is exactly the information a
//! constraint report exists to deliver.
//!
//! This layer therefore reports that sentence in `Message` on every error body this service
//! answers under one of those codes, wherever it was produced. The renaming itself is
//! `service-common`'s, so this file states only the two member names and the shapes the
//! rule covers.
//!
//! What is deliberately left alone: the status, the `__type` discriminator, every other
//! member and the whole success path; a body that already carries `Message` — every error a
//! handler serialized — is answered byte for byte as its renderer wrote it; and a failure
//! whose class this model does NOT declare (the scaffold's own 501) keeps the member the
//! framework wrote its sentence in. A body that is not JSON at all — this model also
//! declares `rpcv2Cbor` — is left alone for the same reason.
//!
//! The structural home for the rule is the framework's error renderer, which writes the
//! synthetic shape's `message` for every service; that renderer is in the agent-immutable
//! `vendor/` tree, so this service states it at the one seam that can see a whole rendered
//! response — an operation wrapper handed back from `Service::override_operation`, whose
//! plumbing is `service_common::restated_refusal`'s. A failure answered before any
//! operation is served (a request naming no modelled operation, a transport failure, an
//! authentication refusal) is not reached from here.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use service_common::restated_refusal::RestatedRefusalOperation;

/// The body member this service's own errors carry their sentence in — the member name
/// every one of those error shapes declares; see the module header for the `smithy_query`
/// call it comes from.
const MODELLED_MESSAGE_MEMBER: &str = "Message";

/// The body member the FRAMEWORK's own refusal renderer writes that sentence in: the member
/// of the synthetic shape it builds a pre-handler refusal from.
const RENDERED_MESSAGE_MEMBER: &str = "message";

/// Every error shape this model declares, by the wire code (`__type`) it renders under —
/// the same `smithy_query` call the module header cites, which is where this list comes
/// from and the only thing that may extend it. Each of them declares the `Message` member
/// this rule moves the sentence into.
///
/// The rename is keyed on THIS set rather than on the status, so a failure whose class this
/// service does not model — the scaffold's own 501 — keeps the member the framework wrote
/// its sentence in.
const MODELLED_ERROR_SHAPES: [&str; 8] = [
    "InternalServerException",
    "InvalidEncodingException",
    "InvalidRequestException",
    "ResourceNotFoundException",
    "ServiceUnavailableException",
    "TextSizeLimitExceededException",
    "TooManyRequestsException",
    "ValidationException",
];

/// `resp` as this service answers it: a modelled error body whose sentence is in its
/// `Message` member, through the one renaming `service-common` states for every service
/// whose wire member differs from the one a layer wrote.
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
/// The words of a refusal are already in the body the framework rendered —
/// [`super::validation`] chose them at the refusal seam — so nothing is noted and nothing
/// is restated here: only the member the sentence travels in is this layer's to state.
fn with_this_services_envelope(
    _req: &RtRequest,
    resp: RtResponse,
    _noted: Option<String>,
) -> RtResponse {
    with_modelled_message_member(resp)
}

/// The layer every modelled operation of this service is served through: the shared
/// wrapper, which serves the operation the generated routing table names — so the framework
/// keeps the whole deserialize → validate → handle → serialize walk and every status, code
/// and word it answers with — and then answers what that walk rendered through
/// [`with_this_services_envelope`].
///
/// A `static` so the reference `Service::override_operation` hands back outlives the call,
/// and one value for the whole process because the wrapper holds nothing per service but
/// that one function.
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

    /// RULE: one row per error shape this model declares — a refusal the framework rendered
    /// under each code this service can name — so the member is a property of the error
    /// envelope rather than of one recorded body.
    #[test]
    fn every_modelled_error_body_reports_its_sentence_in_the_message_member() {
        for code in MODELLED_ERROR_SHAPES {
            let sentence = format!("{code} states its own sentence");
            let body = format!(r#"{{"__type":"{code}","message":"{sentence}"}}"#);
            let answered = with_modelled_message_member(response(400, &body));
            let document = decoded(&answered);
            assert_eq!(
                document.get(MODELLED_MESSAGE_MEMBER).and_then(Value::as_str),
                Some(sentence.as_str()),
                "case {code:?}: expected the sentence in {MODELLED_MESSAGE_MEMBER}, got {document}"
            );
            assert!(
                document.get(RENDERED_MESSAGE_MEMBER).is_none(),
                "case {code:?}: the sentence must be carried under ONE member, got {document}"
            );
            assert_eq!(
                document.get("__type").and_then(Value::as_str),
                Some(code),
                "case {code:?}: the discriminator must survive the rename, got {document}"
            );
        }
    }

    /// RULE: a body this rule does not own is answered byte for byte as its renderer wrote
    /// it — a success, the scaffold's 501, a namespaced class this model does not declare, a
    /// body that already carries the modelled member, and a body that is not JSON at all.
    #[test]
    fn a_body_this_rule_does_not_own_is_answered_unchanged() {
        // (case, status, the body)
        let rows: &[(&str, u16, &str)] = &[
            (
                "a success whose own members are not an error's",
                200,
                r#"{"JobId":"0123456789abcdef0123456789abcdef"}"#,
            ),
            (
                "the scaffold's 501, a class this model does not declare",
                501,
                r#"{"__type":"NotImplemented","message":"not implemented in this emulator"}"#,
            ),
            (
                "another class this model does not declare",
                400,
                r#"{"__type":"SerializationException","message":"unreadable"}"#,
            ),
            (
                "a modelled error a handler already serialized",
                400,
                r#"{"__type":"ResourceNotFoundException","Message":"Could not find specified job."}"#,
            ),
            (
                "a body that is not a JSON object at all",
                400,
                "not json",
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
