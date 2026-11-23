// SPDX-License-Identifier: CC0-1.0

//! WHERE this service's own routes are served: the endpoint a control call arrives on,
//! and not an address this service announced for a DATA plane.
//!
//! `GetDataEndpoint` exists because a stream's media APIs are not served at the
//! control-plane endpoint — "Gets an endpoint for a specified stream for either reading or
//! writing" — and the separation holds in both directions: the address announced for a
//! plane serves that plane's media routes, and the control operations are served at the
//! endpoint the discovery call itself was made to. Kinesis Video Media states the same
//! rule from the data side ("Caller used wrong endpoint to write data to a stream"), and a
//! public data host carries no control route at all.
//!
//! So a control-plane call that reached an address this store announced for a plane
//! ([`super::super::state::published_plane`]) reached an endpoint that binds no route for
//! its path at all, and is answered as such: the status this family gives a path it routes
//! nothing for, the refusal's class in the `x-amzn-errortype` header
//! ([`super::unknown_operation::UNKNOWN_OPERATION_CODE`]) and a body that words NOTHING —
//! no operation run, no member of the request reported, no sentence, and nothing in the
//! store touched. The class is stated where a restJson1 client reads a failure's code
//! because this is the refusal a client re-discovers its endpoint off: it is the one a
//! caller reaches by addressing the wrong plane of an endpoint set it was HANDED. Every
//! other address — the endpoint the client is already talking to, this service's published
//! regional host — keeps serving the whole control surface.
//!
//! "Words nothing" is a thing a JSON error document has to STATE rather than leave out: an
//! SDK reading one substitutes the EMPTY STRING for a message member the document does not
//! carry and reports no message only for a member whose value is `null`
//! (`service_common::error_message_member`), so this answer carries that member as an
//! explicit JSON `null` ([`unrouted_address_body`]). A body left out altogether reaches the
//! caller as a refusal whose reason is the empty string — a value this refusal never has,
//! and the one thing that distinguishes it from a modelled 404 that does word a sentence.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// Whether this request reached an address this store announced for a data plane.
///
/// Only the addresses this emulator PUBLISHED are read: it knows the plane of those and of
/// no others, so a request that reached a host naming a service — a caller resolving AWS's
/// own hostnames — is served the control surface as before.
pub fn addressed_a_data_plane<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> bool {
    let host = req.headers().get(::http::header::HOST.as_str());
    let Some(address) =
        ::service_common::data_plane_addressing::reached_emulator_endpoint(req.uri(), host)
    else {
        return false;
    };
    crate::state::published_plane(engine, &address).is_some()
}

/// Answers a control-plane call that reached a data plane's address: the refusal an
/// endpoint gives a path it routes nothing for.
///
/// Served as an operation rather than decided in `can_handle`, because the answer is this
/// endpoint's own and not the router's "no service claimed it": the request did reach this
/// emulator, at an address that serves another plane's routes.
#[derive(Debug, Clone, Copy)]
pub struct UnroutedOnDataPlaneAddress;

impl ErasedOperation for UnroutedOnDataPlaneAddress {
    fn run<'r>(
        &'r self,
        _svc: &'r dyn Service,
        _req: &'r RtRequest,
        _ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move { unrouted_address_response() })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // The MODELLED operation the routing key names decides which schemes sign a
        // request, and the router resolves that from the routing table rather than from
        // the operation this service handed back, so nothing here weakens them.
        None
    }
}

/// The WHOLE body of that answer: a JSON error document whose one member is the member an
/// error document of this service carries its text in
/// ([`super::error_message_member::MODELLED_MESSAGE_MEMBER`]), reported as the explicit JSON
/// `null` of a refusal that words nothing.
///
/// Nothing in it comes from the request or from this store: the document is the same bytes
/// for every request that reaches it, because no operation, member, path or value was
/// identified to state. The member is PRESENT and null rather than left out so that an SDK
/// reading the document reports no message for the refusal instead of the empty string it
/// substitutes for a member a JSON error document omits.
///
/// It is built through the JSON writer rather than written out as a literal, so the one
/// member name above is the only place that spelling lives.
#[must_use]
pub(crate) fn unrouted_address_body() -> String {
    ::serde_json::json!({
        super::error_message_member::MODELLED_MESSAGE_MEMBER: ::serde_json::Value::Null,
    })
    .to_string()
}

/// The front door's own answer to a request for a control route that reached an address
/// this service announced for a data plane: the 404 an endpoint gives a path it routes
/// NOTHING for, identified by the refusal's own class and wording nothing.
///
/// A data endpoint hosts that plane's media APIs only, so a control route asked for there
/// is a path that endpoint binds no route for — the status an endpoint answers such a path
/// with ([`super::unknown_operation::UNROUTED_PATH_STATUS`]), and a body that states no
/// sentence: no operation was identified, so there is no member, value or reason to report
/// ([`unrouted_address_body`], whose message member is the JSON `null` that reaches a client
/// as NO message at all).
///
/// What it does state is its CLASS, in the `x-amzn-errortype` header
/// ([`super::unknown_operation::UNKNOWN_OPERATION_CODE`]) — the one field a restJson1 client
/// resolves a failure's code from, and the only identification in this answer. This is the
/// refusal a client that followed `GetDataEndpoint` and then addressed the wrong plane of
/// the endpoint set it was handed meets, and the code is what it branches on to discover its
/// endpoint again, so an answer whose class an SDK cannot read leaves that loop unable to
/// fire.
fn unrouted_address_response() -> RtResponse {
    let mut response = RtResponse::new(
        ::aws_smithy_runtime_api::http::StatusCode::try_from(
            super::unknown_operation::UNROUTED_PATH_STATUS,
        )
        .expect("the status is a constant of the front door's own module"),
        ::aws_smithy_types::body::SdkBody::from(unrouted_address_body()),
    );
    response.headers_mut().insert(
        "x-amzn-errortype",
        super::unknown_operation::UNKNOWN_OPERATION_CODE,
    );
    response
        .headers_mut()
        .insert("content-type", super::unknown_operation::JSON_CONTENT_TYPE);
    response
}
