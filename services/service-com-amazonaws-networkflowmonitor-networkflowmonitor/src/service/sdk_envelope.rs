// SPDX-License-Identifier: CC0-1.0

//! The ONE request id a response names its request by, reported in the header this protocol
//! names a request in.
//!
//! What a refused call CARRIES is the sentence the service worded and the id of the request it
//! answered. The envelope a client reads around that sentence — the service that answered, the
//! HTTP status, the request id and how many attempts reaching it took — is the CLIENT's
//! rendering of facts its own call already knows: an SDK appends it when it renders the
//! exception, and no byte of it reaches the wire. So no wording of this service states it
//! ([`crate::state::refusal_message`]), and this layer adds nothing to a message: it names the
//! request, and passes the status, the headers and every byte of the body through exactly as
//! the renderer wrote them.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::service_common::query_error_envelope::{named_request_as, new_request_id};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The response as it is sent: ONE request id, reported in the header this protocol names a
/// request in.
///
/// The id is the one the response is already named by, and a freshly minted one only where no
/// layer named it ([`named_request_as`]), so this is idempotent and two layers of this service
/// can never report two ids for one call. Status, headers and every byte of the body are left
/// exactly as the renderer wrote them.
pub fn report(resp: RtResponse) -> RtResponse {
    let (resp, _request_id) = named_request_as(resp, new_request_id);
    resp
}

/// Serves a request as the operation this service's generated routing table names for it, and
/// finishes whatever that operation answered ([`report`]).
///
/// It adds nothing else and holds no operation of its own: the request, the service and the
/// context are handed to the modelled operation unchanged, so deserialization, validation, the
/// handler and every status, code and sentence of the answer stay the framework's and the
/// operation's own. The routing table holds only generated operations, so the lookup below can
/// never resolve back to this type.
///
/// It is the one layer every response an operation of this service produced passes through — a
/// success, a modelled refusal, and a constraint refusal raised while an operation was being
/// served — so none can state an id its response does not report.
pub struct SdkEnvelopeOperation;

impl ErasedOperation for SdkEnvelopeOperation {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // `override_operation` hands this type back only for a request the routing table
            // can serve, so the lookup resolves; a request it cannot serve keeps the
            // framework's routing-error answer either way.
            let routed = svc
                .definition()
                .protocol()
                .routing_key(req)
                .ok()
                .and_then(|key| svc.definition().routing_table().get(key));
            let Some(operation) = routed else {
                return svc.definition().routing_error_response(req);
            };
            report(operation.run(svc, req, ctx).await)
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names", which is the
        // operation this layer serves: wrapping must not change which schemes sign it.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::report;
    use ::aws_smithy_runtime_api::http::Response as RtResponse;
    use ::aws_smithy_types::body::SdkBody;
    use ::service_common::query_error_envelope::is_request_id;

    /// The id the header carries, if any.
    fn reported_id(resp: &RtResponse) -> Option<String> {
        resp.headers()
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-amzn-requestid"))
            .map(|(_, value)| value.to_string())
    }

    /// A refusal is reported with an id of the form AWS names a request in, and its body is
    /// passed through byte for byte — the envelope a client renders is none of this service's
    /// business.
    #[test]
    fn a_refusal_is_named_and_its_sentence_is_left_alone() {
        // (case, the status answered, the body the renderer wrote)
        let rows = [
            (
                "a resource that is not there",
                404u16,
                "{\"__type\":\"ResourceNotFoundException\",\"message\":\"There's no scope with \
                 ARN: [arn:aws:networkflowmonitor:us-west-2:000000000000:scope/\
                 00000000-0000-4000-8000-000000000001]\"}",
            ),
            (
                "a request the service will not accept",
                400,
                "{\"__type\":\"ValidationException\",\"message\":\"1 validation error \
                 detected: Value null at 'metricName' failed to satisfy constraint: Member \
                 must not be null\"}",
            ),
            ("a served read", 200, "{\"scopes\":[]}"),
        ];
        for (case, status, body) in rows {
            let built = ::http::Response::builder()
                .status(status)
                .body(SdkBody::from(body))
                .expect("the row builds a response");
            let reported = report(
                RtResponse::try_from(built).expect("the response converts to the runtime type"),
            );
            assert_eq!(
                reported.status().as_u16(),
                status,
                "case {case:?}: expected the status to be passed through, got {}",
                reported.status()
            );
            let sent = reported
                .body()
                .bytes()
                .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                .unwrap_or_default();
            assert_eq!(
                sent, body,
                "case {case:?}: expected the body byte for byte, got {sent:?}"
            );
            let id = reported_id(&reported).unwrap_or_default();
            assert!(
                is_request_id(&id),
                "case {case:?}: expected an id of the form AWS reports one in, got {id:?}"
            );
        }
    }
}
