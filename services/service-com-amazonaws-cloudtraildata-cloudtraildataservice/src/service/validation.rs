// SPDX-License-Identifier: CC0-1.0

//! How this service states a request it refused BEFORE any handler ran: the operation
//! wrapper that re-reads the request so a clause about a COLLECTION member can quote the
//! collection the caller submitted.
//!
//! Why a wrapper at all: [`smithy_server_runtime::Service::override_refusal`] is handed
//! the violations alone and never the request they were found in, so the one clause whose
//! value the framework's validating walk could not render — a list has no scalar
//! rendering, and the walk stores its own `<n> items` summary of one, which no caller ever
//! sent — cannot be completed there. Here the input can be read again.
//!
//! Nothing is re-implemented: the violations and the value echoes come from the tree's
//! shared collecting walk (`service_common::validation`, which runs the protocol's own
//! deserializer and the framework's own constraint check and owns every bound on what one
//! clause may quote), each clause's restatement from `service_common::validation_message`,
//! the envelope and every other clause from the framework, and the status and the error
//! body from the framework's refusal channel. A request this wrapper does not refuse is
//! handed to the generated operation, which owns deserialize -> validate -> handle ->
//! serialize and every modelled error.
//!
//! This wrapper is itself served behind the identity-based decision IAM takes in front of
//! the service ([`super::authorization::Authorized`]), so every refusal stated here is
//! stated to a caller that decision allowed: a clause restating what the request submitted
//! can never reach a principal no statement allows.
//!
//! A request this wrapper ANSWERS is a request this service answered, so it emits that
//! request's access record ([`crate::ops::put_audit_events::record_access`]) before
//! answering it, exactly as the layer in front of it and the handler behind it do for the
//! requests they answer: the record is emitted once per answered request, by whichever
//! layer answered it. Which requests those are is decided here rather than left to what the
//! walk happens to be able to describe: the channel reference is held to its member's
//! modelled expression before the payload is read, and a body the codec cannot build the
//! input from at all is answered here too — the operation behind this wrapper would fail
//! the same read, so leaving such a request to it answers it with no layer of this service
//! aware it was answered.

use super::channel_arn_refusal;
use crate::ops::put_audit_events::{record_access, CHANNEL_ARN_MEMBER, CHANNEL_ARN_PATTERN};
use aws_smithy_schema::serde::SerdeError;
use service_common::validation::SubmittedEcho;
use service_common::validation_message::quoting_the_submitted_collection;
use smithy_server_runtime::{ErasedOperation, Operation, Refused, Service, Violation};

/// The error code the framework's own default names a refusal it words itself with on this
/// service's JSON protocol — a constraint refusal and a body its codec could not read
/// alike — restated here because [`Refused`] replaces the code and the message together,
/// and because the access record of a request answered in that wording states the code the
/// caller was answered under.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The member positions one element of the batch spends: one per member `AuditEvent`
/// declares — id, eventData, eventDataChecksum
/// (`smithy_query(query="shape", shape="AuditEvent")`) — and one for the structure itself.
const ECHO_POSITIONS_PER_EVENT: usize = 4;

/// The members `PutAuditEventsRequest` declares, each a position of its own: the batch, the
/// channel reference and the external id
/// (`smithy_query(query="operation", operation="PutAuditEvents")`).
const REQUEST_MEMBERS: usize = 3;

/// The largest batch the shared echo-collecting walk renders at all: it renders a composite
/// from at most 256 children and keeps no rendering for one holding more, so a batch past
/// this is stated with the framework's own count summary whatever this service allows. The
/// other bound on the same rendering is its SIZE, which this service states separately
/// ([`MAX_QUOTED_BATCH`]), so a batch of this many very large events is the case that bound
/// answers.
const LARGEST_RENDERED_BATCH: usize = 256;

/// The member POSITIONS the echo-collecting walk may visit for one request of this
/// service, stated from this model rather than taken from that walk's own default.
///
/// A collection is quoted in a clause only when every one of its elements was walked, and
/// the clause this service states about the `auditEvents` member quotes the batch the
/// request carried — at EVERY size that breaks the member's `@length` maximum (100:
/// `smithy_query(query="shape", shape="AuditEvents")`), not only at the first size past it,
/// since a caller chooses how far past the bound its batch runs. So the ceiling covers
/// every batch that can be rendered at all: [`LARGEST_RENDERED_BATCH`] elements at
/// [`ECHO_POSITIONS_PER_EVENT`] positions each, plus the request's own members. Past this
/// the framework's own count summary stands, which is the fail-safe direction: a batch
/// larger than any the walk renders is not quoted at all rather than quoted short.
const MAX_ECHO_POSITIONS: usize =
    ECHO_POSITIONS_PER_EVENT * LARGEST_RENDERED_BATCH + REQUEST_MEMBERS;

