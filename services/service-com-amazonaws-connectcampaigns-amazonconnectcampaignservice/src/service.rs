// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-connectcampaigns-amazonconnectcampaignservice` service: how it claims requests and how it words the
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
//!   before any handler ran. It delegates to the shared AWS renderer
//!   ([`service_common::validation_message::aws_validation_refusal_withholding`]), so a
//!   constraint refusal of any operation of this service reads as AWS words it: the
//!   framework's envelope, member paths and requirements, with a `@pattern` quoted without
//!   its whole-value anchors and a clause that states a value only where that value is the
//!   COLLECTION the caller submitted. See the inline note on that method.
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
//! ## The media type this service announces
//!
//! Its PROTOCOL's own, on every reply: the model's only protocol trait is `restJson1`,
//! whose responses carry `application/json`, and the recorded responses of this service
//! carry that value too — a JSON body and a refusal envelope alike. So no seam here
//! re-announces anything and the framework's renderers, which stamp the media type the
//! protocol trait implies, are left to it. The sibling ConnectCampaignsV2 API, whose own
//! recorded wire carries the awsJson 1.1 media type instead, states that for itself in its
//! own crate: the value is a fact of each service's wire, and the two share only a signing
//! name.

use smithy_server_runtime::{http, Refusal, Refused, Service, Violation, ViolationKind};
use std::collections::HashSet;
use std::sync::Arc;

/// How this service renders the value of a LIST member whose `@length` a request broke:
/// the elements it submitted, bracketed, which is the value a list has. The EMPTY list is
/// the one such value a structural refusal determines on its own, because a refusal
/// carries a collection's element COUNT and no elements, and zero elements render as this
/// and nothing else.
const EMPTY_LIST_VALUE: &str = "[]";

/// Whether `violation` is the framework's clause about a LIST member the request EMPTIED —
/// a `@length` broken by a collection the walk rendered as zero items.
///
/// The element count is read back out of the framework's own rendering through
/// [`service_common::validation_message::list_item_count`], so this service and any other
/// that words a list's length refusal read one rendering the same way. A LIST only: a
/// map's own rendering counts ENTRIES, not items, so `list_item_count` does not read it and
/// an empty map keeps the valueless clause. That is deliberate — `[]` is the rendering of a
/// list, and nothing here invents the rendering of another collection kind.
fn empties_a_list(violation: &Violation) -> bool {
    matches!(
        violation.kind,
        ViolationKind::MinLength(_) | ViolationKind::MaxLength(_)
    ) && violation.value.as_deref().is_some_and(|stored| {
        service_common::validation_message::list_item_count(stored) == Some(0)
    })
}

/// `refusal` stated the way this service states a constraint refusal, or `None` for a
/// refusal that is not one.
///
/// The envelope, the member paths and every requirement are the shared AWS renderer's
/// ([`service_common::validation_message::aws_validation_refusal_withholding`]), so a
/// constraint refusal of any operation of this service reads as AWS words it, with a
/// `@pattern` quoted without the anchors a whole-value pattern wraps it in.
///
/// What this service decides is the VALUE half of each clause, and it states that half
/// only where the value is a COLLECTION the caller submitted: a scalar member's value is
/// never echoed back — neither its own bytes nor the framework's elision of them — so the
/// clause names the member and the requirement and nothing else. A member that was ABSENT
/// keeps the framework's `Value null`, which is the one thing that half reports about a
/// value nobody sent, and a collection the refusal carries only a COUNT of has that count
/// dropped too, since a count is not a value a caller sent — except for the one such value
/// a structural refusal determines, the empty list, which is restated as the list it is
/// (see [`empties_a_list`]).
fn stated_refusal(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    let mut stated: Vec<Violation> = Vec::with_capacity(violations.len());
    // The member paths whose clause states no value at all: every one the framework wrote
    // a value half for that is not the collection the caller submitted.
    let mut withheld: HashSet<String> = HashSet::new();
    for violation in violations {
        if empties_a_list(violation) {
            stated.push(Violation {
                value: Some(EMPTY_LIST_VALUE.to_string()),
                ..violation.clone()
            });
            continue;
        }
        if violation.value.is_some() || violation.sensitive {
            withheld.insert(violation.path.clone());
        }
        stated.push(violation.clone());
    }
    service_common::validation_message::aws_validation_refusal_withholding(
        &Refusal::Invalid(stated),
        |path| withheld.contains(path),
    )
}

/// The media type this service announces on every reply: the one its `restJson1` protocol
/// trait implies, which is also the one its recorded wire carries (see the module header).
/// Named here because the suites pin it, and stamped by the framework's own renderers
/// rather than by any seam of this file.
pub const RESPONSE_MEDIA_TYPE: &str = "application/json";

/// Whether this service serves the request these `parts` came from.
///
/// `emulator_server::can_handle_based_on_route_key_and_path` is the agent-immutable
/// owner of this rule, and it is CALLED here rather than re-implemented: the request's
/// route key must match this service's signing name AND its first URI path segment
/// must be one the generated, model-derived `ROUTE_PATH_SEGMENTS` declares, by exact
/// segment equality. That path conjunct is what separates two services sharing one
/// SigV4 signing name — ConnectCampaigns v1 and ConnectCampaignsV2 both sign as
/// `connect-campaigns`, so the key alone matches both and the router rejects every
/// such request as `500 AmbiguousService` instead of serving either one — and their
/// modelled path spaces are disjoint: v1 publishes `campaigns`, `campaigns-state`,
/// `campaigns-summary`, `connect-instance` and `tags` here, while every V2 route
/// lives under `v2` there.
///
/// The helper takes a whole `http::Request` while [`Service::can_handle`] is handed
/// only the request's parts, so the two things it reads — the URI and the headers —
/// are re-presented to it as a bodiless request. It reads neither the body nor the
/// method, so nothing it decides on is missing.
fn can_handle_parts(parts: &http::request::Parts) -> bool {
    let Ok(mut request) = http::Request::builder()
        .uri(parts.uri.clone())
        .body(bytes::Bytes::new())
    else {
        return false;
    };
    *request.headers_mut() = parts.headers.clone();
    emulator_server::can_handle_based_on_route_key_and_path(
        &request,
        crate::ROUTE_KEY,
        crate::ROUTE_PATH_SEGMENTS,
    )
}

/// The `com-amazonaws-connectcampaigns-amazonconnectcampaignservice` emulated service: the generated [`ServiceDefinition`] plus
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
        // Delegated to the framework's own predicate — see [`can_handle_parts`]:
        // this service's signing name (`connect-campaigns`) is shared with the
        // ConnectCampaignsV2 API, so the claim is the route key AND the
        // model-derived first URI path segment (v1's unprefixed route space).
        // Matching on the signing name alone would have both services claim every
        // `connect-campaigns`-signed request, which the router answers with 500
        // `AmbiguousService` instead of serving either one.
        can_handle_parts(parts)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) so this service can
        // state it the way the service it emulates states it, which no model records.
        //
        // The whole wording is delegated to [`stated_refusal`], which delegates in turn to
        // the shared AWS renderer: the envelope, the member paths and every requirement
        // stay the framework's, and the renderings AWS does differently are applied to
        // every constrained member of every operation of this service alike — a
        // `@pattern` requirement quoted without the anchors a whole-value pattern wraps it
        // in, and a clause that states a value only where that value is the COLLECTION the
        // caller submitted. A refusal that is not a constraint violation — an
        // unauthenticated caller — keeps the framework's code and wording.
        stated_refusal(refusal)
    }
}
