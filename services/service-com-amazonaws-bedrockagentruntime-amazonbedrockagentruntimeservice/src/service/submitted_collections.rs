// SPDX-License-Identifier: CC0-1.0

//! The one half of a constraint refusal of this service that the framework's validating
//! walk cannot state: the COLLECTION a clause about a collection member is about.
//!
//! # The rule
//!
//! A constraint clause quotes the value it rejected — `Value '<submitted>' at '<member
//! path>' failed to satisfy constraint: <requirement>` — and that value is the one place a
//! refusal reports the request's own input back, so a client comparing it against what it
//! sent has to see its own input. A `@length` on a LIST or a MAP is checked ON THE MEMBER,
//! before the walk descends into the elements, and the only rendering the framework has for
//! the rejected value there is a summary of its SIZE (`<n> items`, `<n> entries`), which is
//! not a value any caller sent (`vendor/smithy-schema-server/src/constraints.rs`). So the
//! clause quotes the collection itself, rendered the way a value of that shape prints: a
//! list as `[<element>, …]`, a map as `{<key>=<value>, …}`, a structure as
//! `<Shape>(<member>=<value>, …)` over every member its shape declares with one the request
//! left unset as `null`, and a union over the one member it carries.
//!
//! That is the whole difference from the refusal this service already words
//! ([`crate::service::EmulatedService::override_refusal`]): the member path, the
//! requirement, the `@pattern` rendering, the error code and the counting envelope stay
//! where they are, and a clause about anything else keeps the value the framework stored. A
//! member the model marks `@sensitive` keeps the valueless clause too: the shared walk
//! renders no such value, and no composite holding one, so the collection a clause quotes
//! cannot disclose one.
//!
//! # Why a layer per operation
//!
//! [`Service::override_refusal`] is handed the violations alone and never the request they
//! were found in, so the collection cannot be recovered there. The layers that DO hold the
//! request are this one — [`QuotingSubmittedCollections`], which wraps each operation the
//! generated routing table serves — and the six hand-authored event-stream operations, which
//! read their own request and reach the same rule through the ONE seam
//! [`quoted_refusal_response`]. Both end in `Service::override_refusal`, so every operation of
//! this service words one refusal the same way.
//!
//! # Why not the tree's shared wrapper
//!
//! `service_common::submitted_collections` carries a wrapper of the same shape, and a service
//! whose refusal it can RENDER uses that one. It cannot render this service's: it words the
//! refusal itself, from the framework's clause over each violation, where this service states
//! a `@pattern` requirement as the expression its own published constraint spells
//! ([`crate::service::EmulatedService::override_refusal`]) — so a refusal breaking a bound AND
//! a pattern at once would lose the pattern half. The wrapper here therefore makes no wording
//! decision at all: it hands the violations to that one seam. Everything else — the walk, the
//! restoring rule, the clause, the envelope, the status and the wire body — is the shared
//! crate's and the framework's.
//!
//! Nothing is re-implemented and no check is repeated in other words: the violations and the
//! renderings come from the tree's shared collecting walk
//! (`service_common::validation::refusing_violations_with_shaped_echoes_over`, which runs the
//! protocol's own deserializer and the framework's own `constraints::validate` and owns every
//! bound on what one clause may quote), the restoring rule is
//! `service_common::validation_message::quoting_the_submitted_collection`, the clause and the
//! envelope are the framework's own, and the status and the wire body come from the
//! framework's refusal channel. The cost is DELIBERATE: a request's body is read twice, here
//! and in the operation's own sequence, because the framework hands no already-built input to
//! an operation.

use std::sync::Arc;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::SerializableStruct;
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::ProtocolError;
use service_common::validation::{
    refusing_input_violations_with_shaped_echoes_over,
    refusing_violations_with_shaped_echoes_over, DeclaringShapes, SubmittedEcho,
};
use service_common::validation_message::quoting_the_submitted_collection;
use smithy_server_runtime::{
    BoxFuture, DeserializableStruct, ErasedOperation, Operation, OperationContext,
    OperationRoutingTable, Refusal, ServerHttpProtocol, Service, Violation,
};

use crate::ops::types::{ApiResult, ContentBody, FunctionResult};

/// The shapes a quoted collection of this model reaches as a struct MEMBER or a map VALUE,
/// whose members are therefore read off the shape's own schema rather than off the member
/// schema the value was written against (`service_common::validation::DeclaringShapes` states
/// why a member schema cannot answer for them).
///
/// The one `@length`-bounded collection of this model that holds structures and is not
/// `@sensitive` is `returnControlInvocationResults` (`@length` 1..5 of
/// `InvocationResultMember`, `smithy_query(query="shape",shape="InlineSessionState")`), whose
/// two union arms are `ApiResult` and `FunctionResult`, each holding a `responseBody` map of
/// `ContentBody` — so these three are every structure such a clause renders. The member NAMES
/// and their ORDER are the model's, read off the generated schemas; a shape not named here
/// renders over the members the request set, which is the shared walk's own fail-safe.
const DECLARING_SHAPES: DeclaringShapes = &[
    &ApiResult::SCHEMA,
    &ContentBody::SCHEMA,
    &FunctionResult::SCHEMA,
];

