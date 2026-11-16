// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-securityir-securityincidentresponse` service: how it claims requests and how it words the
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
//!   before any handler ran. This service keeps the framework's wording there: the
//!   refusal its validating layer words is the one its front door answers, per
//!   operation (see `validation_report`).
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

use service_common::front_door::FrontDoor;
use smithy_server_runtime::{
    http, ErasedOperation, Operation, OperationRoutingTable, Refusal, Refused, Service,
};
use std::sync::Arc;

/// How this service words a request BODY it refused for a rule its request schema
/// declares: the findings the validating layer in front of it reports, which are not the
/// clauses the framework writes for a value bound to the request URI.
mod validation_report;

/// The `com-amazonaws-securityir-securityincidentresponse` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The same modelled operations the generated table holds, each served behind this
    /// service's front door, so a request BODY that breaks a rule the operation's schema
    /// declares is reported in that layer's words ([`validation_report`]) before the
    /// operation runs. Keyed by the routing key the protocol resolves — the key the
    /// generated table uses — and built from the SAME shared engine, so a wrapped operation
    /// reads and writes the one store.
    front_door: OperationRoutingTable,
}

/// Serve one `crate::ops::<module>::<Operation>` per line behind this service's front door.
macro_rules! serve_operations {
    ($table:expr, $engine:expr, $($module:ident :: $operation:ident),+ $(,)?) => {
        $(
            serve(
                &mut $table,
                crate::ops::$module::$operation::new(::std::sync::Arc::clone(&$engine)),
            );
        )+
    };
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        let mut front_door = OperationRoutingTable::new();
        // EVERY modelled operation, so the rule is applied by ONE code path rather than per
        // operation: an operation that binds nothing to the body (`GetCase`, whose only
        // member is its URI label) simply never has a body violation to report, which the
        // door reads off that operation's own input schema. That the list is the WHOLE
        // modelled surface is pinned by the suite at the foot of this file.
        serve_operations!(
            front_door,
            engine,
            batch_get_member_account_details::BatchGetMemberAccountDetails,
            cancel_membership::CancelMembership,
            close_case::CloseCase,
            create_case::CreateCase,
            create_case_comment::CreateCaseComment,
            create_membership::CreateMembership,
            get_case::GetCase,
            get_case_attachment_download_url::GetCaseAttachmentDownloadUrl,
            get_case_attachment_upload_url::GetCaseAttachmentUploadUrl,
            get_finding_metrics::GetFindingMetrics,
            get_membership::GetMembership,
            list_case_edits::ListCaseEdits,
            list_cases::ListCases,
            list_comments::ListComments,
            list_investigations::ListInvestigations,
            list_memberships::ListMemberships,
            list_tags_for_resource::ListTagsForResource,
            send_feedback::SendFeedback,
            tag_resource::TagResource,
            untag_resource::UntagResource,
            update_case::UpdateCase,
            update_case_comment::UpdateCaseComment,
            update_case_status::UpdateCaseStatus,
            update_membership::UpdateMembership,
            update_resolver_type::UpdateResolverType,
        );
        Self {
            entry: crate::ServiceDefinition::new(engine),
            front_door,
        }
    }
}

/// Serve one modelled operation behind this service's front door, under the modelled name
/// the protocol resolves it by.
///
/// The wording is monomorphised for `Op`, which is what hands it that operation's own input
/// schema — the schema the body is validated against. `FrontDoorTable` keys one wording for
/// a whole service, so the table is built here instead.
fn serve<Op: Operation + ErasedOperation + 'static>(
    front_door: &mut OperationRoutingTable,
    operation: Op,
) {
    front_door.add_operation(
        Op::NAME,
        Box::new(FrontDoor::worded(
            operation,
            validation_report::body_findings_for::<Op>,
        )),
    );
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
        // and the operation it names is the one served — behind this service's front door,
        // so a request BODY that breaks a rule its schema declares is reported in the
        // validating layer's words ([`validation_report`]) before the operation runs. A key
        // the model does not name resolves to `None` here exactly as it does in the
        // generated table, so the router's routing-error path is unchanged.
        let key = self.entry.protocol().routing_key(req).ok()?;
        self.front_door.get(key)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this
        // service reword it. Returning `None` keeps the framework's default wording,
        // which is what reaches a caller here.
        //
        // What arrives at this seam is the refusal of a value the request carries OUTSIDE
        // its body — a `@httpLabel` path segment — and the refusal of a body this service's
        // validating layer states no finding for: a BODY violation is reported by the front
        // door above, in that layer's words, which this seam cannot do, because it is handed
        // the violations alone with no operation and so cannot tell the two halves apart.
        // No recording of this service shows its words for either, so both keep the
        // framework's own envelope rather than being given one.
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

#[cfg(test)]
mod tests {
    //! The one thing about the wiring above that no wire test can see: that the
    //! hand-maintained `serve_operations!` list is the WHOLE modelled surface.

    use super::*;

    /// This service's Smithy model, as it ships beside this crate. Read at test time rather
    /// than embedded: it is JSON no non-test build needs.
    const MODEL_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-securityir-securityincidentresponse.json"
    );

    /// RULE applied to the whole surface: every operation the model declares is served
    /// behind the front door, under the same key the generated routing table is keyed by.
    ///
    /// The names come from the MODEL rather than from a second hand-written list, so an
    /// operation added to the model and not to `serve_operations!` fails here instead of
    /// silently answering a refused body in the framework's wording — and a key spelled
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
        assert!(
            !operations.is_empty(),
            "case the model's surface: expected this service's operations, read none at all"
        );

        let service = EmulatedService::new(Arc::new(engine::InMemoryEngine::new()));
        for operation in operations {
            assert!(
                service.entry.routing_table().get(operation).is_some(),
                "case {operation}: the generated table keys an operation by its shape name; \
                 this one resolved to nothing, so the wrapper's key spelling is wrong"
            );
            assert!(
                service.front_door.get(operation).is_some(),
                "case {operation}: every modelled operation is served behind the front door; \
                 this one is missing from serve_operations!, so a body that breaks a rule its \
                 schema declares is refused in the framework's wording instead of this \
                 service's"
            );
        }
    }
}
