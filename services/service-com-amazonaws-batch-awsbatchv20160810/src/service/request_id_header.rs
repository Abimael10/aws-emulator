// SPDX-License-Identifier: CC0-1.0

//! The header this service reports a response's correlation id in.
//!
//! Every response AWS sends names the request it answered, in the `x-amzn-requestid`
//! response header — a success as much as a failure, whichever layer rendered it — which is
//! where an SDK's response metadata reads that id from. The framework's restJson1 renderers
//! write the document and no such header (`vendor/` is read-only), so this module reports
//! it.
//!
//! Batch's frontend words the refusals it WRAPS as `Error executing request, Exception :
//! <detail>, RequestId: <id>` (`crate::state::client_exception_message`), so such a refusal
//! already STATES the id of the call it answers, inside its `message`. The header has to
//! report THAT string rather than a second one, or a client correlating on the header and
//! one reading the message see two ids for one call and neither can be matched against the
//! other. So the id a response's own document states is the id it is reported with
//! ([`crate::state::enveloped_request_id`]) — no byte of the body is rewritten — and every
//! other response (a success, a refusal Batch words BARE, the scaffold's 501) is reported
//! with a freshly minted one, from the same minter the envelope writer uses
//! ([`crate::state::new_request_id`]), so a client that logs the id of a failed call always
//! has one.
//!
//! It reaches every answer the routing table's operation produces through
//! [`RequestIdHeaderOperation`], the operation wrapper `Service::override_operation` hands
//! back, and the answer to a request that named no modelled operation through
//! `Service::override_routing_error`. That wrapper is also where IAM's decision about the
//! request is taken, ahead of the operation it delegates to (see
//! [`crate::service::authorization`]), so a refusal that decision answers is reported with
//! this header too.
//!
//! `service_common::query_error_envelope` is the shared home this rule has for every
//! emulated service, and the service-local statement here records it as its migration
//! target the way the other service-local copies do: that module's reports either mint
//! unconditionally or read an `awsQuery` envelope, so neither of them can report the id a
//! Batch document states, and teaching it a per-service report belongs to the migration
//! rather than to this fix.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};
use std::sync::Arc;

use super::authorization;

/// The header AWS reports a response's own request id in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The member a Batch error document carries its message in (`ClientException$message`,
/// `ServerException$message`).
const MESSAGE_MEMBER: &str = "message";

/// The request id the response's document states, or `None` when it states none.
///
/// An error envelope is built in memory by its renderer, so the bytes are readable here; a
/// streamed body is not one and yields `None`.
fn stated_request_id(resp: &RtResponse) -> Option<String> {
    let bytes = resp.body().bytes()?;
    let document: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let message = document.get(MESSAGE_MEMBER)?.as_str()?;
    crate::state::enveloped_request_id(message).map(str::to_string)
}

/// The response with the request it answers named in [`REQUEST_ID_HEADER`]: the id its own
/// document states, and a freshly minted one for a response that states none.
///
/// Status, body and every other header are left exactly as the renderer wrote them, and a
/// response that already names its request keeps the id it has — so this is idempotent and
/// two layers of this service cannot report two ids for one call.
pub fn with_request_id_header(mut resp: RtResponse) -> RtResponse {
    if resp.headers().get(REQUEST_ID_HEADER).is_some() {
        return resp;
    }
    let request_id = stated_request_id(&resp).unwrap_or_else(crate::state::new_request_id);
    // Dropped rather than fatal if the HTTP layer cannot carry it: a header is not worth
    // failing a modelled answer for.
    let _ = resp.headers_mut().try_insert(REQUEST_ID_HEADER, request_id);
    resp
}

/// The key this service's routing table names this request's operation by — which is that
/// operation's own name ([`smithy_server_runtime::Operation::NAME`]) — or `None` when the
/// request names no modelled operation.
fn routing_key<'s>(svc: &'s dyn Service, req: &'s RtRequest) -> Option<&'s str> {
    svc.definition().protocol().routing_key(req).ok()
}

/// The modelled operation this service's routing table names for this request, or `None`
/// when the request names none.
fn modeled_operation<'s>(svc: &'s dyn Service, req: &'s RtRequest) -> Option<&'s dyn ErasedOperation> {
    svc.definition()
        .routing_table()
        .get(routing_key(svc, req)?)
}

/// Serves a modelled operation through the framework exactly as the routing table would,
/// then reports the request that answer belongs to in the header AWS carries it in.
///
/// It stores no operation of its own: the one it delegates to is the one the service's
/// routing table names for the request, so the framework still owns the whole
/// deserialize → validate → handle → serialize sequence — and every status, code and
/// message it answers with — while this wrapper owns one header.
///
/// What it DOES decide first is the decision AWS takes in front of a handler: whether IAM
/// lets the caller perform the action the request names ([`authorization`]). That decision
/// reads the caller's identity and its policies out of the ONE shared engine, which is why
/// this wrapper carries a handle on it; a refused request never reaches the delegate, so it
/// mints nothing and reports nothing — and its refusal is reported with the header this
/// module owns, exactly as every other answer of this service is.
pub struct RequestIdHeaderOperation<Eng> {
    engine: Arc<Eng>,
}

impl<Eng> RequestIdHeaderOperation<Eng> {
    /// Build the wrapper from the shared engine the service was mounted on.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for RequestIdHeaderOperation<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // IAM's decision, taken before the operation reads or writes anything: the
            // action is the operation's own name under this service's prefix, and the
            // resource is the one the REQUEST addresses.
            let denial = routing_key(svc, req).and_then(|operation| {
                let resource = authorization::judged_resource(ctx, operation, req);
                authorization::authorize(&*self.engine, ctx, operation, &resource).err()
            });
            if let Some(denial) = denial {
                return with_request_id_header(authorization::refusal_response(&denial));
            }
            match modeled_operation(svc, req) {
                Some(op) => with_request_id_header(op.run(svc, req, ctx).await),
                // Unreachable through `Service::override_operation`, which claims a
                // request only when the routing table holds its key; answered as the
                // framework answers an unroutable request rather than guessed at.
                None => with_request_id_header(svc.definition().routing_error_response(req)),
            }
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names" — the
        // operation this wrapper delegates to, so its model-declared schemes decide and
        // this wrapper cannot weaken them.
        None
    }
}
