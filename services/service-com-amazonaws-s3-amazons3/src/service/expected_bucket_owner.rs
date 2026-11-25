// SPDX-License-Identifier: CC0-1.0

//! The refusal a request earns for the EXPECTED BUCKET OWNER it names, before anything
//! compares that value against an owner.
//!
//! `x-amz-expected-bucket-owner` (and, for a copy, `x-amz-source-expected-bucket-owner`)
//! carries an AWS account id: both bind the `AccountId` shape, a plain string the model puts
//! no pattern on (`smithy_query(query="operation",operation="CopyObject")`). A value that is
//! not an account id at all — anything but the twelve decimal digits one carries
//! ([`crate::state::is_account_id`]) — makes the PARAMETER malformed, so S3 refuses the
//! request for the value: [`crate::state::INVALID_BUCKET_OWNER_CODE`] at HTTP 400, naming the
//! rejected text, with no bucket read and no owner compared.
//!
//! The 403 `AccessDenied` is the other refusal, and it is reserved for a WELL-FORMED account
//! id that is not the owner of a bucket this emulator HOLDS
//! ([`super::authorization::expected_bucket_owner_refusal`] makes that one): ownership is
//! something only a bucket that resolved has, so a request naming a bucket that is not there
//! is answered about its ABSENCE by the operation it addressed, whatever account the header
//! carries — which is why that guard takes the bucket's existence as an argument. Keeping the two
//! apart is what a client branches on: a typo, an unexpanded template variable or an ARN
//! pasted where an account id belongs is the caller's own request to fix, where `AccessDenied`
//! sends an operator into a cross-account permissions investigation — and a retry layer
//! treats the two statuses differently.
//!
//! Every operation of this service that carries the header is refused the same way, so the
//! rule lives in ONE place: this operation, which
//! [`super::EmulatedService::override_operation`] routes such a request to instead of the
//! operation its address names — the same way the bucket-name refusal stands in front of
//! every operation that addresses a bucket ([`super::invalid_bucket_name`]). It is reached
//! before any handler, and so before the store, which is why the answer names nothing of the
//! bucket.
//!
//! It is the REQUEST that is judged, not the matched operation: the form of an account id is
//! one rule for the parameter, and almost every bucket and object operation of this model
//! binds it, so the guard reads the header wherever a request states one. The alternative —
//! consulting the matched operation's input shape — is not available at this seam, which
//! resolves a route and not a schema, and spelling out which operations bind the member would
//! put a hand-maintained list of a hundred operations beside the model, which is exactly what
//! drifts from it. The difference this costs is a request that states the header on one of the
//! few operations with no such member, which is refused for the value rather than ignored.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::protocol::ServerProtocol;
use smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

use super::wire::UnwrappedXmlRestXml;

/// The routing key this operation is held in the service's own table under — a key the
/// model's protocol never resolves, so no modelled request can reach it by name.
pub const REFUSE_EXPECTED_BUCKET_OWNER: &str = "@refuse-expected-bucket-owner";

/// The headers that carry an expected bucket owner, in the order a request's values are
/// judged in: the bucket a request ADDRESSES before the one it copies FROM.
///
/// Both members target `AccountId` — `ExpectedBucketOwner` at `x-amz-expected-bucket-owner`
/// on every bucket and object operation that has one, and `ExpectedSourceBucketOwner` at
/// `x-amz-source-expected-bucket-owner` on the copies
/// (`smithy_query(query="operation",operation="CopyObject")`) — so one rule judges both.
const EXPECTED_OWNER_HEADERS: [&str; 2] = [
    "x-amz-expected-bucket-owner",
    "x-amz-source-expected-bucket-owner",
];

/// The expected-owner value `req` carries that is not an AWS account id, or `None` when every
/// one it carries is (including a request carrying none at all).
///
/// Read off the request's headers rather than off a deserialized member, because this stands
/// in front of every operation: which member a header binds is the matched operation's
/// business, and the FORM of an account id is the same question for all of them.
#[must_use]
pub fn malformed_value(req: &RtRequest) -> Option<&str> {
    EXPECTED_OWNER_HEADERS
        .into_iter()
        .filter_map(|header| req.headers().get(header))
        .find(|value| !crate::state::is_account_id(value))
}

/// The operation that answers the refusal, holding the codec it renders the envelope with.
pub struct RefuseExpectedBucketOwner {
    /// This service's own protocol, built ONCE (the service's constructor builds this
    /// operation): the envelope a refusal of this service goes out in — the bare `<Error>`
    /// root, and the `x-amz-request-id` / `x-amz-id-2` pair S3 names every answer by.
    protocol: UnwrappedXmlRestXml,
}

impl RefuseExpectedBucketOwner {
    /// Build the refusal operation. INFALLIBLE, and it holds no state: what it answers is
    /// read from the request it is handed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            protocol: UnwrappedXmlRestXml::new(),
        }
    }

    /// The envelope naming `supplied`: the code S3 answers a malformed expected-owner
    /// parameter with, at that code's status, carrying the sentence that quotes the rejected
    /// value back ([`crate::state::invalid_bucket_owner_message`]).
    fn refusal(&self, supplied: &str) -> Option<RtResponse> {
        let refusal = crate::scaffolded::Unmodeled {
            code: crate::state::INVALID_BUCKET_OWNER_CODE,
            status: crate::state::INVALID_BUCKET_OWNER_STATUS,
            message: crate::state::invalid_bucket_owner_message(supplied),
        };
        self.protocol
            .serialize_error(
                &refusal,
                &crate::scaffolded::UNMODELED_SCHEMA,
                refusal.code,
                refusal.status,
            )
            .ok()
    }
}

impl Default for RefuseExpectedBucketOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl ErasedOperation for RefuseExpectedBucketOwner {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let _ = ctx;
            // The value refused, read the same way the seam that routed the request here read
            // it, so the text the envelope quotes is the text the request carried. A request
            // that carries no malformed value cannot be routed here; if one ever were, it
            // keeps the framework's own answer rather than claiming a refusal of nothing.
            match malformed_value(req).and_then(|supplied| self.refusal(supplied)) {
                Some(response) => response,
                None => svc.definition().routing_error_response(req),
            }
        })
    }
}
