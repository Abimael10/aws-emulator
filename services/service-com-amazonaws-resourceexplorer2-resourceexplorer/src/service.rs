// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-resourceexplorer2-resourceexplorer` service: how it claims requests and how it words the
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
//!   before any handler ran. The default returns `None`, keeping the framework's
//!   wording. See the inline note on that method for how to reword.
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
//! # How a refusal of a request BODY is worded
//!
//! Resource Explorer validates a request's JSON body against the operation's modelled
//! input ahead of the code that owns its modelled errors, and refuses a body that breaks
//! any rule the model declares — a value outside a declared range or length, a collection
//! of a size the model does not admit, a required body member the request omits — with
//! ONE answer: HTTP 400, the `ValidationException` its model declares for invalid input,
//! and the fixed sentence [`INVALID_REQUEST_BODY`], which names neither the member that
//! failed nor the value it rejected. The framework's own wording instead renders the
//! generic constraint envelope ("N validation error(s) detected: Value '…' at '…' failed
//! to satisfy constraint: …"), a dialect this service does not speak and one that quotes
//! the caller's own value back at it.
//!
//! That is not reachable from the `override_refusal` seam, which is handed the violations
//! ALONE and so cannot tell a body value from one the request carries in its URI (the
//! `@httpLabel` `resourceArn` and the `@httpQuery` `tagKeys` of the tagging routes). So
//! every modelled operation is served behind this service's front door
//! ([`service_common::front_door::FrontDoorTable`]), which reads each violated member's
//! HTTP binding off that operation's own input schema. Nothing is re-implemented and no
//! check is repeated in other words: the violations are the framework's own
//! (the protocol's deserializer and `constraints::validate`, through
//! `service_common::validation`), and the envelope and the 400 stay the framework's.
//!
//! A rule the model states only in prose is NOT this answer: it is refused by the
//! operation that owns it, as the modelled `ValidationException` naming what failed (a
//! filter string carrying free-form text, an index in a state that forbids the call).

use service_common::front_door::{FrontDoorTable, INVALID_REQUEST_BODY};
use service_common::validation::SubmittedEcho;
use smithy_server_runtime::{
    http, ErasedOperation, OperationRoutingTable, Refusal, Refused, Service, Violation,
};
use std::sync::Arc;

/// The error code every body refusal this file words carries: the `ValidationException`
/// this model declares for invalid input (`@httpError(400)`), which is the code the
/// framework's own refusal channel names too — so only the MESSAGE differs from the
/// default, and a client still branches on the code it already branched on.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// How this service words a request BODY the boundary validator refused: one fixed
/// sentence for every member and every constraint class, carrying nothing from the
/// request.
///
/// The violations are not read: the answer is the same whichever member broke whichever
/// rule, which is exactly what makes the sentence value-free.
fn body_wording(_refused: &[(Violation, SubmittedEcho)]) -> Option<Refused> {
    Some(Refused {
        code: VALIDATION_EXCEPTION.to_string(),
        message: INVALID_REQUEST_BODY.to_string(),
    })
}

