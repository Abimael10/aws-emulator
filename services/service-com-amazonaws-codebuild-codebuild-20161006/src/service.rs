// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-codebuild-codebuild-20161006` service: how it claims requests and how it words the
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
//! - `override_operation` — the seam a service has BEFORE dispatch. EDITED HERE: it
//!   serves no request the generated routing table would not serve, but it answers a
//!   body this service cannot read into the modelled input shape itself
//!   ([`unreadable_body`]), and serves every other request through the layer that keeps
//!   the operation it named in scope for the run ([`served_operation`]).
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

use aws_smithy_schema::Schema;
use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::sync::Arc;

/// How this service words a request the framework refused before any handler ran.
mod refusal;

/// Which operation is serving the request, for the refusal wording that is a fact about
/// the operation rather than about the constraint alone.
mod served_operation;

/// How a body this service cannot READ into the modelled input shape is refused — the
/// deserialization layer, which answers before the constraint layer above.
mod unreadable_body;

/// The `com-amazonaws-codebuild-codebuild-20161006` emulated service: the generated [`ServiceDefinition`] plus
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
        // NEITHER arm below is a re-route: both answer for the operation the model's own
        // routing key names, and a key the model does not name resolves to `None` here
        // exactly as it does in the generated table, so the router's routing-error path is
        // unchanged.
        //
        // A body this service cannot READ into the modelled input shape is refused FIRST,
        // because deserialization precedes every later layer: the framework would otherwise
        // report a codec failure as a constraint violation, which is a different error class
        // and a different remediation for the caller ([`unreadable_body`]).
        if unreadable_body::refuses(self.definition(), req) {
            return Some(&unreadable_body::UNREADABLE_BODY);
        }
        // Every other request is served exactly as the generated table serves it, with the
        // operation it named in scope for the run — which is what lets [`refusal`] state a
        // sentence that depends on WHICH operation's member was refused
        // ([`served_operation`]).
        if ::service_common::restated_refusal::routes(self.definition(), req) {
            return Some(&served_operation::SERVED_OPERATION_LAYER);
        }
        None
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // CodeBuild states a request it cannot accept as `InvalidInputException`
        // ("The input value that was provided is not valid"), the client-input error
        // every operation in its model declares; the framework's default code is a
        // synthetic `ValidationException` no CodeBuild client branches on. Both the
        // code and the message are replaced together, and the sentence is chosen per
        // broken RULE by [`refusal::message`]: the constraint classes CodeBuild words
        // itself get its own sentence, and every other violation keeps the shared
        // envelope naming the member and the constraint it failed.
        match refusal {
            Refusal::Invalid(violations) => Some(Refused {
                code: "InvalidInputException".to_string(),
                message: refusal::message(violations),
            }),
            // Authentication is the framework's answer to give, not this service's.
            _ => None,
        }
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