/// The member positions the echo-collecting walk may visit for one request of this service,
/// stated from this model rather than taken from the shared default (256).
///
/// A collection is quoted only when every one of its elements was walked. The largest
/// collection a clause of this service quotes is a tagging one — `tags` (`@length` 1..200
/// entries) and `tagKeys` (`@length` 1..200), `smithy_query(query="shape",shape="TagsMap")`
/// and `smithy_query(query="operation",operation="UntagResource")` — so the ceiling has to
/// cover the 201 entries that break either bound, while the six return-control results that
/// break the 1..5 bound spend about ten positions each. Past this the framework's own count
/// summary stands, which is the fail-safe direction: `sources` (`@length` 1..1000 of a
/// structure) is not quoted at all rather than quoted short — and it is `@sensitive`, so no
/// clause of this service quotes it anyway.
const MAX_ECHO_POSITIONS: usize = 1024;

/// One violation as this service states it: a clause about a COLLECTION member quotes the
/// collection the request carried, through the tree's one home for that restoring rule.
///
/// Every other clause is returned untouched — so the only thing this adds to the refusal is
/// the value half the framework had no rendering for. A member whose value this service does
/// not disclose needs nothing here: the shared walk renders no `@sensitive` value and no
/// composite holding one, so such a clause arrives carrying the framework's own count summary
/// and keeps the valueless form.
fn stated(violation: Violation, echo: SubmittedEcho) -> Violation {
    quoting_the_submitted_collection(violation, echo)
}

/// `refused` with every clause about a COLLECTION member quoting the collection `req`
/// carried — for a HAND-AUTHORED operation, which read its own request because the generated
/// sequence cannot carry an event-stream body.
///
/// `T` and `schema` are the input type and schema that operation read with, so this is the
/// same walk over the same request the violations came from. A walk that finds a different
/// number of violations keeps `refused` untouched: the two cannot disagree (both are the
/// framework's deserializer plus `constraints::validate` over one request), and if they ever
/// did, the refusal the operation already holds is the one to answer with.
fn quoting_submitted_collections_of<T>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
    schema: &Schema,
    refused: Vec<Violation>,
) -> Vec<Violation>
where
    T: DeserializableStruct + SerializableStruct,
{
    let quoted: Vec<Violation> = refusing_input_violations_with_shaped_echoes_over::<T>(
        protocol,
        req,
        schema,
        MAX_ECHO_POSITIONS,
        DECLARING_SHAPES,
    )
    .into_iter()
    .map(|(violation, echo)| stated(violation, echo))
    .collect();
    if quoted.len() == refused.len() {
        quoted
    } else {
        refused
    }
}

/// The ONE refusal seam of this service's hand-authored operations: the response a request
/// `T`'s own constraints refused, with every clause about a COLLECTION member quoting the
/// collection `req` carried.
///
/// Each of the six `@streaming`-output operations is served by hand, because the generated
/// sequence cannot carry an event-stream body (`service_common::event_stream`), and each reads
/// its own request — so each hands the violations it was given here rather than wording a
/// refusal of its own. The wording is [`Service::override_refusal`]'s and the status and the
/// wire body are the framework's refusal channel's, exactly as they are for an operation the
/// generated table serves, so one request breaking one bound is refused identically whichever
/// of the two served it.
pub(crate) fn quoted_refusal_response<T>(
    svc: &dyn Service,
    req: &RtRequest,
    schema: &Schema,
    refused: Vec<Violation>,
) -> RtResponse
where
    T: DeserializableStruct + SerializableStruct,
{
    let definition = svc.definition();
    let quoted =
        quoting_submitted_collections_of::<T>(definition.protocol(), req, schema, refused);
    let chosen = svc.override_refusal(&Refusal::Invalid(quoted.clone()));
    definition.refusal_response(ProtocolError::Invalid(quoted), chosen, req)
}

/// One modelled operation of the generated routing table, served so that a refusal of a
/// `@length` on one of its COLLECTION members states the collection the caller submitted.
///
/// It makes no routing decision and holds no state of its own: the operation it wraps is the
/// one the generated table names for the same key, built from the SAME shared engine, and a
/// request this layer does not refuse is handed to it untouched — so the framework still owns
/// the whole deserialize -> validate -> handle -> serialize sequence and every status, shape
/// and message it answers with.
struct QuotingSubmittedCollections<Op> {
    operation: Op,
}

