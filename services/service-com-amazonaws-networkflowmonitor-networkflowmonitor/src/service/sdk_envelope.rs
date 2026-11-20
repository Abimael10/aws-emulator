// SPDX-License-Identifier: CC0-1.0

//! The envelope an SDK states around a refused call, which every client-error refusal of
//! this service carries, and the ONE request id both that envelope and the response's own
//! header name the call by.
//!
//! A client is not handed the bare sentence a service worded: it is handed that sentence
//! followed by the four facts the layer that made the call knows about the answer — the
//! SERVICE that answered, the HTTP STATUS it answered with, the ID of the request it
//! answered, and how many attempts reaching it took. Those facts are the only machine
//! readable correlation a refusal offers: a client that logs the message, matches on it or
//! quotes it into a support case reads the request id out of the text, so a refusal that
//! stops at its sentence leaves nothing to correlate and no text to match.
//!
//! None of the four is known where a sentence is worded — three of them are facts about the
//! RESPONSE — so the wording writes [`crate::state::SDK_ENVELOPE_MARKER`] after the sentence
//! ([`crate::state::refusal_message`]) and this module, the layer every response of this
//! service passes through, renders the envelope in its place:
//!
//! * the SERVICE is this one, named as this API reports itself ([`SDK_SERVICE_NAME`]);
//! * the STATUS is read off the response being sent, so the number inside the text is the
//!   number the response carries and the two cannot disagree;
//! * the REQUEST ID is the id this response names its request by — the one its header
//!   reports — so the id in the message and the id beside it are one identifier of one call;
//! * the ATTEMPTS are the one attempt a refused request got (`RelayedCall::once`): a refusal
//!   of the caller's own request is not retried, there being nothing a second attempt could
//!   change.
//!
//! The envelope itself is rendered by the workspace's one renderer of it
//! ([`relay_envelope_of`]), so a client parses the service, the status and the request id
//! back out of one form whichever emulated service answered.
//!
//! Only the marker is ever rewritten, and only in a CLIENT ERROR's body: a success, a server
//! fault and a refusal that words no marker (one the framework raised at the request
//! boundary, which keeps the framework's own wording) are passed through byte for byte.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_types::body::SdkBody;
use ::service_common::query_error_envelope::{named_request_as, new_request_id};
use ::service_common::relayed_refusal::{relay_envelope_of, RelayedCall};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The name the envelope names this service by: the service's own `sdkId`
/// (`smithy_query(query="service_info")`: `NetworkFlowMonitor`), which is neither its
/// signing name nor its ARN namespace (both `networkflowmonitor`).
const SDK_SERVICE_NAME: &str = "NetworkFlowMonitor";

/// The lowest HTTP status that is a client error, and so the lowest one this service words a
/// refusal of the caller's own request with.
const FIRST_CLIENT_ERROR_STATUS: u16 = 400;

/// The lowest HTTP status that is a SERVER fault — past the client errors, which is where the
/// envelope's attempt count stops being a fact: a fault is retried, so how many attempts the
/// caller will have made is not yet decided (see [`crate::state::refusal_message`]).
const FIRST_SERVER_ERROR_STATUS: u16 = 500;

/// The envelope a refusal this service answered with `status` under `request_id` carries.
fn envelope(status: u16, request_id: &str) -> String {
    relay_envelope_of(RelayedCall::once(SDK_SERVICE_NAME, status), request_id)
}

/// `body` with the envelope rendered where a refusal worded the marker, or `None` when it
/// words none — which makes this idempotent and leaves a body that words no marker untouched.
///
/// The marker is recognised only where a worded message ENDS it — immediately before the
/// closing quote of the member that carries it — so text a refusal echoed back from the
/// request (an ARN a tagging call addressed, a member a validation refusal names) cannot be
/// rewritten even when it carries the marker's own characters.
fn render_envelope(body: &str, status: u16, request_id: &str) -> Option<String> {
    let worded = format!("{}\"", crate::state::SDK_ENVELOPE_MARKER);
    let rendered = format!("{}\"", envelope(status, request_id));
    body.contains(worded.as_str())
        .then(|| body.replace(worded.as_str(), rendered.as_str()))
}

