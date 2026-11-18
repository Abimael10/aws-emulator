// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-customerprofiles-customerprofiles-20200815` service: how it claims requests and how it words the
//! failures the framework hands back.
//!
//! EDIT THIS FILE. It is emitted once with working defaults and is never
//! regenerated, so a choice made here survives adding another service or
//! re-running the generator.
//!
//! `EmulatedService` is what the router mounts (`EmulatedService::new(engine)`).
//! It wraps the generated [`ServiceDefinition`], which carries the model-derived half
//! of this service — its identity, its wire protocol, and its operation routing
//! table — built from the Smithy model at compile time. You do not edit the
//! entry; you edit the behavioral seams below.
//!
//! Six opt-in seams are yours to change; every one defaults to the framework's
//! own behaviour, so an unedited service still routes and answers correctly:
//!
//! - `can_handle` — which requests this service claims. The default matches the
//!   request's resolved route key against this service's signing name. Edit it
//!   for anything the model does not express: a signing name shared by several
//!   services, or routing that keys on the access-key id.
//! - `override_operation` — serve a request the model's routing key cannot name
//!   correctly. The default returns `None`, so every request resolves through the
//!   generated routing table. Return `Some(op)` to route one yourself.
//! - `override_refusal` — how this service words a request the framework refused
//!   before any handler ran. The default returns `None`, keeping the framework's
//!   wording. See the inline note on that method for how to reword.
//! - `override_protocol` — the wire protocol the modelled sequence is driven
//!   through. This service returns its own `restJson1` (`wire`), whose error
//!   document always reports the error shape's `Message` member.
//! - `override_routing_error` — how this service words a request that reached it
//!   but named no modeled operation. The default returns `None`, so the entry
//!   renders the protocol-correct default (query -> 400 InvalidAction, awsJson ->
//!   404, …). Return `Some(resp)` to answer in your own envelope if desired.
//! - `override_framework_error` — how this service words a transport-level
//!   failure the orchestrator detects up front (e.g. a malformed `aws-chunked`
//!   body). The default returns `None`, keeping the entry's protocol-correct
//!   rendering. Return `Some(resp)` to reword it if desired.
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.
//!
//! ## The upload-job file plane
//!
//! `GetUploadJobPath` reports the address an upload job's file is written to
//! ([`crate::state::upload_path`]), PRESIGNED against the base URL this emulator is reached
//! on. The model binds no operation to that path, so a request following the published
//! address is exactly the case [`Service::override_routing_error`] answers, and that is
//! where the write is served: one `PUT`, carrying the three headers the operation documents
//! (the job's `ClientToken` and its digest, under `AES256`), stores the bytes on the job
//! they were written for. Nothing else about the service is reachable through it, and the
//! bytes it keeps are bounded per file and across the store
//! ([`crate::state::MAX_UPLOAD_FILE_BYTES`], [`crate::state::MAX_STORED_UPLOAD_FILE_BYTES`]).

