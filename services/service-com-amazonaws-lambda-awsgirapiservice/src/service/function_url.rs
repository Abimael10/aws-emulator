// SPDX-License-Identifier: CC0-1.0

//! The FUNCTION URL data plane: how a request to the endpoint `CreateFunctionUrlConfig`
//! handed out is served by the function that URL is attached to.
//!
//! A function URL is "a dedicated HTTP(S) endpoint that you can use to invoke your
//! function", published at `https://<url-id>.lambda-url.<region>.on.aws/`. It is NOT a
//! modelled operation — no routing key names it — so it is served through the same two
//! seams the download plane uses: [`claims_parts`] in `Service::can_handle`, so the router
//! does not answer `UnknownService` for an address this service published, and
//! [`FunctionUrlPlane`] in `Service::override_operation`, which serves it.
//!
//! What it serves is one INVOCATION of the function the URL is attached to, through the
//! same in-process engine every other invocation runs on
//! ([`crate::state::run_invocation`]): the request becomes the documented payload-format-2.0
//! event, the handler RUNS, and the response is what the handler's result describes (see
//! [`event`]). Nothing is answered that the function did not produce — a run that raised, a
//! run that timed out, a result that describes no response and a response past the payload
//! ceiling are all the `502` Lambda answers for them, carrying [`BAD_GATEWAY_MESSAGE`] and
//! nothing of the function's own error.
//!
//! ## Which caller is admitted
//!
//! `FunctionUrlConfig$AuthType` decides: "`AWS_IAM` – Restrict access to authenticated
//! users only" and "`NONE` – Bypass IAM authentication to create a public endpoint". The
//! difference is carried by the SCHEMES the plane serving the request declares, not by a
//! check in this module — the emulator verifies every request signature and a service never
//! verifies one itself — so the plane is mounted twice, once per auth type
//! ([`FunctionUrlPlane::anonymous`] and [`FunctionUrlPlane::aws_iam`]), and
//! `override_operation` picks the one the addressed URL's stored `AuthType` names. A URL
//! whose `AuthType` is `AWS_IAM` is therefore served only to a request whose SigV4
//! signature the emulator verified — against this service's own signing name, so a
//! signature scoped to another service, an unsigned request and a request whose body was
//! changed after signing are all refused `403` by the framework before this plane runs —
//! and the caller that signature speaks for is reported in the event's
//! `requestContext.authorizer.iam`.
//!
//! A `NONE` URL bypasses that authentication, so the request reaches this plane carrying
//! whatever credential it came with, and WHO it is admitted as is then the one decision this
//! plane makes itself ([`FunctionUrlPlane::admits_the_caller`]): the IAM decision every
//! invocation of this service goes through
//! (`crate::state::invoke_authorization::admits_function_url_request`), under the endpoint's
//! own action `lambda:InvokeFunctionUrl`. A request presenting a credential speaks for that
//! principal and is decided on its policies and the function's own; a request presenting NONE
//! speaks for nobody, so the only `Principal` element that can name it is the wildcard — which
//! is what makes such an endpoint PUBLIC exactly while the function's resource-based policy
//! grants the invocation to everyone. A policy whose grants all name a principal, and a
//! function with no policy at all, admit no anonymous caller and refuse it the URL
//! authorizer's whole `403` document ([`AUTHORIZER_FORBIDDEN_MESSAGE`]).

//! ## What this plane reads of the configuration, and what it does not
//!
//! It reads the two members that decide WHO is served and WHAT serves them: `AuthType` and
//! the qualifier the URL is attached to, and `FunctionUrlConfig$Cors` — "The cross-origin
//! resource sharing (CORS) settings for your function URL" — which is the ENDPOINT's own
//! setting and so is applied here rather than by the handler ([`cors`]): a CORS PREFLIGHT is
//! answered by this plane itself with the permission the configuration grants and NOTHING
//! invoked, and the answer to every other request carries that permission for an origin the
//! configuration admits. A request whose origin it does not admit is served exactly as one
//! carrying no origin is — the headers are what a browser enforces with, so nothing of the
//! function's answer is withheld for them. `InvokeMode` decides WHAT the run's answer is read
//! as: a `BUFFERED` URL answers the response the value the handler returned describes, while a
//! `RESPONSE_STREAM` URL answers the bytes the handler WROTE — "your function streams payload
//! results as they become available" — shaped by the metadata it announced ahead of them when
//! it announced any ([`event::mapped_stream`]).
//!
//! A preflight carries an `Origin`, and the emulator's front door refuses a request whose
//! browser-set headers say a browser sent it from another site before any operation of this
//! service runs. That guard stays in front of this plane: a preflight it admits is answered
//! here, and one it refuses never reaches this plane at all — a property of this emulator,
//! which serves a developer's machine, rather than of the service it emulates.

use std::sync::Arc;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The event one request becomes and the response one result becomes — the documented
/// payload format 2.0; see the module's own header.
pub mod event;

/// What the endpoint's own `Cors` settings make of a request, and what it answers a
/// browser's PREFLIGHT; see the module's own header.
mod cors;

use event::{mapped_result, mapped_stream, url_event, CallerIdentity, Mapped, UrlRequest};

/// The body of every `502` a function URL answers.
///
/// Lambda does not pass the function's own failure through to the caller — the client is
/// told the gateway got no usable response, and the function's error is in the function's
/// log — so one sentence covers every class of it.
const BAD_GATEWAY_MESSAGE: &str = "Internal Server Error";

/// The status Lambda answers when the function produced no usable response.
const BAD_GATEWAY_STATUS: u16 = 502;

/// The status this plane answers when the INVOCATION never happened at all, because this
/// emulator holds nothing it could run for the function.
///
/// It is not a `502`: a `502` says the function answered unusably, and nothing ran here.
/// The body names the reason, which is the same diagnostic an asynchronous delivery records
/// when a run cannot be started ([`crate::state::async_invocation`]).
const NOT_RUN_STATUS: u16 = 500;

/// The largest response a function URL serves is not stated here: it is the RUN's own
/// bound. "The maximum payload size is 6 MB for synchronous invocations" (the model's own
/// `Invoke` documentation) bounds what a handler may answer at all, so a result past it is
/// stopped where it is produced ([`crate::state::execution`]) and reaches this plane as the
/// function error it became — answered by the same [`bad_gateway`] every other unusable
/// response is. One place states the ceiling.
///
/// The REQUEST half of that quota is this plane's, because a function URL request becomes
/// the event the function is invoked with: "the maximum payload size is 6 MB for synchronous
/// invocations", and a function URL invokes synchronously, so a body larger than that is one
/// no invocation can carry and is refused before the event is built from it.
const MAX_REQUEST_BYTES: usize = 6 * 1024 * 1024;

