// SPDX-License-Identifier: CC0-1.0

//! The split between AUTHENTICATING a request and AUTHORIZING it, for the one caller of
//! Amazon S3 that presents no credential at all: the ANONYMOUS principal.
//!
//! "If you grant READ access to the anonymous user, the `GetObject` operation returns the
//! object without using an authorization header" (`GetObject`'s own documentation). So a
//! request carrying neither an `Authorization` header nor a presigned credential is not a
//! request that FAILED to authenticate: it is a request that speaks for the anonymous
//! principal, which S3 then authorizes like any other — against the addressed resource's own
//! ACL, the bucket policy, and the Block Public Access configuration in force
//! ([`crate::state::Bucket::anonymous_object_read_refusal`] and
//! [`crate::state::Bucket::anonymous_bucket_read_refusal`] are the two readings of that
//! rule). What it may not do is refused `403 AccessDenied` by that decision, not by the
//! signature boundary — which is the difference between a bucket whose Block Public Access
//! settings can be SHOWN to block public access and one where nothing public ever worked.
//!
//! ## Why this is a module rather than a line in a handler
//!
//! Whether a request is authenticated at all is decided before any handler runs: the
//! framework reads the auth schemes of the operation the request names and refuses the
//! request when no registered authenticator recognizes it, which no unsigned request does.
//! The schemes it reads are the MODEL's for that operation ("an operation's own schemes count
//! only when the model names none for the request"), and this model names SigV4 on every
//! operation — the model has no way to say "and also the anonymous principal, whom the
//! resource's own policy judges".
//!
//! Two pieces answer that, and both are needed because the seam that states an operation's
//! schemes ([`::smithy_server_runtime::ErasedOperation::auth_schemes`]) is handed no request:
//!
//! 1. [`ServedUnsigned`], the wrapper the operations of this service that DO judge the
//!    anonymous principal are served behind ([`served_to_the_anonymous_caller`] is the
//!    catalog). It states no auth scheme for a request that presents no credential — so such
//!    a request reaches the handler, whose own authorization then answers it — and the
//!    operation's own modelled schemes for every request that presents one, so a credentialed
//!    request is verified exactly as before. Nothing is waved through: the handler behind it
//!    authorizes the anonymous caller against what the bucket grants to everyone.
//! 2. [`remember`], which records WHICH surface the request being served is on, read once per
//!    request by this service's routing seam (the seam the framework reaches with the whole
//!    request, before it authenticates) and answered by [`admission`]. It is a request-scoped
//!    note rather than state, exactly like the note a refusal's wording is read from
//!    ([`super::sigv4`]): the framework calls both within one synchronous stretch of the one
//!    dispatch.
//!
//! Every operation NOT in the catalog keeps the refusal an unsigned request earns today: it
//! states the model's SigV4 scheme whatever the request presented, so the signature boundary
//! refuses the unsigned caller with S3's own `AccessDenied` ([`super::sigv4`] words it) before
//! any handler runs. That is the conservative half of the split — a handler that does not ask
//! who is calling must never be handed a caller it would read as the bucket owner.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};
use ::std::cell::Cell;

/// WHICH surface of this service the request being served is on, as far as authenticating it
/// goes — the note [`remember`] records and [`admission`] answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Admission {
    /// The request presents a credential — an `Authorization` header or a presigned query —
    /// so it is the caller that credential names and the framework verifies its signature
    /// under the schemes the model gives the operation it addressed.
    Credentialed,
    /// The request presents none: the ANONYMOUS principal, served what the bucket grants to
    /// everyone by the operations of this service that judge that caller.
    Anonymous,
    /// The request is addressed to a bucket's WEBSITE endpoint, a surface the model names no
    /// operation on at all — every request of it is the anonymous principal's, whatever
    /// credential it happens to carry, because a website endpoint reads none.
    WebsiteEndpoint,
}

thread_local! {
    /// Which surface the request this thread is serving reached, for the length of that one
    /// request.
    ///
    /// Not state: a request-scoped note, needed because the seam that states an operation's
    /// auth schemes is handed no request. It is recorded for EVERY request this service
    /// routes, so it can never be read as a previous request's.
    static ADMISSION: Cell<Admission> = const { Cell::new(Admission::Credentialed) };
}

