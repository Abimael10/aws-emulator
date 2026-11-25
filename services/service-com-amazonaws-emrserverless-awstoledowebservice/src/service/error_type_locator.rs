// SPDX-License-Identifier: CC0-1.0

//! WHICH error SHAPE a refusal of this service names, in the two places a rendered restJson1
//! failure names one: the `x-amzn-errortype` response header a client resolves the shape from,
//! and the body's own `__type` discriminator.
//!
//! RULE: a refusal the TAGGING front door answers carries THAT stack's generic error identity
//! rather than a shape this service's model declares. Two of them are observable, one per
//! refusal class of that stack: a value that is no resource ARN of this vendor is its
//! bad-request shape (`super::WRONG_VENDOR_ERROR_TYPE`, raised before any handler runs, so the
//! refusal seam already names it), and an ARN that resolves to no resource is its not-found
//! shape ([`TAGGING_NOT_FOUND`]) — which the handler can only raise through the modelled
//! [`RESOURCE_NOT_FOUND`] its model declares, so that one has to be restated on the way out.
//!
//! The distinction is client-visible, because restJson1 resolves the error SHAPE from that
//! header: an SDK unmarshals a modelled `ResourceNotFoundException` into a typed error and this
//! identity into a generic service error, so a caller's `catch ResourceNotFoundException` fires
//! on a resource read and not on a tag call. RECORDED behaviour, not modelled: no operation of
//! this model declares either front-door shape, and the recorded answers of that path carry
//! them.
//!
//! Only the NAME moves. The status, the sentence the handler stated
//! ([`crate::state::TAGGED_RESOURCE_NOT_FOUND_MESSAGE`]) and every other member and byte of the
//! body stay exactly as the serializer wrote them, and both places are rewritten together, so a
//! client reading the header and one reading the body cannot disagree about what was refused.
//!
//! The restatement itself is [`service_common::error_type_locator::with_restated_error_shape`],
//! the fleet's one home for it; this module supplies only which name replaces which, which is
//! this service's own fact. WHERE it is applied is [`super::front_door_identity`]: the seam that
//! words a refusal is handed the violations alone and a handler cannot reach the rendering of
//! its own error, so the shape is restated on the way out of the operation, where the operation
//! that answered is known.

use aws_smithy_runtime_api::http::Response as WireResponse;
use service_common::error_type_locator::with_restated_error_shape;

/// The error-type identity the tagging front door answers a resource ARN that names no resource
/// with: its generic not-found shape, the counterpart of the bad-request shape the same stack
/// answers an unusable ARN with.
pub(crate) const TAGGING_NOT_FOUND: &str = "NotFoundException";

/// The shape such a refusal is RENDERED under: the only 404 shape this model declares for the
/// tagging operations (`smithy_query(query="operation",
/// operation="TagResource,UntagResource,ListTagsForResource")` lists `ResourceNotFoundException`
/// with `httpError` 404 on all three), and so the name the serializer wrote and the one the
/// front door's identity replaces.
pub(crate) const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// `resp` as the tagging front door answers it: a not-found refusal that was rendered under
/// [`RESOURCE_NOT_FOUND`] names [`TAGGING_NOT_FOUND`] instead, in its header and its body alike.
///
/// A response naming any other shape is returned exactly as its renderer wrote it — every
/// success byte for byte, and every refusal of another class (a value the member does not admit,
/// an ARN of another vendor) under the name its renderer chose.
#[must_use]
pub(crate) fn answered(resp: WireResponse) -> WireResponse {
    with_restated_error_shape(resp, RESOURCE_NOT_FOUND, TAGGING_NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_runtime_api::http::StatusCode;
    use aws_smithy_types::body::SdkBody;
    use service_common::error_type_locator::ERROR_TYPE_HEADER;

    /// One rendered response: a status, and the shape its serializer named in the header and in
    /// the body the way the JSON renderers write it.
    fn response(status: u16, shape: &str, message: &str) -> WireResponse {
        let mut resp = WireResponse::new(
            StatusCode::try_from(status).expect("a valid status"),
            SdkBody::from(format!(r#"{{"__type":"{shape}","message":"{message}"}}"#)),
        );
        resp.headers_mut()
            .insert(ERROR_TYPE_HEADER, shape.to_string());
        resp
    }

    /// The shape a response names in its body.
    fn body_shape(resp: &WireResponse) -> String {
        let document: serde_json::Value =
            serde_json::from_slice(resp.body().bytes().expect("an in-memory body"))
                .expect("the error body is JSON");
        document["__type"].as_str().unwrap_or_default().to_string()
    }

    /// The sentence a response states.
    fn message(resp: &WireResponse) -> String {
        let document: serde_json::Value =
            serde_json::from_slice(resp.body().bytes().expect("an in-memory body"))
                .expect("the error body is JSON");
        document["message"].as_str().unwrap_or_default().to_string()
    }

    /// RULE: only the NAME moves — the status, the sentence and every other member survive the
    /// restatement, however the serializer spelled the modelled shape.
    #[test]
    fn a_restated_refusal_keeps_everything_but_the_shape_name() {
        let sentence = crate::state::TAGGED_RESOURCE_NOT_FOUND_MESSAGE;
        for (case, rendered) in [
            ("the bare shape", RESOURCE_NOT_FOUND.to_string()),
            (
                "a namespaced shape",
                format!("com.amazonaws.emrserverless#{RESOURCE_NOT_FOUND}"),
            ),
        ] {
            let answer = answered(response(404, &rendered, sentence));
            assert_eq!(
                answer.headers().get(ERROR_TYPE_HEADER),
                Some(TAGGING_NOT_FOUND),
                "case {case:?}: expected the header {TAGGING_NOT_FOUND:?}, got {:?}",
                answer.headers().get(ERROR_TYPE_HEADER)
            );
            assert_eq!(
                body_shape(&answer),
                TAGGING_NOT_FOUND,
                "case {case:?}: expected the body to name {TAGGING_NOT_FOUND:?}, got {:?}",
                body_shape(&answer)
            );
            assert_eq!(
                answer.status().as_u16(),
                404,
                "case {case:?}: the status must not move, got {}",
                answer.status().as_u16()
            );
            assert_eq!(
                message(&answer),
                sentence,
                "case {case:?}: the sentence must survive the restatement, got {:?}",
                message(&answer)
            );
        }
    }

    /// Nothing to restate: a success, and a refusal of another class, are each answered exactly
    /// as they arrived.
    #[test]
    fn every_other_response_is_answered_as_the_renderer_wrote_it() {
        for (case, status, shape) in [
            ("a success", 200, ""),
            ("an ARN of another vendor", 400, super::super::WRONG_VENDOR_ERROR_TYPE),
            ("a value the member does not admit", 400, "ValidationException"),
            ("a job run id that names none", 404, "ResourceNotFound"),
            ("the store being unavailable", 500, "InternalServerException"),
        ] {
            let arrived = response(status, shape, "whatever the renderer stated");
            let body = arrived.body().bytes().expect("an in-memory body").to_vec();
            let answer = answered(arrived);
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