/// The status and the sentence a request whose body is past [`MAX_REQUEST_BYTES`] is
/// refused with: the payload is larger than the invocation it would become can carry.
const TOO_LARGE_STATUS: u16 = 413;
/// See [`TOO_LARGE_STATUS`].
const TOO_LARGE_MESSAGE: &str = "Request Entity Too Large";

/// What a request whose body this plane cannot read at all is told: the invocation carries
/// the request's bytes, so there is no event to build and nothing runs.
const UNREADABLE_BODY_MESSAGE: &str =
    "the request's body could not be read, so no invocation was made for it";

/// The header a function URL response reports its request id in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The address a request whose connection this emulator observed none for came from: the
/// core hands a handler nothing about the peer of a connection it accepted, and every
/// connection to a loopback listener comes from loopback, so that is the address such a
/// request arrived from — the same reading every other plane of this emulator that reports
/// the address a request came from uses.
const LOOPBACK_SOURCE_IP: &str = "127.0.0.1";

/// The address the CONNECTION this request arrived on was opened from, as the end that
/// accepted it knows it.
///
/// Read off the CONNECTION and never off the request: a client of this emulator states where
/// it opens a connection from when it opens one
/// ([`::service_common::dialable_data_planes`]), and [`LOOPBACK_SOURCE_IP`] is the address of
/// a connection nothing stated one for. That is what makes the address a function URL
/// attributes a call to unspoofable — a header the client wrote is the client's claim about
/// the hops ahead of this one, not a statement about where this one came from.
fn connection_address() -> String {
    ::service_common::dialable_data_planes::connected_from()
        .map_or_else(|| LOOPBACK_SOURCE_IP.to_string(), |from| from.ip().to_string())
}

/// The function URL `addressed` names, out of the one shared store.
///
/// The ADDRESS is read first, with no guard taken: a request that addresses no function URL
/// at all — every control-plane call, and every other service's traffic, which this runs for
/// on the way to `can_handle`'s answer — is answered before the store is touched.
///
/// Two readings, and which one decides is the whole question. The AUTHORITY the URL was
/// PUBLISHED on ([`crate::state::function_url::published_function_url`]) was opened for ONE
/// function URL and identifies it, so a path a consumer supplies there is a path WITHIN that
/// URL — including one spelled like another URL's own addressing, which names nothing else at
/// this address. The addressings that carry the url id in the path
/// ([`crate::state::function_url::addressed_url`]) are how a client reaching the emulator's
/// own endpoint names one, and they decide only when they name the URL this address serves:
/// that is the announced value followed verbatim, whose marked prefix is the endpoint's base
/// rather than a path within it. Both resolve only to a configuration this store HOLDS.
pub(super) fn target_of<Eng: Send + Sync + 'static>(
    engine: &Eng,
    addressed: &crate::state::AddressedFunctionUrl<'_>,
) -> Option<crate::state::FunctionUrlTarget> {
    let addressed_url = crate::state::function_url::addressed_url(addressed);
    let state = crate::state::service_state(engine)?;
    let store = state.lock();
    let published = crate::state::function_url::published_function_url(&store, addressed);
    let named = addressed_url
        .and_then(|named| crate::state::function_url::held_function_url(&store, &named));
    match (published, named) {
        (Some(published), Some(named)) if named.url_id == published.url_id => Some(named),
        (Some(published), _) => Some(published),
        (None, named) => named,
    }
}

/// Whether `addressed` names an ADDRESS this service PUBLISHED a function URL on, whatever
/// the store holds behind it ([`crate::state::ComAmazonawsLambdaAwsgirapiserviceState::
/// answers_at_published_authority`]).
///
/// An address this service handed a caller stays this service's to answer: a function URL
/// whose configuration was deleted is still an address of this service, and what a caller
/// reading it is told is this service's to say rather than the router's "no service is
/// registered for this".
fn addresses_a_published_endpoint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    addressed: &crate::state::AddressedFunctionUrl<'_>,
) -> bool {
    let Some(host) = addressed.host.as_deref() else {
        return false;
    };
    // Every published address is a `host:port` nobody can read a service off, so a request to
    // another service's endpoint host is never read as one.
    if ::service_common::data_plane_addressing::service_named_by_host(host).is_some() {
        return false;
    }
    crate::state::service_state(engine)
        .is_some_and(|state| state.lock().answers_at_published_authority(host))
}

/// Which callers a mounted plane serves, which is what its declared auth schemes say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admits {
    /// `AuthType=NONE`, reached by a request presenting NO credential: "Bypass IAM
    /// authentication to create a public endpoint", so nothing authenticates it and it
    /// speaks for nobody — served while the function's resource-based policy grants the
    /// invocation to everyone ([`FunctionUrlPlane::admits_the_caller`]).
    Anonymous,
    /// `AuthType=NONE`, reached by a request that PRESENTS a SigV4 credential: the caller
    /// offered an identity, so this mounting declares the SigV4 scheme and the emulator
    /// verifies that credential before the plane runs, exactly as it does for an `AWS_IAM`
    /// URL. What the verified principal may do is then the function's own policy's to say.
    SignedPublic,
    /// `AuthType=AWS_IAM`: "Restrict access to authenticated users only", so the request is
    /// served only once the emulator has verified its SigV4 signature.
    SignedRestricted,
}

impl Admits {
    /// Whether a request this mounting serves speaks for nobody — the one mounting the
    /// emulator authenticates no caller of, which is what makes the wildcard principal the
    /// only one that can name it.
    fn is_anonymous(self) -> bool {
        self == Self::Anonymous
    }

    /// Whether this mounting serves a URL whose `AuthType` is `NONE`, whichever credential
    /// the request arrived with — the two mountings whose callers the function's own
    /// resource-based policy admits.
    fn serves_a_public_url(self) -> bool {
        matches!(self, Self::Anonymous | Self::SignedPublic)
    }
}

/// The function URL data plane: one invocation of the function a URL is attached to.
///
/// Mounted once per kind of caller ([`Admits`]), because which callers a request is admitted
/// as is carried by the auth schemes the serving operation declares — the emulator verifies
/// every request signature and a service never verifies one itself.
pub struct FunctionUrlPlane<Eng> {
    /// The ONE shared engine this service's operations read and write, so a request served
    /// here sees exactly the functions the control plane persisted.
    engine: Arc<Eng>,
    /// Which callers this mounting serves.
    admits: Admits,
}

