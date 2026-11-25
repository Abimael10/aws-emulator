// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-dynamodbstreams-dynamodbstreams-20120810` service: how it claims requests and how it words the
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

use aws_smithy_schema::Schema;
use aws_smithy_schema_server::constraints::{path, LengthTrait, Violation, ViolationKind};
use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::sync::Arc;

mod validation;

/// The service key `::emulator_server::service_from_target` reads out of the
/// `X-Amz-Target` header this service's awsJson1_0 operations carry: that reader
/// takes the segment before the `.`, strips the `_<version>` suffix and
/// lower-cases it, so `DynamoDBStreams_20120810.<OperationName>` (`OPERATIONS.md`,
/// "Request routing") normalises to this key — and DynamoDB's own target
/// normalises to `dynamodb` instead.
const TARGET_SERVICE_KEY: &str = "dynamodbstreams";

/// The `com-amazonaws-dynamodbstreams-dynamodbstreams-20120810` emulated service: the generated [`ServiceDefinition`] plus
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
        // This service and DynamoDB are two services behind ONE SigV4 signing name
        // (`dynamodb`), which the model cannot express, so the default signing-name
        // match makes BOTH claim every DynamoDB-signed request and the router answers
        // `500 AmbiguousService` for all of it. On awsJson1_0 the request names its
        // service in `X-Amz-Target` (`DynamoDBStreams_20120810.<Operation>`), so that
        // prefix is the discriminator; DynamoDB's own `can_handle` declines this
        // prefix for the same reason.
        match smithy_server_runtime::resolve_route_key(parts) {
            Some(key) if key == self.entry.identity().signing_name() => {
                ::emulator_server::service_from_target(&parts.headers).as_deref()
                    == Some(TARGET_SERVICE_KEY)
            }
            _ => false,
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
        // Three refusals of this service are worded by the service rather than by the
        // framework, and none carries anything of the request inside it. A request that
        // OMITS the identifier it addresses draws that identifier's own sentence
        // ([`absent_identifier_refusal`]), so it is asked first: the request named no
        // position at all, which is the same fault as naming one of the wrong form and
        // not a constraint on a value. A TABLE NAME the model's own constraints do not
        // admit draws a single sentence stating the window a name has to fit, with no
        // constraint envelope around it ([`table_name_refusal`]). Every other broken
        // requirement draws the constraint envelope naming the member and the
        // requirement alone ([`validation::refused`]). Authentication keeps the
        // framework's wording: it is not an input failure, and its code and status
        // are not this service's to change.
        let Refusal::Invalid(violations) = refusal else {
            return None;
        };
        absent_identifier_refusal(violations)
            .or_else(|| table_name_refusal(violations))
            .or_else(|| validation::refused(refusal))
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

/// The error code a refused request carries, whichever member it was refused over: a
/// client branches on the code, and this service answers one code for every input
/// refusal, so rewording a message never moves a caller onto another branch.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The identifiers this service's operations ADDRESS a position with, each paired with
/// the one sentence a request that does not name it draws.
///
/// Each sentence is the service's own, stated once in [`crate::state`] and answered
/// wherever the identifier is judged: the handlers answer it for a value that is not
/// written as that identifier, and this front door answers the same sentence for a
/// request that carried no value at all — one fault with one wording, because neither
/// request names a position this service could read. They are keyed on the SHAPE the
/// member targets, which is what the sentence names, so every member of that shape is
/// refused alike; the members this model declares for them are named after it
/// (`DescribeStream$StreamArn`, `GetShardIterator$StreamArn`, `GetRecords$ShardIterator`).
///
/// An identifier whose malformed value this service does NOT state a sentence of — a
/// shard id, an iterator type — is absent from this table, so an omitted one keeps the
/// framework's constraint envelope.
const ADDRESSING_IDENTIFIERS: [(&str, &str); 2] = [
    ("StreamArn", crate::state::INVALID_STREAM_ARN),
    ("ShardIterator", crate::state::INVALID_SHARD_ITERATOR),
];

/// How this service refuses a request that NAMES NOTHING: a member carrying one of the
/// identifiers above was not in the request at all.
///
/// `None` for a refusal that names no such member, which falls through to the wordings
/// below. Nothing here is keyed to an operation: the member path the framework's walk
/// reports is matched against the model's own member names, so an operation this model
/// gains that addresses a stream or a shard iterator is refused the same way.
fn absent_identifier_refusal(violations: &[Violation]) -> Option<Refused> {
    let message = violations.iter().find_map(|violation| {
        if !matches!(violation.kind, ViolationKind::MissingRequiredMember) {
            return None;
        }
        addressing_identifier(&violation.path)
    })?;
    Some(Refused {
        code: VALIDATION_EXCEPTION.to_string(),
        message: message.to_string(),
    })
}

/// The sentence the member refused at `refused_at` draws, if that member carries one of
/// the identifiers this service addresses by.
///
/// The path is compared against the one the framework spells for the model's own member
/// ([`path::member`]), rather than against text written out here, so the comparison
/// cannot drift from either the model or the framework's walk.
fn addressing_identifier(refused_at: &str) -> Option<&'static str> {
    ADDRESSING_IDENTIFIERS
        .iter()
        .copied()
        .find_map(|(identifier, message)| {
            let member = input_member(identifier)?.member_name()?;
            (path::member(member) == refused_at).then_some(message)
        })
}