/// The response as it is sent: ONE request id, reported in the header this protocol names a
/// request in, and stated inside the envelope of every client-error refusal the body words.
///
/// The id is the one the response is already named by, and a freshly minted one only where no
/// layer named it ([`named_request_as`]), so this is idempotent and two layers of this service
/// can never report two ids for one call. Status, headers and every other byte of the body are
/// left exactly as the renderer wrote them.
pub fn report(resp: RtResponse) -> RtResponse {
    let (mut resp, request_id) = named_request_as(resp, new_request_id);
    let status = resp.status().as_u16();
    if !(FIRST_CLIENT_ERROR_STATUS..FIRST_SERVER_ERROR_STATUS).contains(&status) {
        return resp;
    }
    let rendered = {
        // An error body is built in memory by its renderer, so the bytes are readable here;
        // a streamed body words no refusal and is left alone.
        let Some(bytes) = resp.body().bytes() else {
            return resp;
        };
        let Ok(text) = ::std::str::from_utf8(bytes) else {
            return resp;
        };
        match render_envelope(text, status, &request_id) {
            Some(rendered) => rendered,
            None => return resp,
        }
    };
    // Only a renderer that advertised a length needs it corrected; the framework's error
    // renderers set `content-type` alone and leave framing to the transport.
    if resp.headers().get("content-length").is_some() {
        let length = rendered.len().to_string();
        let _ = resp.headers_mut().try_insert("content-length", length);
    }
    *resp.body_mut() = SdkBody::from(rendered);
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
/// served — so no refusal can leave stating a marker, and none can state an id its response
/// does not report.
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
    use super::{envelope, render_envelope, SDK_SERVICE_NAME};
    use ::service_common::query_error_envelope::is_request_id;

    /// The envelope states the four facts about the answer, in the one form the workspace
    /// renders them in, and nothing else.
    #[test]
    fn the_envelope_states_the_service_the_status_and_the_request_id() {
        // (case, the status answered, the id the response names its request by)
        let rows = [
            ("a resource that is not there", 404u16, "a0000000-0000-4000-8000-000000000001"),
            ("a request the service will not accept", 400, "b0000000-0000-4000-8000-000000000002"),
            ("a reference it cannot authorize", 403, "c0000000-0000-4000-8000-000000000003"),
            ("a quota that is already full", 402, "d0000000-0000-4000-8000-000000000004"),
            ("a name it already holds", 409, "e0000000-0000-4000-8000-000000000005"),
        ];
        for (case, status, request_id) in rows {
            let expected = format!(
                " (Service: {SDK_SERVICE_NAME}, Status Code: {status}, Request ID: \
                 {request_id}) (SDK Attempt Count: 1)"
            );
            let stated = envelope(status, request_id);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
            assert!(
                is_request_id(request_id),
                "case {case:?}: the row must state an id of the form AWS reports one in, got \
                 {request_id:?}"
            );
        }
    }

    /// The marker is rewritten only where a worded message ends it, so echoed text carrying
    /// the marker's own characters is left as the handler wrote it — and a body that words no
    /// marker is not rewritten at all.
    #[test]
    fn only_a_worded_marker_is_rendered() {
        let marker = crate::state::SDK_ENVELOPE_MARKER;
        let request_id = "f0000000-0000-4000-8000-000000000006";
        let body = format!(
            "{{\"__type\":\"ResourceNotFoundException\",\"message\":\"Resource \
             [arn:aws:networkflowmonitor:us-west-2:000000000000:monitor/{marker}-echoed] \
             doesn't exist{marker}\"}}"
        );
        let rendered = render_envelope(&body, 404, request_id).expect("the message words it");
        assert!(
            rendered.ends_with(&format!("{}\"}}", envelope(404, request_id))),
            "case \"a refusal wording the marker\": expected the envelope at the end of the \
             message, got {rendered:?}"
        );
        assert!(
            rendered.contains(&format!("monitor/{marker}-echoed")),
            "case \"echoed text carrying the marker\": expected it left alone, got {rendered:?}"
        );
        assert_eq!(
            render_envelope(&rendered, 404, request_id),
            None,
            "case \"a rendered body\": expected no second rendering"
        );
        for case in [
            "{\"__type\":\"ValidationException\",\"message\":\"1 validation error detected: \
             Value null at 'metricName' failed to satisfy constraint: Member must not be \
             null\"}",
            "{\"monitorName\":\"probe\"}",
        ] {
            assert_eq!(
                render_envelope(case, 400, request_id),
                None,
                "case {case:?}: expected no rendering"
            );
        }
    }
}
