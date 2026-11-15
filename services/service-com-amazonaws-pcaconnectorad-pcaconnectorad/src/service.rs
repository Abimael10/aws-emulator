// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-pcaconnectorad-pcaconnectorad` service: how it claims requests and how it words the
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
//!   before any handler ran. This service keeps the framework's envelope and states
//!   one rendering of its own, a `@pattern` requirement quoted without the
//!   whole-value anchors the model wraps it in. See the inline note on that method.
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

use service_common::front_door::FrontDoorTable;
use smithy_server_runtime::{
    http, ErasedOperation, OperationRoutingTable, Refusal, Refused, Service,
};
use std::sync::Arc;

/// How this service words a request BODY it refused for a modelled constraint: the
/// findings the validating layer in front of it states, which are not the clauses the
/// framework writes for a value bound to the request URI.
mod schema_findings;

/// The `com-amazonaws-pcaconnectorad-pcaconnectorad` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The same modelled operations the generated table holds, each served behind this
    /// service's front door, so a request BODY that breaks a modelled constraint is
    /// refused in that layer's words ([`schema_findings`]) before the operation runs.
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
        let mut front_door = FrontDoorTable::worded(schema_findings::body_findings);
        // EVERY modelled operation, so the rule is applied by ONE code path rather than
        // per operation: an operation that binds nothing to the body (`GetConnector`,
        // whose only member is its URI label) simply never has a body violation to find,
        // which the door reads off its own input schema. Each is keyed by its own
        // modelled name, which `FrontDoorTable` reads off the operation.
        front_door
            .serve(crate::ops::create_connector::CreateConnector::new(Arc::clone(&engine)))
            .serve(crate::ops::create_directory_registration::CreateDirectoryRegistration::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::create_service_principal_name::CreateServicePrincipalName::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::create_template::CreateTemplate::new(Arc::clone(&engine)))
            .serve(
                crate::ops::create_template_group_access_control_entry::CreateTemplateGroupAccessControlEntry::new(
                    Arc::clone(&engine),
                ),
            )
            .serve(crate::ops::delete_connector::DeleteConnector::new(Arc::clone(&engine)))
            .serve(crate::ops::delete_directory_registration::DeleteDirectoryRegistration::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::delete_service_principal_name::DeleteServicePrincipalName::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::delete_template::DeleteTemplate::new(Arc::clone(&engine)))
            .serve(
                crate::ops::delete_template_group_access_control_entry::DeleteTemplateGroupAccessControlEntry::new(
                    Arc::clone(&engine),
                ),
            )
            .serve(crate::ops::get_connector::GetConnector::new(Arc::clone(&engine)))
            .serve(crate::ops::get_directory_registration::GetDirectoryRegistration::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::get_service_principal_name::GetServicePrincipalName::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::get_template::GetTemplate::new(Arc::clone(&engine)))
            .serve(
                crate::ops::get_template_group_access_control_entry::GetTemplateGroupAccessControlEntry::new(
                    Arc::clone(&engine),
                ),
            )
            .serve(crate::ops::list_connectors::ListConnectors::new(Arc::clone(&engine)))
            .serve(crate::ops::list_directory_registrations::ListDirectoryRegistrations::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::list_service_principal_names::ListServicePrincipalNames::new(
                Arc::clone(&engine),
            ))
            .serve(crate::ops::list_tags_for_resource::ListTagsForResource::new(Arc::clone(
                &engine,
            )))
            .serve(
                crate::ops::list_template_group_access_control_entries::ListTemplateGroupAccessControlEntries::new(
                    Arc::clone(&engine),
                ),
            )
            .serve(crate::ops::list_templates::ListTemplates::new(Arc::clone(&engine)))
            .serve(crate::ops::tag_resource::TagResource::new(Arc::clone(&engine)))
            .serve(crate::ops::untag_resource::UntagResource::new(Arc::clone(&engine)))
            .serve(crate::ops::update_template::UpdateTemplate::new(Arc::clone(&engine)))
            .serve(
                crate::ops::update_template_group_access_control_entry::UpdateTemplateGroupAccessControlEntry::new(
                    Arc::clone(&engine),
                ),
            );
        Self {
            entry: crate::ServiceDefinition::new(engine),
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
        // NOT a re-route: the key is the one the generated table's own protocol
        // resolves, and the operation it names is the one served — behind this
        // service's front door, so a request BODY outside a modelled constraint is
        // refused in that layer's words ([`schema_findings`]) before the operation
        // runs. A key the model does not name resolves to `None` here exactly as it
        // does in the generated table, so the router's routing-error path is unchanged.
        let key = self.entry.protocol().routing_key(req).ok()?;
        self.front_door.get(key)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (a value
        // outside a declared bound, a value a `@pattern` does not admit) to let this
        // service state it its own way.
        //
        // This service answers the `ValidationException` envelope the framework
        // already renders — same 400, same code, same `N validation error(s)
        // detected:` prefix, same member paths, same echoed value — with ONE
        // rendering of its own: a `@pattern` requirement is quoted as the expression
        // a value has to satisfy, WITHOUT the `^` / `$` the model wraps it in to turn
        // Smithy's find into a whole-value match. The anchoring is how the model
        // spells that match, not part of the character grammar this service
        // publishes, so a client that lifts the expression out of the message and
        // re-validates locally gets the one the service advertises. The CHECK is
        // unchanged: it still runs the model's pattern as written.
        //
        // That rendering is shared by every service on a JSON protocol (this one
        // answers restJson1), so it lives in `service-common` rather than being
        // spelled here, and it reaches every member of every operation this service
        // models rather than the one `@pattern` a recording happened to break.
        //
        // What reaches here is the refusal of a value the request carries OUTSIDE its
        // body: a BODY member's violation is answered by the front door above, in the
        // words the validating layer in front of this service states
        // ([`schema_findings`]), which this seam cannot do — it is handed the
        // violations alone, with no operation, so it cannot tell the two halves apart.
        ::service_common::validation_message::aws_validation_refusal(refusal)
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

#[cfg(test)]
mod tests {
    //! The one thing about the wiring above that a wire test cannot see: that the
    //! hand-maintained `serve` list is the WHOLE surface.

    use super::*;

    /// This service's Smithy model, as it ships beside this crate. Read at test time rather
    /// than embedded: it is JSON that no non-test build needs.
    const MODEL_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-pcaconnectorad-pcaconnectorad.json"
    );

    /// How many operations this service's model declares, as a second, independent
    /// statement of its surface (`smithy_query(query="list_operations")`): a model that
    /// grows or shrinks fails here rather than quietly leaving the new operation out of the
    /// front door.
    const MODELLED_OPERATIONS: usize = 25;

    /// RULE applied to the whole surface: every operation the model declares is served
    /// behind this service's front door, under the same key the generated routing table is
    /// keyed by.
    ///
    /// The names come from the MODEL rather than from a second hand list, so an operation
    /// added to the model and not to the `serve` list fails here instead of silently
    /// answering a body-constraint refusal in the framework's wording — and a key spelled
    /// differently from the generated table's fails here instead of silently never being
    /// reached by `override_operation`.
    #[test]
    fn every_modelled_operation_is_served_behind_the_front_door() {
        let model = std::fs::read_to_string(MODEL_PATH)
            .expect("the model that generated this crate ships beside it");
        let model: serde_json::Value =
            serde_json::from_str(&model).expect("the model that generated this crate is JSON");
        let shapes = model["shapes"]
            .as_object()
            .expect("a Smithy model declares shapes");
        let operations: Vec<&str> = shapes
            .iter()
            .filter(|(_, shape)| shape["type"] == "operation")
            .filter_map(|(id, _)| id.rsplit('#').next())
            .collect();
        assert_eq!(
            operations.len(),
            MODELLED_OPERATIONS,
            "case the model's surface: expected this service's {MODELLED_OPERATIONS} \
             operations, got {operations:?}"
        );

        let service = EmulatedService::new(Arc::new(engine::InMemoryEngine::new()));
        for operation in operations {
            assert!(
                service.entry.routing_table().get(operation).is_some(),
                "case {operation}: the generated table keys an operation by its shape name; \
                 this one resolved to nothing, so the front door's key spelling is wrong"
            );
            assert!(
                service.front_door.get(operation).is_some(),
                "case {operation}: every modelled operation is served behind the front door; \
                 this one is missing from the `serve` list, so a body that breaks a modelled \
                 constraint is refused in the framework's wording instead of this service's"
            );
        }
    }
}