impl<Eng: Send + Sync + 'static> FunctionUrlPlane<Eng> {
    /// The plane serving a `NONE` URL to a request that presents no credential.
    pub fn anonymous(engine: Arc<Eng>) -> Self {
        Self {
            engine,
            admits: Admits::Anonymous,
        }
    }

    /// The plane serving a `NONE` URL to a request that presents a SigV4 credential, which
    /// the emulator verifies because this mounting declares that scheme.
    pub fn signed_public(engine: Arc<Eng>) -> Self {
        Self {
            engine,
            admits: Admits::SignedPublic,
        }
    }

    /// The plane serving the URLs whose `AuthType` is `AWS_IAM`.
    pub fn aws_iam(engine: Arc<Eng>) -> Self {
        Self {
            engine,
            admits: Admits::SignedRestricted,
        }
    }

    /// Whether THIS plane serves the request these parts came from — the half of
    /// `Service::can_handle` that claims a function URL.
    ///
    /// Narrowed to a function URL this service HOLDS, read from the one shared store, or to
    /// an address that NAMES this plane and nothing else
    /// ([`crate::state::function_url::named_endpoint`]) — so nothing is claimed that this
    /// service cannot answer about and exactly one service claims a request. An address of
    /// this plane that no configuration sits behind is this service's to answer too: the
    /// caller was handed that address by this service, and what it is told about it is not
    /// the router's "no service is registered for this".
    pub fn claims(&self, parts: &smithy_server_runtime::http::request::Parts) -> bool {
        let addressed = crate::state::AddressedFunctionUrl::of_parts(parts);
        crate::state::function_url::named_endpoint(&addressed).is_some()
            || addresses_a_published_endpoint(&*self.engine, &addressed)
            || target_of(&*self.engine, &addressed).is_some()
    }

    /// Whether the runtime-API request `req` addresses this plane at all — by an addressing
    /// that names it and nothing else, by an ADDRESS this service published a function URL
    /// on, or by naming a function URL this store HOLDS, which is exactly the reading
    /// `claims` admitted the request by.
    pub fn addressed(&self, req: &RtRequest) -> bool {
        let addressed = crate::state::AddressedFunctionUrl::of_request(req);
        crate::state::function_url::named_endpoint(&addressed).is_some()
            || addresses_a_published_endpoint(&*self.engine, &addressed)
            || target_of(&*self.engine, &addressed).is_some()
    }

    /// The function URL the runtime-API request `req` addresses, or `None` when it
    /// addresses none this service holds.
    pub fn target(&self, req: &RtRequest) -> Option<crate::state::FunctionUrlTarget> {
        target_of(
            &*self.engine,
            &crate::state::AddressedFunctionUrl::of_request(req),
        )
    }

    /// Whether this mounting is the one that serves a request to a URL whose `AuthType` is
    /// `auth_type`, presenting `credential` or none.
    ///
    /// A `NONE` URL is served by one of two mountings, because what the emulator
    /// authenticates is decided per REQUEST and not per URL: a request that presents a SigV4
    /// credential is served by the mounting that declares that scheme, so the emulator
    /// verifies it, and one that presents none by the mounting that declares no scheme at
    /// all. Which of the two a request is, is read through this emulator's ONE reader of a
    /// request's credential ([`crate::state::invoke_authorization::presented_credential`]) —
    /// a ROUTING choice, not a decision about what that credential may do, which stays with
    /// the schemes the chosen mounting declares and the policy the plane reads.
    pub fn serves(&self, auth_type: &str, credential: Option<&str>) -> bool {
        match self.admits {
            Admits::SignedRestricted => auth_type == AUTH_TYPE_AWS_IAM,
            Admits::SignedPublic => auth_type != AUTH_TYPE_AWS_IAM && credential.is_some(),
            Admits::Anonymous => auth_type != AUTH_TYPE_AWS_IAM && credential.is_none(),
        }
    }

    /// Serve one request: run the function the URL is attached to with the event the
    /// request becomes, and answer what the run produced — with the endpoint's own CORS
    /// permission on the answer, and ANSWERING a preflight itself rather than running
    /// anything for it (see [`cors`]).
    async fn serve(&self, req: &RtRequest, ctx: &OperationContext<'_>) -> RtResponse {
        let Some(target) = self.target(req) else {
            // The URL was resolved when the request was CLAIMED; a configuration deleted
            // between the two — and an address of this plane no configuration ever sat
            // behind — admits no caller at all, so what a client is told is the refusal of
            // a caller this endpoint does not admit, and nothing of whether a URL was ever
            // configured there.
            return unauthorized();
        };
        // The endpoint's own `Cors` settings, which decide the answer to a preflight whole
        // and decorate the answer to everything else. A URL configured with none states no
        // permission at all, and its preflight is a request like any other.
        if let Some(configured) = target.cors.as_ref() {
            if let Some(asked) = cors::preflight_of(req) {
                return cors::answered_preflight(configured, &asked);
            }
        }
        let permitted = target
            .cors
            .as_ref()
            .map_or_else(Vec::new, |configured| cors::decoration(configured, req));
        let mut response = self.invoked(&target, req, ctx).await;
        cors::decorate(&mut response, &permitted);
        response
    }

    /// The answer the INVOCATION of the function `target`'s URL is attached to produces —
    /// every answer of this plane except the preflight the endpoint answers itself.
    async fn invoked(
        &self,
        target: &crate::state::FunctionUrlTarget,
        req: &RtRequest,
        ctx: &OperationContext<'_>,
    ) -> RtResponse {
        let request_id = crate::state::mint_request_id();
        // WHO is admitted is decided first, at the endpoint's own front door: before the
        // request is read as an invocation at all, and with no guard of this service's store
        // held, because the decision reads the function's resource-based policy under that
        // store's own lock (see [`FunctionUrlPlane::admits_the_caller`]).
        if self.admits.serves_a_public_url() {
            if let Err(reason) = self.admits_the_caller(target, ctx) {
                ::tracing::info!(
                    request_id = %request_id,
                    function = %target.function_name,
                    reason = %reason,
                    "a function URL request was not admitted by the function's resource-based \
                     policy"
                );
                return forbidden(Some(&request_id));
            }
        }
        // The request's own bytes, read ONCE: the event the function is invoked with carries
        // them, so a body this plane cannot read is a request it cannot turn into an
        // invocation at all — reported as the one it could not make rather than invoked with
        // an empty body the caller did not send.
        let Some(body) = req.body().bytes() else {
            ::tracing::warn!(
                request_id = %request_id,
                "a function URL request carries a body this emulator cannot read, so nothing \
                 ran"
            );
            return plane_error(NOT_RUN_STATUS, UNREADABLE_BODY_MESSAGE);
        };
        // The event this request becomes is the invocation's payload, so the quota on that
        // payload bounds the request: a body past it is refused before anything is built
        // from it (see [`MAX_REQUEST_BYTES`]).
        if body.len() > MAX_REQUEST_BYTES {
            return plane_error(TOO_LARGE_STATUS, TOO_LARGE_MESSAGE);
        }
        let incoming = Incoming {
            req,
            body,
            request_id: &request_id,
            received: ::std::time::SystemTime::now(),
        };
        let mut prepared = match self.prepare(target, &incoming, ctx) {
            Ok(prepared) => prepared,
            Err(response) => return *response,
        };
        // A function whose environment variables are encrypted with a customer managed key
        // is only invoked while that key can decrypt them, exactly as on `Invoke`: the
        // invocation begins with that decrypt, and a key that cannot be used means nothing
        // ran.
        if let Some(fault) = crate::state::environment_key_fault(
            ctx.callback(),
            &prepared.invocation.region,
            prepared.kms_key_arn.as_deref(),
            &prepared.invocation.variables,
        )
        .await
        {
            ::tracing::warn!(
                function = %prepared.invocation.function_name,
                detail = ?fault,
                "a function URL request reached a function whose environment variables cannot \
                 be decrypted, so nothing ran"
            );
            return bad_gateway(&request_id);
        }
        // A function URL request is an invocation of the function, so it draws on the SAME
        // concurrency pool every other invocation does — "concurrency settings apply to the
        // function as a whole, including all published versions and the unpublished version"
        // (`PutFunctionConcurrency`). It is admitted the way `Invoke`'s is, holds its slot
        // until the run ends however it ends, and a request the pool has no room for is
        // answered as the throttle it is with nothing run
        // (`crate::state::execution_slot`).
        let _slot = match crate::state::service_state(&*self.engine).map(|state| {
            crate::state::admit_invocation(
                &state,
                &prepared.invocation.function_name,
                &prepared.qualifier,
                ::std::time::SystemTime::now(),
            )
        }) {
            Some(Ok(admitted)) => {
                prepared.invocation.initialization_type = admitted.initialization_type;
                // Admitting the invocation raised the function's concurrency, which is
                // published as one reading of that gauge exactly as on `Invoke`
                // (`crate::state::concurrency_metering`).
                crate::state::publish_function_concurrency(
                    &*self.engine,
                    &prepared.invocation.function_name,
                    admitted.concurrent_executions,
                    ::std::time::SystemTime::now(),
                );
                // An invocation of a qualifier with provisioned concurrency is metered on the
                // provisioned-concurrency family for what the admission found, exactly as on
                // `Invoke` (`crate::state::metrics`).
                crate::state::publish_provisioned_concurrency(
                    &*self.engine,
                    &prepared.metered,
                    &admitted.provisioned,
                );
                Some(admitted.slot)
            }
            Some(Err(throttled_by)) => {
                ::tracing::info!(
                    request_id = %request_id,
                    function = %prepared.invocation.function_name,
                    reason = %throttled_by.reason,
                    "a function URL request found the function's concurrency pool full, so \
                     nothing ran"
                );
                // Answered the throttle, so metered as one request the service throttled,
                // exactly as on `Invoke` (`crate::state::metrics`).
                crate::state::publish_throttle(&*self.engine, &prepared.metered);
                return throttle_response(throttled_by.reason, &throttled_by.message);
            }
            None => None,
        };
        // The invocation is admitted to the function at this point, so it is metered as one
        // `Invocations` datapoint of the function, of the resource the URL addressed and of the
        // version that served it, before the run is attempted: the count is of invocation
        // requests that reached the function, and says nothing about what the execution
        // produced.
        crate::state::publish_invocation(&*self.engine, &prepared.metered);
        // A function URL request starts no durable execution: the endpoint invokes the
        // function, so the run it makes is no execution's run and reports no step onto one.
        let invoked = match crate::state::run_invocation(prepared.invocation, None).await {
            // No run slot of the substrate came free inside this request's whole wait: the
            // capacity that was full is the one a function with no reservation draws on, so
            // the request is answered as the throttle it is rather than as a gateway failure
            // of a run that was never attempted.
            Err(crate::state::Unrunnable::Overloaded(message)) => {
                ::tracing::warn!(
                    request_id = %request_id,
                    "a function URL request found no capacity to run the function, so nothing \
                     ran"
                );
                return throttle_response(
                    crate::state::execution_slot::ACCOUNT_CONCURRENCY_REASON,
                    &message,
                );
            }
            Ok(invoked) => invoked,
            Err(why) => {
                ::tracing::warn!(
                    detail = ?why,
                    "a function URL request could not be run by this emulator"
                );
                return plane_error(NOT_RUN_STATUS, &unrunnable_reason(&why));
            }
        };
        // "An unhandled error" is the function's own failure, which a function URL does not
        // pass through: the caller is told the gateway got no usable response, and the error
        // stays in the run's log.
        //
        // A RESPONSE-STREAMING answer is the exception, and only once bytes have been
        // WRITTEN: a stream that has been begun cannot be withdrawn, so the response stands
        // and the failure that cut it short leaves the caller a body that stops where the
        // handler stopped. A stream that was never begun is no response at all — whether the
        // handler failed before its first write or returned having written nothing — so it is
        // the same gateway answer every unusable response is, and never an empty success.
        let streamed = served_as_a_stream(target);
        let began = !invoked.chunks.is_empty();
        if (invoked.function_error.is_some() && !(streamed && began)) || (streamed && !began) {
            ::tracing::info!(
                request_id = %request_id,
                "a function URL invocation produced no usable response, answered as a bad \
                 gateway"
            );
            return bad_gateway(&request_id);
        }
        let mapped = if streamed {
            mapped_stream(&invoked.payload)
        } else {
            mapped_result(&invoked.payload)
        };
        match mapped {
            Mapped::Response {
                status,
                headers,
                body,
            } => served(status, &headers, body, &request_id),
            Mapped::Malformed(why) => {
                ::tracing::info!(
                    request_id = %request_id,
                    detail = %why,
                    "a function URL response describes no response, answered as a bad gateway"
                );
                bad_gateway(&request_id)
            }
        }
    }

    /// Everything the run needs, read out of the store under one guard: the invocation
    /// itself, the resource the metering is charged to, and the key the function's
    /// environment variables are encrypted with.
    ///
    /// The guard lives in this call and no longer — the run below it happens with no guard
    /// held, so one caller's handler cannot stall another caller's request.
    fn prepare(
        &self,
        target: &crate::state::FunctionUrlTarget,
        incoming: &Incoming<'_>,
        ctx: &OperationContext<'_>,
    ) -> Result<Prepared, Box<RtResponse>> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(Box::new(plane_error(
                NOT_RUN_STATUS,
                "Lambda state is not reachable on this engine",
            )));
        };
        let now = incoming.received;
        let request_id = incoming.request_id;
        let mut store = state.lock();
        // A function URL is attached to the unpublished version or to an ALIAS, and the
        // alias names the version it invokes AT THE TIME OF THE CALL, so the version is
        // resolved here, on every request, by the one resolution every invoking path shares.
        let qualifier = (target.qualifier != crate::state::VERSION_LATEST)
            .then(|| target.qualifier.clone());
        let Ok((record, version)) = crate::state::resolve_function(
            &mut store,
            &target.function_name,
            qualifier.as_deref(),
            // The region the caller ADDRESSED, which the published endpoint host carries;
            // an addressing that carries none resolves under this service's default.
            target
                .addressed_region
                .as_deref()
                .unwrap_or(crate::state::DEFAULT_REGION),
            now,
        ) else {
            return Err(Box::new(plane_error(
                NOT_FOUND_STATUS,
                "The function this URL is attached to does not exist.",
            )));
        };
        // "During this time, you can't invoke or modify the function" (`CreateFunction`): a
        // request that arrives inside the provisioning window reaches no instance of the
        // function, so the URL has no response to serve.
        if record.state != crate::state::STATE_ACTIVE {
            ::tracing::info!(
                function = %target.function_name,
                state = %record.state,
                "a function URL request reached a function that cannot be invoked yet"
            );
            return Err(Box::new(bad_gateway(request_id)));
        }
        // Lambda's recursive-loop guardrail decides whether this invocation happens at all,
        // exactly as it does on `Invoke`: an invocation through a function URL is an
        // invocation of the function.
        let chain = crate::state::request_chain(ctx);
        if crate::state::guardrail(record, chain.as_deref(), now) == crate::state::Guardrail::Stopped
        {
            let function_name = record.config.name.clone();
            drop(store);
            crate::state::publish_recursive_invocation_dropped(&*self.engine, &function_name);
            return Err(Box::new(bad_gateway(request_id)));
        }
        let function_name = record.config.name.clone();
        // The version this invocation resolved to: a URL attached to an alias that splits its
        // invocations between two versions resolves per invocation
        // (`crate::state::alias_routing`), so the configuration and the package the run uses
        // are the ones of the version it was routed to.
        let executed_version = record
            .routed_version(&version)
            .unwrap_or_else(|| version.clone());
        let resolved_config = record
            .config_of(&executed_version)
            .cloned()
            .unwrap_or_else(|| record.config.clone());
        // What every series about this invocation is reported for: the function, the resource
        // the URL addressed and the version that served it (`crate::state::metrics`).
        let metered = crate::state::InvokedTarget::new(&function_name, &version, &executed_version);
        // The X-Ray trace context this invocation runs with: the chain the client named on its
        // own request, under the rule the function's `TracingConfig` mode gives
        // (`crate::state::execution::trace_context`). Resolved BEFORE the event is built,
        // because the handler is told about that chain among the request's headers as well as
        // in its environment, and the two are one value.
        let trace = crate::state::invocation_trace_context(
            crate::state::supplied_trace_header(ctx).as_deref(),
            &resolved_config.tracing_mode,
            ::std::time::SystemTime::now(),
        );
        let invocation = crate::state::Invocation {
            function_name: function_name.clone(),
            executed_version: executed_version.clone(),
            invoked_function_arn: record.versioned_arn(&version),
            region: record.region.clone(),
            runtime: resolved_config.runtime.clone(),
            handler: resolved_config.handler.clone(),
            code_revision: record
                .serving_revision_of(&executed_version, &resolved_config.revision_id),
            variables: resolved_config.environment.clone(),
            memory_size: resolved_config.memory_size,
            ephemeral_storage_mb: resolved_config.ephemeral_storage_mb,
            timeout: resolved_config.timeout,
            architecture: resolved_config
                .architectures
                .first()
                .cloned()
                .unwrap_or_default(),
            zip_bytes: record
                .serving_code_of(&executed_version)
                .zip_bytes
                .clone(),
            layer_zips: crate::state::layer_archives(&resolved_config.layers),
            payload: self.event_for(target, incoming, ctx, &trace.header),
            // A function URL request carries no `ClientContext` and no `TenantId`: both are
            // members of the `Invoke` API, which this plane is not.
            client_context: None,
            tenant_id: None,
            request_id: request_id.to_string(),
            trace: Some(trace),
            // Which kind of environment serves it is the admission's to decide, once this
            // store guard is released (`crate::state::execution_slot`).
            initialization_type: crate::state::execution_slot::INITIALIZATION_TYPE_ON_DEMAND,
            // The AWS SDK this runtime supplies the handler reaches the rest of this
            // emulator through the callback the framework handed THIS request, signing
            // every call the handler makes as the session of the function's own execution
            // role (`crate::state::execution_role_session`).
            aws: crate::state::function_sdk_access_in_vpc(
                &*self.engine,
                ctx,
                &record.region,
                &resolved_config.role,
                &function_name,
                &record.vpc_network_interface_ids,
            ),
            // Where the run's log is delivered, and what the function's execution role may
            // do there (`crate::state::execution::log_delivery`): an invocation through a
            // function URL is an invocation of the function, so it logs like one.
            logs: Some(crate::state::log_target(
                &*self.engine,
                &record.region,
                &resolved_config,
            )),
            // What this invocation is metered by once it has run, exactly as on `Invoke`:
            // the time the run took and the failure it came to
            // (`crate::state::metrics`).
            metrics: Some(crate::state::InvocationMetrics::new(&*self.engine, &metered)),
        };
        Ok(Prepared {
            metered,
            qualifier: version,
            kms_key_arn: resolved_config.kms_key_arn.clone(),
            invocation,
        })
    }

    /// The payload-format-2.0 event this request becomes, for an invocation running under the
    /// trace context `trace_header` names.
    fn event_for(
        &self,
        target: &crate::state::FunctionUrlTarget,
        incoming: &Incoming<'_>,
        ctx: &OperationContext<'_>,
        trace_header: &str,
    ) -> Vec<u8> {
        let req = incoming.req;
        // The host the handler is told about is the function URL's OWN — the HOST of the
        // address this service PUBLISHED for it, which is the address a client was handed
        // and the one `requestContext.domainName` names — rather than the authority the
        // request happened to reach this process at. A handler reconstructs its own URL
        // from that header, enforces a host allow-list with it and signs callbacks from it,
        // so what it has to name is the endpoint and not the socket in between; and it is
        // that address's host alone, because a domain name carries no transport port
        // (`crate::state::function_url`).
        let domain_name = &target.endpoint_host;
        let headers: Vec<(String, String)> = req
            .headers()
            .iter()
            .map(|(name, value)| {
                if name.eq_ignore_ascii_case(HOST_HEADER) {
                    return (name.to_string(), domain_name.clone());
                }
                (name.to_string(), value.to_string())
            })
            .collect();
        let content_type = req.headers().get("content-type").map(str::to_string);
        // The address the call is attributed to is the one its CONNECTION came from: a
        // client's own `X-Forwarded-For` travels on to the handler verbatim among the
        // headers, and decides nothing here.
        let source_ip = connection_address();
        let request = UrlRequest {
            method: req.method(),
            raw_path: &target.request_path,
            raw_query: raw_query(req.uri()),
            headers: &headers,
            body: incoming.body,
            content_type: content_type.as_deref(),
            url_id: &target.url_id,
            domain_name,
            account_id: ::engine::emulator_account_id(),
            request_id: incoming.request_id,
            source_ip: &source_ip,
            trace_header,
            received: incoming.received,
            iam: self.caller(ctx),
        };
        url_event(&request)
    }

    /// Whether this endpoint admits the caller this request speaks for, and the REASON IAM
    /// withheld the permission when it does not.
    ///
    /// The decision is IAM's, taken by the one gate every invocation of this service goes
    /// through ([`crate::state::invoke_authorization::admits_function_url_request`]) under the
    /// endpoint's own action. WHO asks is the mounting's to say rather than this gate's, and
    /// it is the same fact the framework decided the request's authentication by ([`Admits`]):
    /// the mounting serving an ANONYMOUS caller declares no auth scheme, so nothing
    /// authenticated it and it speaks for nobody — only a grant to the wildcard principal can
    /// name it, which is what makes a `NONE` URL public exactly while its function's policy
    /// grants the invocation to everyone ("`NONE` – Bypass IAM authentication to create a
    /// public endpoint"). The mounting serving a credentialled caller declares the SigV4
    /// scheme, so the emulator VERIFIED that credential before this plane ran, and what the
    /// principal it speaks for may do is the union of its own policies and the function's.
    ///
    /// A URL whose `AuthType` is `AWS_IAM` carries its admission in that same verification
    /// ([`Admits::SignedRestricted`]), so this gate is asked for the two `NONE` mountings
    /// ([`Admits::serves_a_public_url`]).
    ///
    /// Taken with NO guard of this service's store held: the policy is read under that store's
    /// own lock.
    fn admits_the_caller(
        &self,
        target: &crate::state::FunctionUrlTarget,
        ctx: &OperationContext<'_>,
    ) -> Result<(), String> {
        crate::state::invoke_authorization::admits_function_url_request(
            &*self.engine,
            ctx,
            // The resource the decision is about is the ARN of the function AT the qualifier
            // this endpoint is attached to, read off the record that holds the URL: the
            // address a request arrived at names no Region of its own for a client reaching
            // the emulator's own endpoint, while the function it reaches has one.
            &target.resource_arn,
            &target.function_name,
            &target.qualifier,
            &target.auth_type,
            self.admits.is_anonymous(),
        )
    }

    /// The caller a request was signed by, for the mounting that serves `AWS_IAM` URLs.
    ///
    /// The signature itself was verified by the emulator before this plane ran, so what is
    /// reported here is the identity THAT credential speaks for: the access key id the
    /// request presented (the public half, never a secret) and the account's own principal,
    /// which is the one long-term credential this emulator publishes
    /// ([`service_common::caller_principal`]). A `NONE` URL reports no caller at all,
    /// whichever of its two mountings served the request, because the member the event
    /// carries it in is the `AWS_IAM` authorizer's.
    fn caller(&self, ctx: &OperationContext<'_>) -> Option<CallerIdentity> {
        if self.admits != Admits::SignedRestricted {
            return None;
        }
        Some(CallerIdentity {
            access_key: ::engine::extract_akid(&ctx.parts().headers),
            account_id: ::engine::emulator_account_id().to_string(),
            user_arn: ::service_common::caller_principal::ambient_user_arn(),
            user_id: ::service_common::caller_principal::ambient_user_id(),
        })
    }
}

