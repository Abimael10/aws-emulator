// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-verifiedpermissions-verifiedpermissions` service: how it claims requests and how it words the
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
//!   before any handler ran. This service words a broken model constraint itself
//!   ([`validation_refusal`]) and keeps the framework's wording for every other
//!   class of refusal.
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

use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::sync::Arc;

use aws_smithy_schema::{Schema, ShapeType};
use aws_smithy_schema_server::Violation;

use crate::ops::types;

/// The error code this service answers a boundary constraint refusal with — the
/// `ValidationException` its model declares on every operation, which is also the
/// code the framework's own default picks on this protocol. It travels with the
/// message below, so a reworded refusal never loses its matching code.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// Every shape of this REQUEST surface that declares a member the model makes a
/// MAP — the shapes a violation path can carry a caller-chosen key out of.
///
/// A step from a collection into one of its elements reaches the dotted path as
/// `<position-or-key>.member` whichever kind of collection it was, so what that
/// step is spelled by is decided by the TYPE the model declares for the member
/// one step up, read off that member's own schema below. These are the schemas
/// the model generated, so no member's type is restated here: a shape is listed,
/// and [`declares_map_member`] keeps the members whose declared type is a map.
///
/// The path names every member it descended through, a union's arm included
/// (`context.contextMap.<key>.member.…`), so the member one step up is always a
/// member of one of these shapes. One list covers every operation because that
/// member is addressed by NAME and no member name this service declares a map is
/// a list anywhere in its model — `tags`, `encryptionContext`, `attributes`,
/// `contextMap` and `record` are maps wherever they appear, and `entityList`,
/// `parents`, `set`, `requests`, `clientIds` and `audiences` are lists wherever
/// they do (`smithy_query(query="shape", shape="TagMap")` and the shapes below).
const SHAPES_DECLARING_A_MAP: &[&Schema] = &[
    // `TagResource$tags` and `CreatePolicyStore$tags`: a resource's tag set,
    // keyed by tag key.
    types::TAG_RESOURCE_INPUT_SCHEMA,
    types::CREATE_POLICY_STORE_INPUT_SCHEMA,
    // `KmsEncryptionSettings$encryptionContext`: the encryption context a
    // policy store is created or updated with, keyed by context key.
    types::KMS_ENCRYPTION_SETTINGS_SCHEMA,
    // `EntityItem$attributes` and `EntityItem$tags`: one entity's attributes and
    // Cedar tags, keyed by attribute and tag name.
    types::ENTITY_ITEM_SCHEMA,
    // `ContextDefinition$contextMap`: an authorization request's context, keyed
    // by context key.
    types::CONTEXT_DEFINITION_SCHEMA,
    // `AttributeValue$record` and `CedarTagValue$record`: a Cedar record nested
    // inside an attribute or a tag value, keyed by field name.
    types::ATTRIBUTE_VALUE_SCHEMA,
    types::CEDAR_TAG_VALUE_SCHEMA,
];

/// Whether the member a dotted path segment names is one the model declares a
/// MAP, so a step into one of its entries is addressed by the key the caller
/// sent rather than by a position.
///
/// The segment is compared against the member name spelled the way the framework
/// spells it in the path it walks constraints with
/// ([`aws_smithy_schema_server::constraints::path::member`]), so the two cannot
/// drift into different casings of one member.
fn declares_map_member(segment: &str) -> bool {
    SHAPES_DECLARING_A_MAP.iter().any(|shape| {
        shape.members().iter().any(|member| {
            matches!(member.shape_type(), ShapeType::Map)
                && member.member_name().is_some_and(|name| {
                    aws_smithy_schema_server::constraints::path::member(name) == segment
                })
        })
    })
}

/// One member of the request as a JSON pointer rooted at the request body, built
/// from the dotted path the framework walks its constraints with.
///
/// The spelling itself — a pointer segment per dotted segment, a collection step
/// spelled by the element alone, a list element renumbered to the 0-based index
/// RFC 6901 gives an array element, a map entry's key kept as sent with `~` and
/// `/` escaped — is the one every service of this tree shares
/// ([`service_common::json_pointer`]). The one fact the dotted path does not
/// carry is whether a collection step entered a list or a map, so this service
/// answers that from its own model ([`declares_map_member`]) and the shared rule
/// turns it into the spelling.
fn json_pointer(dotted: &str) -> String {
    service_common::json_pointer::json_pointer_keyed(dotted, declares_map_member)
}

/// The clause one violation states: the member it is about, addressed as a JSON
/// pointer, and the requirement that member failed — and NOTHING of the value
/// that was submitted.
///
/// The requirement half is the framework's, taken off the clause it renders for
/// this violation rather than rewritten here, so a constraint class it learns to
/// report cannot drift from this wording. Only the `Value '<submitted>' ` /
/// `Value null ` subject is replaced, because this service states no value at
/// all: a member outside its constraint can carry caller data, and the member the
/// clause names is what a caller has to fix.
fn clause(violation: &Violation) -> String {
    let addressed = Violation {
        path: json_pointer(&violation.path),
        kind: violation.kind.clone(),
        value: violation.value.clone(),
        sensitive: violation.sensitive,
    };
    let stated = addressed.message();
    let subject = match &addressed.value {
        Some(submitted) => format!("Value '{submitted}' "),
        None => "Value null ".to_string(),
    };
    match stated.strip_prefix(&subject) {
        Some(requirement_half) => format!("Value {requirement_half}"),
        // The framework worded the clause some other way; it is still a true
        // statement of the refusal, so it is passed through rather than dropped.
        None => stated,
    }
}

/// How this service states the framework's refusal of a request that broke a
/// constraint its model declares: one clause per violation, in the envelope
/// `N validation error(s) detected. <clause>; <clause>`.
///
/// The count is followed by a PERIOD, where the framework's own default follows
/// it with a colon, and each clause addresses its member as a JSON pointer rather
/// than as a dotted path, because that pointer is the machine-readable half of
/// the message — the same spelling this service's own input checks report in
/// `ValidationException$fieldList` ([`crate::ops::paths`]). `None` when there is
/// nothing to state, which keeps the framework's wording.
fn validation_refusal(violations: &[Violation]) -> Option<Refused> {
    let clauses: Vec<String> = violations.iter().map(clause).collect();
    let message = match clauses.as_slice() {
        [] => return None,
        [only] => format!("1 validation error detected. {only}"),
        many => format!(
            "{} validation errors detected. {}",
            many.len(),
            many.join("; ")
        ),
    };
    Some(Refused {
        code: VALIDATION_EXCEPTION.to_string(),
        message,
    })
}

/// The `com-amazonaws-verifiedpermissions-verifiedpermissions` emulated service: the generated [`ServiceDefinition`] plus
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
        // required member, or a value outside a declared bound) to let this
        // service reword it. This service states that class of refusal in its own
        // envelope — see [`validation_refusal`], which carries the matching
        // `ValidationException` code — and keeps the framework's code and wording
        // for every other class, which an unauthenticated caller's 403 is.
        match refusal {
            Refusal::Invalid(violations) => validation_refusal(violations),
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