pub mod modelled_refusal;
pub mod unreadable_values;
pub mod wire;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{
    http, ErasedOperation, OperationRoutingTable, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The `com-amazonaws-customerprofiles-customerprofiles-20200815` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The same modelled operations the generated table holds, each wrapped in
    /// [`modelled_refusal::ModelledRefusal`] so a request refused before its handler runs
    /// is answered as this service's own `BadRequestException` rather than in the
    /// framework's pre-dispatch envelope, which names no `Message`. Keyed by the routing
    /// key the protocol resolves — the key the generated table uses — and built from the
    /// SAME shared engine, so a wrapped operation reads and writes the one store.
    refused_as_modelled: OperationRoutingTable,
    /// The wire protocol the modelled sequence is driven through: `restJson1` as the model
    /// declares it, with an error document that reports its shape's `Message` member even
    /// for a refusal that words none ([`wire`]). Built once, here, because
    /// `override_protocol` hands back a borrow of it.
    wire_protocol: wire::MessageReportedRestJson,
    /// The SAME shared engine the generated entry threads into every operation.
    ///
    /// Held here because the upload-job file plane is served from
    /// [`Service::override_routing_error`], which is not an operation with a `self.engine`
    /// of its own, and it must write to the one store `CreateUploadJob` and
    /// `GetUploadJobPath` read.
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        let refused_as_modelled = modelled_refusal::table(&engine);
        Self {
            entry: crate::ServiceDefinition::new(Arc::clone(&engine)),
            refused_as_modelled,
            wire_protocol: wire::MessageReportedRestJson::new(),
            engine,
        }
    }

    /// Serve one request to the upload target of an upload job — the address
    /// `GetUploadJobPath` published for that job's file.
    ///
    /// A `PUT` carrying the three headers that operation documents stores the body as the
    /// job's file, which is the only thing this plane touches. Everything else is refused:
    /// another method, a target naming a job this store does not hold, a request that does
    /// not present the job's own data key, a body this process cannot read, and a write
    /// larger than this emulator keeps for one file or across the store — each before
    /// anything is stored, so a 200 from here always means the bytes were kept.
    fn serve_upload_job_file(
        &self,
        req: &RtRequest,
        target: &crate::state::UploadTarget,
    ) -> RtResponse {
        if req.method() != "PUT" {
            return plane_error(405, "MethodNotAllowed", UPLOAD_METHOD_MESSAGE);
        }
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return plane_error(500, "InternalServerException", UPLOAD_STORE_MESSAGE);
        };
        // The body is what this plane STORES, so a request whose body this process cannot
        // read is refused rather than stored as an empty file: answering 200 to a write
        // that kept nothing would tell the caller its file had landed.
        let Some(body) = req.body().bytes() else {
            return plane_error(400, "IncompleteBody", UPLOAD_UNREADABLE_MESSAGE);
        };
        if body.len() > crate::state::MAX_UPLOAD_FILE_BYTES {
            return plane_error(413, "EntityTooLarge", UPLOAD_TOO_LARGE_MESSAGE);
        }
        let algorithm = req.headers().get(crate::state::CUSTOMER_ALGORITHM_HEADER);
        let presented_key = req.headers().get(crate::state::CUSTOMER_KEY_HEADER);
        let presented_key_md5 = req.headers().get(crate::state::CUSTOMER_KEY_MD5_HEADER);
        let mut store = state.lock();
        // What the whole store already holds apart from the slot this write replaces, so a
        // re-written file is not counted twice against the total.
        let held = store.stored_upload_file_bytes_apart_from(target);
        let Some(job) = store.upload_job_mut(target) else {
            return plane_error(404, "NoSuchKey", UPLOAD_NOT_FOUND_MESSAGE);
        };
        if !crate::state::write_is_authorized(
            algorithm,
            presented_key,
            presented_key_md5,
            &job.client_token,
        ) {
            return plane_error(403, "AccessDenied", UPLOAD_UNAUTHORIZED_MESSAGE);
        }
        if held.saturating_add(body.len()) > crate::state::MAX_STORED_UPLOAD_FILE_BYTES {
            return plane_error(413, "EntityTooLarge", UPLOAD_STORE_FULL_MESSAGE);
        }
        // A target is written by ONE caller and may be re-written until the job is
        // started, exactly as an object store's key is: the last write is the job's file.
        job.uploaded_file = Some(body.to_vec());

        let mut resp = RtResponse::new(
            StatusCode::try_from(200_u16).expect("200 is a valid status"),
            SdkBody::empty(),
        );
        resp.headers_mut().insert("content-length", "0");
        resp
    }
}

/// The body of an upload-plane refusal of a method other than the one write a target takes.
const UPLOAD_METHOD_MESSAGE: &str = "This upload target is written by a single PUT";

/// The body of an upload-plane answer when this service's store is unreachable.
const UPLOAD_STORE_MESSAGE: &str = "the Customer Profiles upload-job store is unavailable";

/// The body of an upload-plane refusal of a request whose body this process cannot read,
/// and so cannot store.
const UPLOAD_UNREADABLE_MESSAGE: &str = "The upload's body could not be read";

/// The body of an upload-plane refusal of a target naming no stored upload job.
const UPLOAD_NOT_FOUND_MESSAGE: &str = "The upload target does not exist";

/// The body of an upload-plane refusal of a write that does not present the job's own data
/// key in the headers `GetUploadJobPath` documents. It names the headers, never the key.
const UPLOAD_UNAUTHORIZED_MESSAGE: &str = "A write to this upload target must carry the \
     upload job's client token and its MD5 digest in the server-side-encryption customer-key \
     headers";

/// The body of an upload-plane refusal of a file larger than this emulator keeps for one
/// upload job.
const UPLOAD_TOO_LARGE_MESSAGE: &str = "The upload is larger than this emulator keeps for \
     one upload job";