/// The `AuthType` whose URLs are served to authenticated callers only.
const AUTH_TYPE_AWS_IAM: &str = "AWS_IAM";

/// The `InvokeMode` whose URLs answer the bytes the handler WROTE rather than the response a
/// value it returned describes: "RESPONSE_STREAM – Your function streams payload results as
/// they become available".
const INVOKE_MODE_RESPONSE_STREAM: &str = "RESPONSE_STREAM";

/// Whether the URL `target` SERVES IN the response-streaming mode, which is the mode stored
/// on it or the documented default when none ever was
/// (`crate::state::FunctionUrlConfigRecord::served_invoke_mode`).
fn served_as_a_stream(target: &crate::state::FunctionUrlTarget) -> bool {
    target.invoke_mode == INVOKE_MODE_RESPONSE_STREAM
}

/// The status a request the function's concurrency pool had no room for is answered with:
/// the throttle AWS answers an invocation past a concurrency limit with.
const THROTTLED_STATUS: u16 = 429;

/// The status a request addressing a function URL this store no longer holds is answered
/// with: the endpoint names nothing.
const NOT_FOUND_STATUS: u16 = 404;

/// One request as the plane read it off the wire: the request itself, the bytes of its body
/// (read once), the id this request and its invocation share, and the instant it arrived —
/// so one request has ONE instant and one reading of its body however many steps serve it.
struct Incoming<'r> {
    /// The request the plane was handed.
    req: &'r RtRequest,
    /// Its body's bytes.
    body: &'r [u8],
    /// This request's id.
    request_id: &'r str,
    /// When it arrived.
    received: ::std::time::SystemTime,
}