/// The `com-amazonaws-resourceexplorer2-resourceexplorer` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The same modelled operations the generated table holds, each served behind this
    /// service's front door so a request BODY that breaks a modelled rule is refused the
    /// way this service refuses one (see "How a refusal of a request BODY is worded").
    /// Keyed by the routing key the protocol resolves — the key the generated table uses
    /// — and built from the SAME shared engine, so a wrapped operation reads and writes
    /// the one store.
    front_door: OperationRoutingTable,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        let mut front_door = FrontDoorTable::worded(body_wording);
        // EVERY modelled operation, so the rule is applied by ONE code path rather than
        // per operation. Each is keyed by its own modelled name, which `FrontDoorTable`
        // reads off the operation.
        front_door
            .serve(crate::ops::associate_default_view::AssociateDefaultView::new(Arc::clone(
                &engine,
            )))
            .serve(crate::ops::batch_get_view::BatchGetView::new(Arc::clone(&engine)))
            .serve(crate::ops::create_index::CreateIndex::new(Arc::clone(&engine)))
            .serve(crate::ops::create_resource_explorer_setup::CreateResourceExplorerSetup::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::create_view::CreateView::new(Arc::clone(&engine)))
            .serve(crate::ops::delete_index::DeleteIndex::new(Arc::clone(&engine)))
            .serve(crate::ops::delete_resource_explorer_setup::DeleteResourceExplorerSetup::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::delete_view::DeleteView::new(Arc::clone(&engine)))
            .serve(crate::ops::disassociate_default_view::DisassociateDefaultView::new(Arc::clone(
                &engine,
            )))
            .serve(crate::ops::get_default_view::GetDefaultView::new(Arc::clone(&engine)))
            .serve(crate::ops::get_index::GetIndex::new(Arc::clone(&engine)))
            .serve(crate::ops::get_managed_view::GetManagedView::new(Arc::clone(&engine)))
            .serve(crate::ops::get_resource_explorer_setup::GetResourceExplorerSetup::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::get_service_view::GetServiceView::new(Arc::clone(&engine)))
            .serve(crate::ops::get_view::GetView::new(Arc::clone(&engine)))
            .serve(crate::ops::list_indexes::ListIndexes::new(Arc::clone(&engine)))
            .serve(crate::ops::list_indexes_for_members::ListIndexesForMembers::new(Arc::clone(
                &engine,
            )))
            .serve(crate::ops::list_managed_views::ListManagedViews::new(Arc::clone(&engine)))
            .serve(crate::ops::list_resources::ListResources::new(Arc::clone(&engine)))
            .serve(crate::ops::list_service_indexes::ListServiceIndexes::new(Arc::clone(&engine)))
            .serve(crate::ops::list_service_views::ListServiceViews::new(Arc::clone(&engine)))
            .serve(
                crate::ops::list_streaming_access_for_services::ListStreamingAccessForServices::new(
                    Arc::clone(&engine),
                ),
            )
            .serve(crate::ops::list_supported_resource_types::ListSupportedResourceTypes::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::list_tags_for_resource::ListTagsForResource::new(Arc::clone(
                &engine,
            )))
            .serve(crate::ops::list_views::ListViews::new(Arc::clone(&engine)))
            .serve(crate::ops::search::Search::new(Arc::clone(&engine)))
            .serve(crate::ops::tag_resource::TagResource::new(Arc::clone(&engine)))
            .serve(crate::ops::untag_resource::UntagResource::new(Arc::clone(&engine)))
            .serve(crate::ops::update_index_type::UpdateIndexType::new(Arc::clone(&engine)))
            .serve(crate::ops::update_view::UpdateView::new(Arc::clone(&engine)));
        Self {
            entry: crate::ServiceDefinition::new(Arc::clone(&engine)),
            front_door: front_door.finish(),
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
        // NOT a re-route: the key is the one the generated table's own protocol resolves,
        // and the operation it names is the one served — wrapped in the front door so a
        // request BODY that breaks a modelled rule is refused the way this service
        // refuses one. A key the model does not name resolves to `None` here exactly as
        // it does in the generated table, so the router's routing-error path is
        // unchanged.
        let key = self.entry.protocol().routing_key(req).ok()?;
        self.front_door.get(key)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The front door above answers every refusal of a request BODY, so this seam is
        // left to the framework: what reaches it is a refusal of a value the request
        // carries OUTSIDE its body (the `@httpLabel` `resourceArn` and the `@httpQuery`
        // `tagKeys` of the tagging routes), or a body the codec itself could not read at
        // all and so reported no constraint violation for. Neither is the body rejection
        // this service words, and AWS states a URI-bound value's refusal naming the
        // member that failed — which is what the framework's own wording already does.
        let _ = refusal;
        None
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