impl<Op> QuotingSubmittedCollections<Op> {
    /// Wrap one operation, already constructed from the shared engine.
    fn new(operation: Op) -> Self {
        Self { operation }
    }
}

impl<Op> ErasedOperation for QuotingSubmittedCollections<Op>
where
    Op: Operation + ErasedOperation,
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // The whole refusal is decided — and the borrow of the definition it is rendered
            // through taken and dropped — BEFORE the await below:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding one across that await
            // would make this future non-`Send`.
            if let Some(refused) = quoting_refusal::<Op>(svc, req) {
                return refused;
            }
            self.operation.run(svc, req, ctx).await
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not change
    /// which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.operation.auth_schemes()
    }
}

/// The refusal `req` earns from `Op`'s own constraints, worded by this service
/// ([`Service::override_refusal`]) over clauses whose collection halves are stated, or `None`
/// when the request breaks none of them and the operation is to serve it.
fn quoting_refusal<Op: Operation>(svc: &dyn Service, req: &RtRequest) -> Option<RtResponse> {
    let quoted: Vec<Violation> = refusing_violations_with_shaped_echoes_over::<Op>(
        svc.definition().protocol(),
        req,
        MAX_ECHO_POSITIONS,
        DECLARING_SHAPES,
    )
    .into_iter()
    .map(|(violation, echo)| stated(violation, echo))
    .collect();
    if quoted.is_empty() {
        return None;
    }
    let chosen = svc.override_refusal(&Refusal::Invalid(quoted.clone()));
    Some(
        svc.definition()
            .refusal_response(ProtocolError::Invalid(quoted), chosen, req),
    )
}

/// Wire one `crate::ops::<module>::<Operation>` per line into `table`, each wrapped in
/// [`QuotingSubmittedCollections`] and keyed by its operation shape name — the routing key
/// `ServerHttpProtocol::routing_key` resolves and the generated table is keyed by.
macro_rules! wrap_operations {
    ($table:expr, $engine:expr, $($module:ident :: $operation:ident),+ $(,)?) => {
        $(
            $table.add_operation(
                stringify!($operation),
                ::std::boxed::Box::new(QuotingSubmittedCollections::new(
                    crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
                )) as ::std::boxed::Box<dyn ErasedOperation>,
            );
        )+
    };
}

/// Every modelled operation the generated routing table serves, each behind the layer above
/// and built from the SAME shared engine, so a wrapped operation reads and writes the one
/// store.
///
/// EVERY one of them, rather than the few whose model declares a bounded collection: which
/// operations pay for the second read is a choice, and one table over all of them is what
/// makes the rule hold for this whole service rather than for a list of operation names that
/// a later model refresh could leave behind. An operation this list did miss would still be
/// ROUTED and served: [`crate::service::EmulatedService::override_operation`] declines a key
/// this table does not hold, which leaves the request to the generated table exactly as
/// before, with the framework's own count summary in its clause. The six `@streaming`-output
/// operations are not here — the generated table cannot serve one at all (`crate::service`
/// routes each to its hand-authored handler), and each reaches the same rule through
/// [`quoted_refusal_response`].
pub(super) fn quoting_submitted_collections<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
) -> OperationRoutingTable {
    let mut table = OperationRoutingTable::new();
    wrap_operations!(
        table,
        engine,
        check_ingested_document_acl::CheckIngestedDocumentAcl,
        create_invocation::CreateInvocation,
        create_session::CreateSession,
        delete_agent_memory::DeleteAgentMemory,
        delete_session::DeleteSession,
        end_session::EndSession,
        generate_query::GenerateQuery,
        get_agent_memory::GetAgentMemory,
        get_document_content::GetDocumentContent,
        get_execution_flow_snapshot::GetExecutionFlowSnapshot,
        get_flow_execution::GetFlowExecution,
        get_ingested_document_acl::GetIngestedDocumentAcl,
        get_invocation_step::GetInvocationStep,
        get_session::GetSession,
        list_flow_execution_events::ListFlowExecutionEvents,
        list_flow_executions::ListFlowExecutions,
        list_invocation_steps::ListInvocationSteps,
        list_invocations::ListInvocations,
        list_sessions::ListSessions,
        list_tags_for_resource::ListTagsForResource,
        put_invocation_step::PutInvocationStep,
        rerank::Rerank,
        retrieve::Retrieve,
        retrieve_and_generate::RetrieveAndGenerate,
        start_flow_execution::StartFlowExecution,
        stop_flow_execution::StopFlowExecution,
        tag_resource::TagResource,
        untag_resource::UntagResource,
        update_session::UpdateSession,
    );
    table
}