/// The largest batch rendering one clause of this service quotes.
///
/// A constraint clause of this service quotes the value the request carried, and for the
/// `auditEvents` member that value is the batch itself — so a ceiling able to quote the
/// refusal of the largest batch this operation documents has to sit above that batch's own
/// size. `PutAuditEvents` documents "You can add up to 100 of these events (or up to 1 MB)
/// per PutAuditEvents request" (`smithy_query(query="doc", shape="PutAuditEvents")`), so
/// the batch a clause has to quote is bounded by that 1 MB plus the punctuation and member
/// names the rendering writes around it. Twice the documented maximum, so a batch a little
/// past it is still quoted whole while one clause can never carry request-sized content: a
/// MODEL constant rather than a request-sized one. Past it the framework's own count
/// summary stands, which is the fail-safe direction.
const MAX_QUOTED_BATCH: usize = 2 * 1024 * 1024;

/// What the access record of a request refused for its channel reference states: the value
/// the request carried is outside the expression every channel reference is modelled with,
/// so it names no channel at all.
const INVALID_CHANNEL_ARN_DETAIL: &str =
    "channelArn is outside the pattern a channel reference is modelled with";

/// What the access record of every other pre-handler refusal states: the request broke one
/// of the model's own constraints, so nothing of its payload was read and nothing was
/// persisted.
const CONSTRAINT_DETAIL: &str = "the request is outside a modelled constraint";

/// What the access record of a request whose body this service's codec could not read at
/// all states: no member of the payload was built, so nothing of it was validated and
/// nothing was persisted.
const UNREADABLE_BODY_DETAIL: &str = "the request body is not one this protocol's codec can read";

/// One modelled operation, served so that a refusal found before its handler runs is
/// stated the way this service states one.
pub(super) struct StatingSubmittedValues<Op> {
    operation: Op,
}

impl<Op> StatingSubmittedValues<Op> {
    /// Wrap one operation, already constructed from the shared engine.
    pub(super) fn new(operation: Op) -> Self {
        Self { operation }
    }
}

impl<Op> ErasedOperation for StatingSubmittedValues<Op>
where
    Op: Operation + ErasedOperation,
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r aws_smithy_runtime_api::http::Request,
        ctx: &'r smithy_server_runtime::OperationContext<'r>,
    ) -> smithy_server_runtime::BoxFuture<'r, aws_smithy_runtime_api::http::Response> {
        Box::pin(async move {
            // The channel reference comes FIRST, and before the payload is read at all: it
            // is read off the request's own query binding and held to the member's modelled
            // expression here, so this service answers that fault under the error its model
            // declares for it whatever the request's body carried — including a body no
            // codec can build an input from, which leaves the walk below no member to
            // report a violation at.
            if let Some(violation) = channel_arn_violation(req) {
                let collected = vec![(violation, SubmittedEcho::Unrendered)];
                if let Some(refusal) = refusal_of(&collected) {
                    return answered(svc, req, ctx, collected, refusal);
                }
            }
            // The protocol is read, used and dropped inside this one statement:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the await
            // below would make this future non-`Send`.
            let read: Result<Vec<(Violation, SubmittedEcho)>, SerdeError> =
                ::service_common::validation::refusing_violations_with_declared_echoes_rendering_up_to_or_unreadable::<Op>(
                    svc.definition().protocol(),
                    req,
                    MAX_ECHO_POSITIONS,
                    MAX_QUOTED_BATCH,
                );
            let collected = match read {
                Ok(collected) => collected,
                // A body the codec could not build the input from at all is refused by
                // nothing the walk can name — no member of it was built — and the operation
                // behind this wrapper would fail the SAME read, so the request is answered
                // here instead: with the framework's own wording for a codec failure, which
                // is the answer such a request already had, and with this request's access
                // record, which the layer that answers a request is the layer that emits.
                Err(unreadable) => {
                    record_access(
                        ctx,
                        &super::authorization::judged_resource(req),
                        VALIDATION_EXCEPTION,
                        UNREADABLE_BODY_DETAIL,
                    );
                    return svc.definition().refusal_response(
                        aws_smithy_schema_server::ProtocolError::Serde(unreadable),
                        None,
                        req,
                    );
                }
            };
            if let Some(refusal) = refusal_of(&collected) {
                return answered(svc, req, ctx, collected, refusal);
            }
            self.operation.run(svc, req, ctx).await
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not
    /// change which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.operation.auth_schemes()
    }
}

/// How this wrapper ANSWERS a request it refused: the access record every request this
/// service answers emits, and then the refusal itself in the framework's own envelope under
/// the code and sentence this service chose for it.
///
/// One place for both, so no refusal this layer states can be answered without being
/// recorded: the layer that answers a request is the layer that records it, and the channel
/// the record names is the reference the REQUEST carried, read the way the layer in front of
/// this one reads it.
fn answered(
    svc: &dyn Service,
    req: &aws_smithy_runtime_api::http::Request,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    collected: Vec<(Violation, SubmittedEcho)>,
    PreHandlerRefusal { refused, detail }: PreHandlerRefusal,
) -> aws_smithy_runtime_api::http::Response {
    record_access(
        ctx,
        &super::authorization::judged_resource(req),
        &refused.code,
        detail,
    );
    let violations: Vec<Violation> = collected
        .into_iter()
        .map(|(violation, _)| violation)
        .collect();
    svc.definition().refusal_response(
        aws_smithy_schema_server::ProtocolError::Invalid(violations),
        Some(refused),
        req,
    )
}

