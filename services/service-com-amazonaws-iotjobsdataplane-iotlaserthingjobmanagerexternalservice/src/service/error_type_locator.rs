// SPDX-License-Identifier: CC0-1.0

//! WHICH error shape a request this plane refuses at its BOUNDARY — before any handler ran
//! — is named under, in the two places a rendered JSON failure names one: the
//! `x-amzn-errortype` header a restJson1 client resolves the shape from, and the body's own
//! `__type` discriminator.
//!
//! RULE: a constraint refusal names the client-input error shape the OPERATION declares.
//! The framework's boundary renderer names one generic shape ([`BOUNDARY_SHAPE`]) for every
//! service it renders a refusal for, which an SDK generated from THIS model can resolve only
//! for the operations whose errors include it; for every other operation it is a shape the
//! client has never heard of, so the refusal arrives as an unmodelled failure instead of as
//! the 400 the operation documents. The wire names the one the operation models.
//!
//! Which shape that is per operation is the model's fact
//! (`smithy_query(query="digest")` lists each operation's errors):
//!
//! | operation | client-input error |
//! |---|---|
//! | `DescribeJobExecution`, `GetPendingJobExecutions`, `StartNextPendingJobExecution`, `UpdateJobExecution` | [`INVALID_REQUEST_EXCEPTION`] (`@httpError 400`, "The contents of the request were invalid.") |
//! | `StartCommandExecution` | [`BOUNDARY_SHAPE`] itself (`@httpError 400`), so nothing is restated |
//!
//! Both shapes declare the same single `message` member and the same 400, so only the NAME
//! moves: the status, the counted sentence the boundary rendered and every other member stay
//! exactly as the renderer wrote them.
//!
//! The restatement itself — both places in one step, with every other member and byte of the
//! body left alone — is
//! [`service_common::error_type_locator::with_restated_error_shape`]; this module supplies
//! only which name replaces which, which is this plane's own fact. WHERE it is applied is
//! [`super::refusal_shape`]: the seam that words a refusal is handed the violations alone and
//! never the operation they came from, so the shape is restated on the way out of the
//! operation, where the model's answer to "which error does this one declare" is known.
//!
//! What is deliberately LEFT ALONE: every response naming any OTHER shape — a success byte
//! for byte, and a failure a handler decided (a refused member, an execution that does not
//! exist, a transition the execution's state refuses) under the name its serializer wrote.

use ::aws_smithy_runtime_api::http::Response as RtResponse;
use ::service_common::error_type_locator::with_restated_error_shape;

/// The shape the framework's boundary renderer names a constraint refusal with, for every
/// service it renders one for. Also the shape `StartCommandExecution` itself declares.
pub(crate) const BOUNDARY_SHAPE: &str = "ValidationException";

/// The client-input error the four device-facing operations declare: "The contents of the
/// request were invalid." (`@httpError 400`, one `message` member — the same status and the
/// same member the boundary renderer already used).
pub(crate) const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";