/// The body of an upload-plane refusal of a write that would take the whole store past what
/// this emulator keeps for upload files at once.
const UPLOAD_STORE_FULL_MESSAGE: &str = "This emulator already holds the maximum it keeps \
     for upload-job files";

/// One answer of the plane this service serves outside the model: the status, the code in
/// `x-amzn-errortype`, and a JSON body carrying the reason.
///
/// That plane is plain HTTPS rather than a modelled operation, so it answers in this one
/// small envelope instead of the service's restJson1 one. The message is built from the
/// constants above, never from the request or from stored state: a caller is told what is
/// wrong with its write, and nothing about what the store holds.
fn plane_error(status: u16, code: &str, message: &str) -> RtResponse {
    let body = ::serde_json::json!({ "message": message, "code": code }).to_string();
    let mut resp = RtResponse::new(
        StatusCode::try_from(status).expect("the status is a constant of this module"),
        SdkBody::from(body),
    );
    resp.headers_mut().insert("content-type", "application/json");
    resp.headers_mut()
        .insert("x-amzn-errortype", code.to_string());
    resp
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry.
        &self.entry
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        // Claim a request whose resolved route key is this service's signing
        // name. Edit this for anything the model does not express — a signing
        // name shared by several services, or access-key-id-based routing.
        match smithy_server_runtime::resolve_route_key(parts) {
            Some(key) => key == self.entry.identity().signing_name(),
            None => false,
        }
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // NOT a re-route: the key is the one the generated table's own protocol resolves,
        // and the operation it names is the one served — wrapped in
        // `modelled_refusal::ModelledRefusal` so a request refused BEFORE the handler runs
        // is answered as this service's modelled `BadRequestException`, whose prose member
        // the model spells `Message`, rather than in the framework's pre-dispatch envelope.
        // A key the model does not name resolves to `None` here exactly as it does in the
        // generated table, so the router's routing-error path is unchanged.
        let key = self.entry.protocol().routing_key(req).ok()?;
        self.refused_as_modelled.get(key)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // Customer Profiles states an input it will not accept as `BadRequestException`
        // ("The input you provided is invalid.") — the one client-input error every
        // operation in this model declares; the model declares no `ValidationException`
        // at all. `modelled_refusal::ModelledRefusal` answers such a refusal itself, as
        // that modelled SHAPE, so this seam only words whatever refusal the FRAMEWORK
        // still renders for a request that layer did not reach. Wording it from the same
        // violations keeps the two from disagreeing on the code; the status stays with the
        // protocol: restJson1 renders a refusal as HTTP 400, which is
        // `BadRequestException`'s modelled status.
        match refusal {
            Refusal::Invalid(violations) => Some(Refused {
                code: "BadRequestException".to_string(),
                message: aws_smithy_schema_server::envelope(violations)
                    .unwrap_or_else(|| "The input you provided is invalid.".to_string()),
            }),
            // Authentication is not this service's concern; the framework's 403 wording
            // is already what a caller needs.
            _ => None,
        }
    }

    fn override_protocol(&self) -> Option<&dyn smithy_server_runtime::ServerHttpProtocol> {
        // The model's own `restJson1`, with the one rendering this service owns: an error
        // document reports its shape's `Message` member — as JSON `null` for a refusal that
        // words no sentence — so "no message" and "no such member" stay distinguishable on
        // the wire. Routing, request reading and every success body are the framework's.
        Some(&self.wire_protocol)
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it. Returning `None` renders the protocol-correct
        // default for this service's envelope (query -> 400 InvalidAction text/xml,
        // awsJson -> 404, rpcv2Cbor -> 404, rest -> 501). Return `Some(resp)` to
        // word that routing failure in your own envelope instead.
        //
        // A request to the upload target `GetUploadJobPath` published is exactly that case
        // by construction — the model binds no operation to the upload path — so this is
        // where those writes are served. Anything else keeps the framework's default.
        let target = crate::state::parse_upload_path(::service_common::request_path(req.uri()))?;
        Some(self.serve_upload_job_file(req, &target))
    }

    fn override_framework_error(
        &self,
        error: &aws_smithy_schema_server::ProtocolError,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this on a transport-level failure it detects up front —
        // before (or instead of) naming an operation — such as a malformed
        // `aws-chunked` body. Returning `None` keeps the entry's protocol-correct
        // rendering. Return `Some(resp)` to reword that failure in your own
        // envelope. The error is borrowed, so the default renderer can still
        // consume it on `None`.
        let _ = error;
        None
    }
}