/// The violation a channel reference the member's modelled expression does not admit IS, or
/// `None` for a request whose reference that expression admits or which binds none at all.
///
/// The value is the one the request's query BINDS ([`super::authorization`] reads it the same
/// way for the decision in front of this layer), and the check is the model's own expression
/// evaluated by the engines every request of this tree is held to a `@pattern` by
/// (`service_common::unapplied_patterns`), so the requirement this states is the one the
/// validating walk states for the same value — no second reading of the expression and no
/// second spelling of the fault.
fn channel_arn_violation(req: &aws_smithy_runtime_api::http::Request) -> Option<Violation> {
    let bound = super::authorization::channel_reference_bound(req.uri())?;
    if ::service_common::unapplied_patterns::modelled_pattern_admits(CHANNEL_ARN_PATTERN, &bound) {
        return None;
    }
    Some(Violation {
        path: ::aws_smithy_schema_server::constraints::path::member(CHANNEL_ARN_MEMBER),
        kind: ::smithy_server_runtime::ViolationKind::Pattern(CHANNEL_ARN_PATTERN.to_string()),
        // Quoted under the one ceiling this tree quotes a submitted value to, as the walk's
        // own clauses are: this refusal's sentence carries no caller value, but a violation
        // that states one must not state an unbounded one.
        value: Some(::service_common::validation::quoted_echo(&bound).into_owned()),
        sensitive: false,
    })
}

/// One refusal this wrapper states: the error the wire carries, and what this request's
/// access record states about why it was refused.
struct PreHandlerRefusal {
    /// The code and the message the refusal is answered under.
    refused: Refused,
    /// The `detail` of the access record emitted for it.
    detail: &'static str,
}

/// How this service answers the violations `collected` from one request, or `None` when
/// there are none and the request is the operation's to serve.
///
/// A channel reference comes FIRST, whatever else the request carried: this service
/// resolves every call against the channel the request names before it reads the payload,
/// so a value that is not a channel reference is answered under the error its model
/// declares for that fault rather than as one constraint violation among several. That
/// decision, and the answer it carries, are taken by [`super::channel_arn_refusal`] —
/// the one place this service builds that refusal, shared with the
/// [`Service::override_refusal`] seam, so a refusal of the same fault cannot be stated
/// two ways. The reference is held to that expression before the payload is read at all
/// ([`channel_arn_violation`]); this path states the same rule for such a violation
/// wherever the validating walk reports one.
///
/// Everything else is the constraint envelope AWS states on this service's JSON protocol,
/// with the two renderings it differs from the framework in — a `@pattern` requirement
/// quoting the expression without the whole-value anchors the model wraps it in, and a
/// collection's `@length` clause quoting the collection the request carried.
///
/// Each refusal carries the detail its access record states beside the wire error, so the
/// one decision about how a refusal is classed also decides how it is recorded.
fn refusal_of(collected: &[(Violation, SubmittedEcho)]) -> Option<PreHandlerRefusal> {
    if collected.is_empty() {
        return None;
    }
    if let Some(refused) = channel_arn_refusal(collected.iter().map(|(violation, _)| violation)) {
        return Some(PreHandlerRefusal {
            refused,
            detail: INVALID_CHANNEL_ARN_DETAIL,
        });
    }
    let stated: Vec<Violation> = collected
        .iter()
        .map(|(violation, echo)| stated_clause(violation.clone(), echo.clone()))
        .collect();
    Some(PreHandlerRefusal {
        refused: Refused {
            code: VALIDATION_EXCEPTION.to_string(),
            message: aws_smithy_schema_server::constraints::envelope(&stated)?,
        },
        detail: CONSTRAINT_DETAIL,
    })
}

/// One violation as this service states it: a `@pattern` requirement quoting the
/// expression unanchored, and a COLLECTION member's `@length` clause quoting the
/// collection the request carried (`validation_message::quoting_the_submitted_collection`,
/// the tree's one home for that restoring rule). Every other clause is the framework's,
/// untouched.
fn stated_clause(violation: Violation, echo: SubmittedEcho) -> Violation {
    let quoted = quoting_the_submitted_collection(violation, echo);
    match &quoted.kind {
        ::smithy_server_runtime::ViolationKind::Pattern(pattern) => Violation {
            kind: ::smithy_server_runtime::ViolationKind::Pattern(
                ::service_common::validation::unanchored(pattern),
            ),
            ..quoted
        },
        _ => quoted,
    }
}