/// `resp` as this plane answers it: a boundary refusal that named [`BOUNDARY_SHAPE`] names
/// `modelled` instead, in its header and its body alike.
///
/// A response naming any other shape — every success, and every failure a handler decided —
/// is returned exactly as its renderer wrote it, and an operation whose own modelled shape IS
/// the boundary shape is left alone too, which is what makes this safe to apply to every
/// operation.
#[must_use]
pub(crate) fn answered(resp: RtResponse, modelled: &str) -> RtResponse {
    if modelled == BOUNDARY_SHAPE {
        return resp;
    }
    with_restated_error_shape(resp, BOUNDARY_SHAPE, modelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::aws_smithy_runtime_api::http::StatusCode;
    use ::aws_smithy_types::body::SdkBody;
    use ::service_common::error_type_locator::ERROR_TYPE_HEADER;

    /// One rendered response: a status, and the shape its renderer named in the header and in
    /// the body the way the JSON renderers write it.
    fn response(status: u16, shape: &str) -> RtResponse {
        let mut resp = RtResponse::new(
            StatusCode::try_from(status).expect("a valid status"),
            SdkBody::from(format!(
                r#"{{"__type":"{shape}","message":"1 validation error detected: …"}}"#
            )),
        );
        resp.headers_mut()
            .insert(ERROR_TYPE_HEADER, shape.to_string());
        resp
    }

    /// The shape a response names in its body.
    fn body_shape(resp: &RtResponse) -> String {
        let document: ::serde_json::Value =
            ::serde_json::from_slice(resp.body().bytes().expect("an in-memory body"))
                .expect("the error body is JSON");
        document["__type"].as_str().unwrap_or_default().to_string()
    }

    /// The sentence a response states.
    fn message(resp: &RtResponse) -> String {
        let document: ::serde_json::Value =
            ::serde_json::from_slice(resp.body().bytes().expect("an in-memory body"))
                .expect("the error body is JSON");
        document["message"].as_str().unwrap_or_default().to_string()
    }

    /// A boundary refusal is restated under the operation's own shape in BOTH places,
    /// however the renderer spelled the generic one — and the sentence and status survive.
    #[test]
    fn a_boundary_refusal_names_the_shape_the_operation_models() {
        for (case, rendered) in [
            ("the bare shape", BOUNDARY_SHAPE.to_string()),
            (
                "a namespaced shape",
                format!("com.amazonaws.iotjobsdataplane#{BOUNDARY_SHAPE}"),
            ),
        ] {
            let answer = answered(response(400, &rendered), INVALID_REQUEST_EXCEPTION);
            assert_eq!(
                answer.headers().get(ERROR_TYPE_HEADER),
                Some(INVALID_REQUEST_EXCEPTION),
                "case {case:?}: expected the header {INVALID_REQUEST_EXCEPTION:?}, got {:?}",
                answer.headers().get(ERROR_TYPE_HEADER)
            );
            assert_eq!(
                body_shape(&answer),
                INVALID_REQUEST_EXCEPTION,
                "case {case:?}: expected the body to name {INVALID_REQUEST_EXCEPTION:?}"
            );
            assert_eq!(
                message(&answer),
                "1 validation error detected: …",
                "case {case:?}: the sentence must survive the restatement"
            );
            assert_eq!(
                answer.status().as_u16(),
                400,
                "case {case:?}: the status must not move"
            );
        }
    }

    /// Nothing to restate: a success, a failure a handler decided, and an operation whose own
    /// modelled shape IS the boundary shape are each answered exactly as they arrived.
    #[test]
    fn every_other_response_is_answered_as_the_renderer_wrote_it() {
        for (case, status, shape, modelled) in [
            ("a success", 200, BOUNDARY_SHAPE, INVALID_REQUEST_EXCEPTION),
            (
                "a refused member",
                400,
                INVALID_REQUEST_EXCEPTION,
                INVALID_REQUEST_EXCEPTION,
            ),
            (
                "an unknown execution",
                404,
                "ResourceNotFoundException",
                INVALID_REQUEST_EXCEPTION,
            ),
            (
                "a refused transition",
                409,
                "InvalidStateTransitionException",
                INVALID_REQUEST_EXCEPTION,
            ),
            (
                "an operation modelling the boundary shape",
                400,
                BOUNDARY_SHAPE,
                BOUNDARY_SHAPE,
            ),
        ] {
            let arrived = response(status, shape);
            let body = arrived.body().bytes().expect("an in-memory body").to_vec();
            let answer = answered(arrived, modelled);
            assert_eq!(
                answer.headers().get(ERROR_TYPE_HEADER),
                Some(shape),
                "case {case:?}: expected the header left at {shape:?}, got {:?}",
                answer.headers().get(ERROR_TYPE_HEADER)
            );
            assert_eq!(
                answer.body().bytes().expect("an in-memory body"),
                body.as_slice(),
                "case {case:?}: expected the body left as the renderer wrote it"
            );
        }
    }
}
