// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-directconnect-overtureservice` service: how it claims requests and how it words the
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
//! Five opt-in seams are yours to change; every one defaults to the framework's
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
//!   before any handler ran. Edited here: an enumerated type's value set is stated
//!   in the order THIS service states it in, which is not the order the set reaches
//!   the seam in; every other refusal keeps the framework's own wording.
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

use smithy_server_runtime::{http, ErasedOperation, Refusal, Refused, Service, Violation};
use std::sync::Arc;

/// The enumerated value sets this service states in an order of its OWN: one row per
/// enumerated type, holding the values as the MODEL declares them and the same values in
/// the order this service's own refusals state them.
///
/// The model half is matched as a SET, so an enum is recognised by the values a member
/// admits and not by the order a schema presents them in: one type is stated alike for
/// every member targeting it, in any operation and at any depth, and a model revision
/// that reordered its own declaration would not move the stated set. A set no row names
/// keeps the order it reached the seam in, because this service has shown no other order
/// for it.
const STATED_ENUM_VALUE_SETS: &service_common::validation_message::StatedEnumValueSets<'static> =
    &[(
        // `ResiliencyModel`, declared `maximum-resiliency, high-resiliency,
        // basic-resiliency` (`smithy_query(query="shape",shape="ResiliencyModel")`) and
        // stated by this service's own refusals in ascending order — the values the
        // model declares, reordered and nothing else. The model half is the generated
        // slice, so a model revision that ADDED a value to this enum would no longer
        // match this row (the match is on the whole set) and the refusal would fall
        // back to stating the set in the order the schema carries it, which is the
        // framework's own behaviour rather than a wrong order of this service's.
        crate::ops::types::RESILIENCY_MODEL_VALUES,
        &["basic-resiliency", "high-resiliency", "maximum-resiliency"],
    )];

/// `violation` with an enumerated type's value set stated in the order THIS service
/// states that type's values in ([`STATED_ENUM_VALUE_SETS`]); every other violation, and
/// every value set this service pins no order of, is returned unchanged.
///
/// The CLAUSE stays the framework's: only what goes inside the brackets moves, so the
/// envelope, the member path, the echoed value and the requirement's wording keep coming
/// from the one place that writes them, and the ordering itself is `service_common`'s one
/// statement of it.
fn in_stated_enum_order(violation: &Violation) -> Violation {
    service_common::validation_message::in_stated_enum_order(violation, STATED_ENUM_VALUE_SETS)
}

/// The `com-amazonaws-directconnect-overtureservice` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(engine),
        }
    }
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
        // Return `Some(op)` to serve a request the model's routing key cannot name
        // correctly; `None` (the default) lets the generated routing table resolve
        // it. `req` is the whole request, so a decision that depends on the body —
        // the `Action` form field on a query protocol, say — is reachable here.
        let _ = req;
        None
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound or an enumerated
        // set) before any handler runs.
        //
        // ONE thing this service states differently: an enumerated type's value
        // set is listed in the order this service states that type in
        // ([`in_stated_enum_order`]) rather than the order the set reaches this
        // seam in, which is the order the model happens to declare that type's
        // members in. The code, the status, the envelope, the member paths, the
        // echoed value and every other requirement stay the framework's, so
        // nothing else can drift — and a refusal this service states exactly as the
        // framework already does keeps the framework's own rendering rather than
        // being re-rendered here.
        let Refusal::Invalid(violations) = refusal else {
            return None;
        };
        let restated: Vec<Violation> = violations.iter().map(in_stated_enum_order).collect();
        if restated == *violations {
            return None;
        }
        Some(Refused {
            // The code a constraint refusal carries on this service's awsJson1_1
            // envelope — the same one the framework's default picks, supplied here
            // because a chosen message must travel with its matching code.
            code: "ValidationException".to_string(),
            // The same envelope the framework renders (`N validation error(s)
            // detected: …`), built from the restated violations so the clause
            // wording stays in the one place that owns it.
            message: aws_smithy_schema_server::constraints::envelope(&restated)?,
        })
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
        let _ = req;
        None
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