/// Everything one function URL request needs once the store's guard is released.
struct Prepared {
    /// The invocation to run.
    invocation: crate::state::Invocation,
    /// What the series about this invocation are reported for: the function, the resource the
    /// URL addressed and the version that served it.
    metered: crate::state::InvokedTarget,
    /// The version label the URL addressed — the alias the URL is attached to, or the
    /// unpublished version — which the invocation is admitted against.
    qualifier: String,
    /// The key the function's environment variables are encrypted with, when it has one.
    kms_key_arn: Option<String>,
}

impl<Eng: Send + Sync + 'static> ErasedOperation for FunctionUrlPlane<Eng> {
    fn run<'r>(
        &'r self,
        _svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let response = self.serve(req, ctx).await;
            ::tracing::info!(
                service = ctx.identity().signing_name(),
                access_key_id = ::engine::extract_akid(&ctx.parts().headers),
                status = response.status().as_u16(),
                "served a function URL request"
            );
            response
        })
    }

    /// The schemes this mounting's callers sign with: a URL whose `AuthType` is `AWS_IAM` is
    /// served only to a request the emulator verified a SigV4 signature for, and so is a
    /// request that PRESENTS a credential to a URL whose `AuthType` is `NONE` — the caller
    /// offered an identity, so the emulator holds it to it. A `NONE` URL reached with no
    /// credential at all is "a public endpoint", served however it arrived.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        match self.admits {
            Admits::SignedRestricted | Admits::SignedPublic => Some(&[SIGV4_SCHEME]),
            Admits::Anonymous => Some(&[]),
        }
    }
}