/// Record that the request this thread is serving reached `admission`'s surface.
pub(super) fn remember(admission: Admission) {
    ADMISSION.with(|note| note.set(admission));
}

/// The surface [`remember`] recorded for the request this thread is serving.
pub(super) fn admission() -> Admission {
    ADMISSION.with(Cell::get)
}

/// Whether `request` presents NO credential at all — the ANONYMOUS caller.
///
/// The one reading of that question ([`crate::state::presents_no_credential`]), asked of the
/// runtime-API request this service's routing seam is handed — so the seam that admits the
/// anonymous caller, the wording of a refusal and the handler that authorizes it cannot
/// disagree about which requests those are.
pub(super) fn presents_no_credential(request: &RtRequest) -> bool {
    crate::state::presents_no_credential(
        request
            .headers()
            .get(::http::header::AUTHORIZATION.as_str()),
        super::wire::request_query(request.uri()),
    )
}

/// The operations of this service that judge the ANONYMOUS principal themselves, keyed by the
/// modelled name the protocol resolves for each.
///
/// Exactly the reads whose authorization this service decides against what the bucket grants
/// to EVERYONE, and no others:
///
/// * the object reads — `GetObject` and `HeadObject`, authorized under `s3:GetObject` ("a
///   `HEAD` request has the same options as a `GET` operation on an object"), which
///   [`crate::state::Bucket::anonymous_object_read_refusal`] answers;
/// * the bucket reads — `ListObjects`, `ListObjectsV2` and `HeadBucket`, authorized under
///   `s3:ListBucket` ("you must have READ access to the bucket"), which
///   [`crate::state::Bucket::anonymous_bucket_read_refusal`] answers.
///
/// An operation outside this catalog keeps the refusal the signature boundary gives an
/// unsigned request: its handler resolves the calling principal from the credential the
/// request presents, and a request presenting none must not be read as the account that owns
/// the bucket.
pub(super) const SERVED_TO_THE_ANONYMOUS_CALLER: [&str; 5] = [
    "GetObject",
    "HeadObject",
    "ListObjects",
    "ListObjectsV2",
    "HeadBucket",
];

/// Whether the operation named `operation` is one this service serves the anonymous caller
/// ([`SERVED_TO_THE_ANONYMOUS_CALLER`]).
pub(super) fn served_to_the_anonymous_caller(operation: &str) -> bool {
    SERVED_TO_THE_ANONYMOUS_CALLER.contains(&operation)
}

/// One operation of this service, served to the ANONYMOUS caller as well as to the
/// credentialed one.
///
/// The operation behind it is untouched — it owns deserialize -> validate -> handle ->
/// serialize exactly as before, authorization included. What this wrapper adds is the one
/// thing an operation cannot say for itself: that a request presenting NO credential has
/// nothing for the signature boundary to verify, so it is admitted and judged by the handler's
/// own authorization instead of being refused for a signature it never carried.
pub(super) struct ServedUnsigned {
    served: Box<dyn ErasedOperation>,
}

impl ServedUnsigned {
    /// Serve `served` to the anonymous caller as well. Takes the operation already wrapped in
    /// whatever else this service serves it behind (its front door, its request counting, its
    /// data-event record), so one operation is served once however the caller is
    /// authenticated.
    pub(super) fn new(served: Box<dyn ErasedOperation>) -> Self {
        Self { served }
    }
}

impl ErasedOperation for ServedUnsigned {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        self.served.run(svc, req, ctx)
    }

    /// No scheme at all for a request that presents no credential, and the wrapped operation's
    /// own modelled schemes for every request that presents one.
    ///
    /// The anonymous caller signs nothing, so there is no signature to verify and no
    /// credential to resolve: what it may do is the authorization question the handler behind
    /// this wrapper answers from the bucket's own ACL, policy and Block Public Access
    /// settings. A request that DID present a credential is verified under the model's own
    /// schemes, so a wrong signature is still refused for the signature.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        match admission() {
            Admission::Credentialed => self.served.auth_schemes(),
            Admission::Anonymous | Admission::WebsiteEndpoint => Some(&[]),
        }
    }
}