/// The member of one of this service's inputs named `name`, as the MODEL declares it.
fn input_member(name: &str) -> Option<&'static Schema> {
    INPUT_SCHEMAS
        .iter()
        .copied()
        .flat_map(|input| input.members().iter().copied())
        .find(|member| member.member_name() == Some(name))
}

/// Every input shape this service reads a request into, so a rule stated over its
/// members covers the whole surface rather than one operation.
const INPUT_SCHEMAS: [&Schema; 4] = [
    crate::ops::types::DESCRIBE_STREAM_INPUT_SCHEMA,
    crate::ops::types::GET_RECORDS_INPUT_SCHEMA,
    crate::ops::types::GET_SHARD_ITERATOR_INPUT_SCHEMA,
    crate::ops::types::LIST_STREAMS_INPUT_SCHEMA,
];

/// How this service refuses a TABLE NAME its model's own constraints do not admit:
/// one sentence naming the member and the length window a name has to fit, and
/// nothing else.
///
/// The sentence states the REQUIREMENT rather than the measurement, so it is the same
/// for every value the member rejects — too short, too long, or built of characters
/// the name's pattern does not admit (an ARN among them, which carries separators no
/// table name may). Nothing of the request is inside it: a caller already holds the
/// value it sent, and a refusal that quotes it reflects the request back into the
/// response.
///
/// `None` for a refusal that names no table name, which falls through to
/// [`validation::refused`]. Only `ListStreams` carries a table name among this
/// service's inputs, so the member resolves to one schema and one pair of bounds.
fn table_name_refusal(violations: &[Violation]) -> Option<Refused> {
    let member = table_name_member()?;
    let name = member.member_name()?;
    // The path the framework's walk reports for that member, spelled by the framework
    // rather than written out here, so the one spelling cannot drift from the model.
    let refused_at = path::member(name);
    violations.iter().find(|violation| {
        violation.path == refused_at
            && matches!(
                violation.kind,
                ViolationKind::MinLength(_) | ViolationKind::MaxLength(_) | ViolationKind::Pattern(_)
            )
    })?;
    let bounds = declared_length(member)?;
    let (min, max) = (bounds.min?, bounds.max?);
    Some(Refused {
        code: VALIDATION_EXCEPTION.to_string(),
        message: format!(
            "{name} must be at least {min} characters long and at most {max} characters long"
        ),
    })
}

/// The member of the one input that carries a table name, as the MODEL declares it —
/// so the sentence above names the member and its bounds as the API states them
/// rather than restating either here.
fn table_name_member() -> Option<&'static Schema> {
    crate::ops::types::LIST_STREAMS_INPUT_SCHEMA
        .members()
        .iter()
        .copied()
        .find(|member| member.member_name() == Some(TABLE_NAME_MEMBER))
}

/// The name the model gives the member that carries a table name.
const TABLE_NAME_MEMBER: &str = "TableName";

/// The `@length` bounds the model attaches to `member`, if it bounds it at all.
fn declared_length(member: &Schema) -> Option<LengthTrait> {
    let carried = member.traits()?.get(&LengthTrait::TRAIT_ID)?;
    carried.as_any().downcast_ref::<LengthTrait>().copied()
}