/// The Smithy auth scheme a SigV4-signed request presents.
const SIGV4_SCHEME: &str = "aws.auth#sigv4";

/// The query a request carries, without its `?`, and empty for a request carrying none.
fn raw_query(uri: &str) -> &str {
    uri.split_once('?').map_or("", |(_, query)| query)
}

/// The header a request names the host it addressed in, which the event reports as the
/// endpoint's own ([`FunctionUrlPlane::event_for`]).
const HOST_HEADER: &str = "host";

/// One served response: the status, headers and body the handler's result described.
///
/// The headers are appended through the NON-panicking appender: a header the mapping already
/// held to what a response can carry ([`event::mapped_result`]) cannot fail here, and a
/// response this one could not carry is answered as the unusable response it is rather than
/// by a panic on the request's path.
fn served(
    status: u16,
    headers: &[(String, String)],
    body: Vec<u8>,
    request_id: &str,
) -> RtResponse {
    let mut response = RtResponse::new(plane_status(status), SdkBody::from(body));
    for (name, value) in headers {
        // A header the result names more than once — `set-cookie`, one per cookie — is
        // answered once per value, so every cookie the function set reaches the client.
        if response
            .headers_mut()
            .try_append(name.clone(), value.clone())
            .is_err()
        {
            ::tracing::info!(
                request_id = %request_id,
                "a function URL response names a header no response can carry, answered as a \
                 bad gateway"
            );
            return bad_gateway(request_id);
        }
    }
    response
        .headers_mut()
        .insert(REQUEST_ID_HEADER, request_id.to_string());
    response
}