/// Every operation input this service validates a request against, as the schema
/// the framework walks for it.
///
/// Listed in full rather than for the one operation that happens to need it today,
/// so the floating-point rendering below is derived from the MODEL for every
/// request this service accepts: an operation whose input grows a floating-point
/// member is covered without another edit here.
const INPUT_SCHEMAS: &[&Schema] = &[
    crate::ops::types::BATCH_DELETE_BUILDS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_BUILD_BATCHES_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_BUILDS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_COMMAND_EXECUTIONS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_FLEETS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_PROJECTS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_REPORT_GROUPS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_REPORTS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_SANDBOXES_INPUT_SCHEMA,
    crate::ops::types::CREATE_FLEET_INPUT_SCHEMA,
    crate::ops::types::CREATE_PROJECT_INPUT_SCHEMA,
    crate::ops::types::CREATE_REPORT_GROUP_INPUT_SCHEMA,
    crate::ops::types::CREATE_WEBHOOK_INPUT_SCHEMA,
    crate::ops::types::DELETE_BUILD_BATCH_INPUT_SCHEMA,
    crate::ops::types::DELETE_FLEET_INPUT_SCHEMA,
    crate::ops::types::DELETE_PROJECT_INPUT_SCHEMA,
    crate::ops::types::DELETE_REPORT_GROUP_INPUT_SCHEMA,
    crate::ops::types::DELETE_REPORT_INPUT_SCHEMA,
    crate::ops::types::DELETE_RESOURCE_POLICY_INPUT_SCHEMA,
    crate::ops::types::DELETE_SOURCE_CREDENTIALS_INPUT_SCHEMA,
    crate::ops::types::DELETE_WEBHOOK_INPUT_SCHEMA,
    crate::ops::types::DESCRIBE_CODE_COVERAGES_INPUT_SCHEMA,
    crate::ops::types::DESCRIBE_TEST_CASES_INPUT_SCHEMA,
    crate::ops::types::GET_REPORT_GROUP_TREND_INPUT_SCHEMA,
    crate::ops::types::GET_RESOURCE_POLICY_INPUT_SCHEMA,
    crate::ops::types::IMPORT_SOURCE_CREDENTIALS_INPUT_SCHEMA,
    crate::ops::types::INVALIDATE_PROJECT_CACHE_INPUT_SCHEMA,
    crate::ops::types::LIST_BUILD_BATCHES_FOR_PROJECT_INPUT_SCHEMA,
    crate::ops::types::LIST_BUILD_BATCHES_INPUT_SCHEMA,
    crate::ops::types::LIST_BUILDS_FOR_PROJECT_INPUT_SCHEMA,
    crate::ops::types::LIST_BUILDS_INPUT_SCHEMA,
    crate::ops::types::LIST_COMMAND_EXECUTIONS_FOR_SANDBOX_INPUT_SCHEMA,
    crate::ops::types::LIST_CURATED_ENVIRONMENT_IMAGES_INPUT_SCHEMA,
    crate::ops::types::LIST_FLEETS_INPUT_SCHEMA,
    crate::ops::types::LIST_PROJECTS_INPUT_SCHEMA,
    crate::ops::types::LIST_REPORT_GROUPS_INPUT_SCHEMA,
    crate::ops::types::LIST_REPORTS_FOR_REPORT_GROUP_INPUT_SCHEMA,
    crate::ops::types::LIST_REPORTS_INPUT_SCHEMA,
    crate::ops::types::LIST_SANDBOXES_FOR_PROJECT_INPUT_SCHEMA,
    crate::ops::types::LIST_SANDBOXES_INPUT_SCHEMA,
    crate::ops::types::LIST_SHARED_PROJECTS_INPUT_SCHEMA,
    crate::ops::types::LIST_SHARED_REPORT_GROUPS_INPUT_SCHEMA,
    crate::ops::types::LIST_SOURCE_CREDENTIALS_INPUT_SCHEMA,
    crate::ops::types::PUT_RESOURCE_POLICY_INPUT_SCHEMA,
    crate::ops::types::RETRY_BUILD_BATCH_INPUT_SCHEMA,
    crate::ops::types::RETRY_BUILD_INPUT_SCHEMA,
    crate::ops::types::START_BUILD_BATCH_INPUT_SCHEMA,
    crate::ops::types::START_BUILD_INPUT_SCHEMA,
    crate::ops::types::START_COMMAND_EXECUTION_INPUT_SCHEMA,
    crate::ops::types::START_SANDBOX_CONNECTION_INPUT_SCHEMA,
    crate::ops::types::START_SANDBOX_INPUT_SCHEMA,
    crate::ops::types::STOP_BUILD_BATCH_INPUT_SCHEMA,
    crate::ops::types::STOP_BUILD_INPUT_SCHEMA,
    crate::ops::types::STOP_SANDBOX_INPUT_SCHEMA,
    crate::ops::types::UPDATE_FLEET_INPUT_SCHEMA,
    crate::ops::types::UPDATE_PROJECT_INPUT_SCHEMA,
    crate::ops::types::UPDATE_PROJECT_VISIBILITY_INPUT_SCHEMA,
    crate::ops::types::UPDATE_REPORT_GROUP_INPUT_SCHEMA,
    crate::ops::types::UPDATE_WEBHOOK_INPUT_SCHEMA,
];

/// How the model names the INPUT shape of an operation: the operation's own name with this
/// suffix, which is what [`input_schema`] resolves a routing key through.
const INPUT_SHAPE_SUFFIX: &str = "Input";

/// The input schema of the operation `operation` names, or `None` when this service serves
/// no operation by that name.
///
/// Resolved off the schemas themselves ([`INPUT_SCHEMAS`]) rather than from a table written
/// here, so every operation this service serves is reachable by the name its routing key
/// carries — including the ones whose handlers are still scaffolded — and an operation added
/// to the model needs no edit here. The name of a routing key and the name of its input shape
/// are both the model's: `BatchGetProjects` is served by `BatchGetProjectsInput`.
fn input_schema(operation: &str) -> Option<&'static Schema> {
    let shape = format!("{operation}{INPUT_SHAPE_SUFFIX}");
    INPUT_SCHEMAS
        .iter()
        .copied()
        .find(|input| input.shape_id().shape_name() == shape)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a routing key resolves to the input shape the model gives that operation, and a
    /// name no operation of this service carries resolves to nothing.
    ///
    /// The rows span a batch read, a create, an update and a listing — and, as the negative
    /// side, an input SHAPE name (which is not a routing key) and a name this model does not
    /// declare at all.
    #[test]
    fn every_routing_key_resolves_to_its_modelled_input() {
        for operation in [
            "BatchGetProjects",
            "BatchGetBuilds",
            "CreateProject",
            "UpdateProject",
            "ListProjects",
        ] {
            let resolved = input_schema(operation);
            assert_eq!(
                resolved.map(|input| input.shape_id().shape_name()),
                Some(format!("{operation}{INPUT_SHAPE_SUFFIX}")).as_deref(),
                "case {operation:?}: expected the modelled input shape, got {:?}",
                resolved.map(|input| input.shape_id().shape_name())
            );
        }
        for unknown in ["BatchGetProjectsInput", "NoSuchOperation", ""] {
            assert!(
                input_schema(unknown).is_none(),
                "case {unknown:?}: expected no input shape, got one"
            );
        }
    }

    /// Every input this service validates a request against is reachable by the operation
    /// name its routing key carries: a model whose input shape is named otherwise would
    /// silently take the deserialization layer out of the path for that operation.
    #[test]
    fn every_input_is_reachable_by_its_operation_name() {
        for input in INPUT_SCHEMAS {
            let shape = input.shape_id().shape_name();
            let operation = shape
                .strip_suffix(INPUT_SHAPE_SUFFIX)
                .unwrap_or_else(|| panic!("case {shape}: expected an input shape name"));
            assert_eq!(
                input_schema(operation).map(|resolved| resolved.shape_id().shape_name()),
                Some(shape),
                "case {shape}: expected to resolve from {operation:?}"
            );
        }
    }
}