/// The `502` every class of unusable response is answered with: AWS's own sentence, and
/// nothing of the function's failure.
///
/// The sentence is the WHOLE body, unwrapped: a gateway answer is not one of this API's
/// failures, so it carries neither an envelope member nor an error class naming a modelled
/// shape — only the status, the request id and the sentence.
fn bad_gateway(request_id: &str) -> RtResponse {
    let mut response = RtResponse::new(
        plane_status(BAD_GATEWAY_STATUS),
        SdkBody::from(BAD_GATEWAY_MESSAGE),
    );
    let headers = response.headers_mut();
    headers.insert("content-type", event::DEFAULT_CONTENT_TYPE);
    headers.insert(REQUEST_ID_HEADER, request_id.to_string());
    response
}

/// The `429` an invocation the function's concurrency had no room for is answered with: the
/// throttle document Lambda words for it.
///
/// A function URL request IS an invocation of the function, so what it is refused with is
/// `TooManyRequestsException` — the sentence of the throttle, the `Reason` naming WHICH
/// capacity was full (`crate::state::execution_slot`, the same reason `Invoke` reports) and
/// the class of fault a quota the caller controls makes it
/// ([`super::fault_class::USER_FAULT`]) — so a client can tell which pool it has to wait for.
fn throttle_response(reason: &str, message: &str) -> RtResponse {
    let body = ::serde_json::json!({
        "message": message,
        "Reason": reason,
        "Type": super::fault_class::USER_FAULT,
    })
    .to_string();
    let mut response = RtResponse::new(plane_status(THROTTLED_STATUS), SdkBody::from(body));
    let headers = response.headers_mut();
    headers.insert("content-type", event::DEFAULT_CONTENT_TYPE);
    if let Some(error_type) = error_type_of(THROTTLED_STATUS) {
        headers.insert(ERROR_TYPE_HEADER, error_type);
    }
    response
}

/// One refusal of a function URL request, in the `{"message": …}` envelope a function URL
/// answers with, naming the CLASS of the failure in [`ERROR_TYPE_HEADER`].
///
/// The endpoint serves no modelled operation, so there is no modelled error document to
/// render — but the failure still belongs to one of this API's error classes
/// ([`error_type_of`]), and that class is what a client branches on, so it is named here
/// exactly as every other answer of this service names the shape its failure is.
fn plane_error(status: u16, message: &str) -> RtResponse {
    let body = ::serde_json::json!({ "message": message }).to_string();
    let mut response = RtResponse::new(plane_status(status), SdkBody::from(body));
    response
        .headers_mut()
        .insert("content-type", event::DEFAULT_CONTENT_TYPE);
    if let Some(error_type) = error_type_of(status) {
        response.headers_mut().insert(ERROR_TYPE_HEADER, error_type);
    }
    response
}

/// The modelled exception a failure this plane answers with `status` belongs to, or `None`
/// for a status that is not one of this API's failures at all.
///
/// `Invoke` — which is what a function URL request IS an invocation through — models each
/// of these classes against exactly this status
/// (`smithy_query(query="operation", operation="Invoke")`: `TooManyRequestsException`
/// `httpError` 429, `ResourceNotFoundException` 404, `RequestTooLargeException` 413,
/// `ServiceException` 500), so the class a caller is told is the one the model gives that
/// refusal. A `502` names none: the gateway's own answer stands for no modelled failure of
/// this API.
fn error_type_of(status: u16) -> Option<&'static str> {
    match status {
        FORBIDDEN_STATUS => Some(FORBIDDEN_ERROR_TYPE),
        THROTTLED_STATUS => Some("TooManyRequestsException"),
        NOT_FOUND_STATUS => Some("ResourceNotFoundException"),
        TOO_LARGE_STATUS => Some("RequestTooLargeException"),
        NOT_RUN_STATUS => Some("ServiceException"),
        _ => None,
    }
}

/// The class of a refusal of a caller whose SIGNATURE is the reason it was not admitted —
/// a signature that does not verify, and one scoped to another service or Region.
pub(super) const INVALID_SIGNATURE_EXCEPTION: &str = "InvalidSignatureException";

/// The class of a refusal of a caller whose credential this emulator does not know.
pub(super) const UNRECOGNIZED_CLIENT_EXCEPTION: &str = "UnrecognizedClientException";

/// The refusal the URL's OWN AUTHORIZER gives a caller on an address no configuration sits
/// behind: a URL whose configuration was deleted, and one that never had any.
///
/// The same status as every other refusal of this endpoint, with the member AWS answers it
/// carrying no sentence: a caller is told the request was not admitted and nothing about
/// whether a URL is configured at that address. The null member is the recorded answer of a
/// deleted function URL (`lambda.url-deleted-config-stops-serving-the-url`, whose check
/// asserts the body `{"Message": null}` exactly); it is not the wording of the refusal of a
/// caller a POLICY does not admit, which is [`forbidden`]'s.
pub(super) fn unauthorized() -> RtResponse {
    refusal_document(
        ::serde_json::json!({ FORBIDDEN_MEMBER: ::serde_json::Value::Null }),
        FORBIDDEN_ERROR_TYPE,
    )
}

/// The refusal a caller is given when its SIGNATURE is why it was not admitted: the class of
/// that failure and the sentence AWS words for it, which is where a client reads what about
/// the signature was wrong.
pub(super) fn signature_refused(error_type: &str, message: &str) -> RtResponse {
    refusal_document(::serde_json::json!({ "message": message }), error_type)
}

/// One `403` of this endpoint carrying a document of its own: the body it answers, the class
/// that body belongs to, and the id of the request it refused — the same two headers
/// [`forbidden`] names a refusal with.
fn refusal_document(body: ::serde_json::Value, error_type: &str) -> RtResponse {
    let mut response = RtResponse::new(
        plane_status(FORBIDDEN_STATUS),
        SdkBody::from(body.to_string()),
    );
    let headers = response.headers_mut();
    headers.insert("content-type", event::DEFAULT_CONTENT_TYPE);
    headers.insert(ERROR_TYPE_HEADER, error_type.to_string());
    headers.insert(REQUEST_ID_HEADER, crate::state::mint_request_id());
    response
}

/// The refusal the URL's OWN AUTHORIZER gives a caller the authorization decision does not
/// admit: the whole fixed document [`AUTHORIZER_FORBIDDEN_MESSAGE`], naming the class of the
/// refusal and the request it refused.
///
/// The member is spelled `Message` here and `message` on the failures above because that is
/// how the two are answered — the authorization refusal of a Lambda front door carries the
/// capitalised member and a sentence that is fixed WHOLE, with nothing of the request, the
/// caller or the policy named in it: the authorizer says that the request was not authorized
/// and where an operator reads up on why, and never which grant was missing.
///
/// Two headers name the refusal, exactly as a served answer names the request it answered
/// ([`served`]): the CLASS of the failure, which for a caller no policy admits is the common
/// AWS access error ([`FORBIDDEN_ERROR_TYPE`]) — the same header this crate's download plane
/// names a refusal's class in — and the id of the request it refused, so a client correlating
/// on it has an id for a refused call as much as for a served one. `request_id` is the id the
/// plane minted for the request; a refusal taken before a request reached the plane names a
/// fresh one, because a front door assigns one to every request it answers.
///
/// It is the ONE wording of every AUTHORIZATION refusal this plane takes on the function's
/// own policy ([`FunctionUrlPlane::admits_the_caller`]). A caller the emulator did not
/// AUTHENTICATE never reached an authorization decision at all, so it reads the front door's
/// own shorter refusal instead ([`unauthenticated`]), and a caller whose SIGNATURE is what
/// did not hold is told that much beside it ([`signature_refused`]).
pub(super) fn forbidden(request_id: Option<&str>) -> RtResponse {
    access_denied(request_id, AUTHORIZER_FORBIDDEN_MESSAGE)
}

/// The refusal a caller the emulator did not AUTHENTICATE reads, through the seam that words
/// a refusal taken before this plane could run ([`super::function_url_refusal`]): the front
/// door's own [`FORBIDDEN_MESSAGE`], which names nothing of the request and, unlike the
/// authorizer's above, points at no troubleshooting — no authorization decision was reached
/// for a request that presented no credential the emulator could judge.
pub(super) fn unauthenticated() -> RtResponse {
    access_denied(None, FORBIDDEN_MESSAGE)
}

/// One `403` of this endpoint that is a decision about WHO the caller is: the sentence it
/// carries, and the class every such refusal belongs to ([`FORBIDDEN_ERROR_TYPE`]).
fn access_denied(request_id: Option<&str>, message: &str) -> RtResponse {
    let mut response = refused(request_id, message);
    response
        .headers_mut()
        .insert(ERROR_TYPE_HEADER, FORBIDDEN_ERROR_TYPE);
    response
}

/// The refusal the endpoint gives a PREFLIGHT whose origin or method the URL's own `Cors`
/// settings do not admit ([`cors`]): the front door's own `403` sentence, naming the request
/// it refused and NOT the access error above — a preflight the configuration does not cover
/// is the CORS layer's own answer rather than a decision about who the caller is, so it
/// carries neither that class nor the authorizer's pointer at authorization troubleshooting.
pub(super) fn refused_preflight() -> RtResponse {
    refused(None, FORBIDDEN_MESSAGE)
}

/// The `403` body `message` is answered in, and the request id every refusal carries.
fn refused(request_id: Option<&str>, message: &str) -> RtResponse {
    let body = ::serde_json::json!({ FORBIDDEN_MEMBER: message }).to_string();
    let mut response = RtResponse::new(plane_status(FORBIDDEN_STATUS), SdkBody::from(body));
    let named = request_id.map_or_else(crate::state::mint_request_id, str::to_string);
    response
        .headers_mut()
        .insert("content-type", event::DEFAULT_CONTENT_TYPE);
    response.headers_mut().insert(REQUEST_ID_HEADER, named);
    response
}

/// The member that refusal carries its sentence in.
const FORBIDDEN_MEMBER: &str = "Message";

/// The header a refusal of this endpoint names the class of its failure in.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The class a caller the endpoint does not admit is refused under: the common AWS access
/// error, which is the failure an authorization decision that admitted nobody is.
const FORBIDDEN_ERROR_TYPE: &str = "AccessDeniedException";

/// The status and the sentence a caller the endpoint's own front door turns away is refused
/// with, before any authorization decision is reached for it.
const FORBIDDEN_STATUS: u16 = 403;
/// See [`FORBIDDEN_STATUS`].
const FORBIDDEN_MESSAGE: &str = "Forbidden";

/// The whole sentence the URL's own AUTHORIZER refuses a caller the authorization decision
/// does not admit with: the word above and the fixed troubleshooting pointer AWS appends to
/// it, which is where an operator reading a refused request's body is sent to find out why a
/// function URL did not admit a caller
/// (<https://docs.aws.amazon.com/lambda/latest/dg/urls-auth.html>, the page the sentence
/// names). Fixed WHOLE: no part of it is taken from the request, the caller or the policy
/// that did not admit it.
const AUTHORIZER_FORBIDDEN_MESSAGE: &str =
    "Forbidden. For troubleshooting Function URL authorization issues, see: \
     https://docs.aws.amazon.com/lambda/latest/dg/urls-auth.html";

/// A status this plane answers with. Every call site passes a constant or a status
/// [`mapped_result`] already held to the HTTP range, so the conversion cannot fail.
fn plane_status(status: u16) -> StatusCode {
    StatusCode::try_from(status).unwrap_or_else(|_| {
        StatusCode::try_from(BAD_GATEWAY_STATUS).expect("a constant status is a valid one")
    })
}

/// Why the function a function URL request addressed never ran.
///
/// The sentences are the ONE set this crate has for a run that could not be made
/// ([`crate::state::async_invocation::unrunnable_reason`], which the asynchronous queue
/// records on its own entry and which a durable execution fails with); only the SUBJECT
/// differs, which is this request rather than a queued event.
fn unrunnable_reason(why: &crate::state::Unrunnable) -> String {
    crate::state::async_invocation::unrunnable_reason(why, THIS_REQUEST)
}

/// What the reason sentences above call the work that never ran, here.
const THIS_REQUEST: &str = "this request";
