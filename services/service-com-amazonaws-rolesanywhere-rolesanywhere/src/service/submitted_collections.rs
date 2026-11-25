// SPDX-License-Identifier: CC0-1.0

//! The one half of a constraint refusal of this service that [`super::validation`] cannot
//! state: the LIST a clause about a list member is about.
//!
//! # The rule
//!
//! A value outside a constraint the model declares is refused before any handler runs,
//! over one clause per violation, each naming the value that was rejected, the member path
//! it sat at and the requirement it failed. The value is the only place the request's own
//! input is reported back, so a client comparing it against what it sent has to see its
//! own input.
//!
//! A `@length` on a list is checked ON THE MEMBER, before the walk descends into the
//! elements, so the only rendering the framework has for the rejected value is a SUMMARY
//! of its size — `<n> items` (`vendor/smithy-schema-server/src/constraints.rs`) — which is
//! not a value any caller sent. AWS states the list itself there, as its bracketed
//! elements: `Value '[<element>, <element>, …]' at '<member>' failed to satisfy
//! constraint: Member must have length less than or equal to <max>`.
//!
//! # Why a per-operation layer
//!
//! [`Service::override_refusal`](smithy_server_runtime::Service::override_refusal) is
//! handed the violations alone and never the request they were found in, so that value
//! cannot be recovered there. [`QuotingSubmittedLists`] is where it is: a per-operation
//! wrapper, which has the request and can re-read it.
//!
//! Nothing is re-implemented and no check is repeated in other words: the violations and
//! the submitted lists come from the tree's shared collecting walk
//! (`service_common::validation::refusing_violations_with_echoes_over`, which runs the
//! protocol's own deserializer and the framework's own `constraints::validate` and owns
//! every bound on what one clause may quote), the restoring rule is
//! `service_common::validation_message::quoting_the_submitted_collection`, the whole
//! message is the one [`super::validation::refused`] words for every other refusal of this
//! service, and the status and the wire envelope come from the framework's own refusal
//! channel. A refusal with no such clause is left to that channel untouched, so a request
//! this layer changes nothing about is answered exactly as before.
//!
//! The RULE and every rendering are therefore shared; what is local is the few lines of
//! wrapper that reach them, as they are in the sibling services that have their own clause
//! renderings to apply (`service-com-amazonaws-cloudhsmv2-baldrapiservice`,
//! `service-com-amazonaws-directoryservicedata-directoryservicedata`,
//! `service-com-amazonaws-route53domains-route53domains-v20140515` and
//! `service-com-amazonaws-cognitoidentity-awscognitoidentityservice` each carry one). The
//! tree's turnkey wrapper (`service_common::submitted_collections::StatedClauseTable` over
//! `QuotingSubmittedCollections`) states each clause with the FRAMEWORK's own wording,
//! which would drop the three restatements [`super::validation`] owns — the published
//! `@pattern` expression, the wider expression a resource-id member publishes and the order
//! an enumerated type's values are listed in — from any refusal that also carries one of
//! them, and it walks the request under the shared walk's own position ceiling rather than
//! one computed from this model's declared list maxima ([`MAX_ECHOED_POSITIONS`]). Both are
//! why this service words the whole refusal through its own seam instead.
//!
//! # Which clauses quote their list, and which state the requirement alone
//!
//! A clause quotes the list the request carried for the list members whose elements are
//! SCALARS the model does not mark `@sensitive` — `roleArns` (`RoleArnList` of `RoleArn`)
//! and `managedPolicyArns` (`ManagedPolicyList` of `String`), which is every bounded list
//! of scalars this model declares
//! (`smithy_query(query="shape", shape="RoleArnList,ManagedPolicyList,TagList,TagKeyList,NotificationSettings,NotificationSettingKeys")`).
//! Two classes keep the framework's own clause, which this service renders as the
//! requirement and no value at all:
//!
//! * a list whose elements the model marks `@sensitive` (`TagKeyList` of `TagKey`) and a
//!   list of structures that HOLD such values (`TagList` of `Tag`, whose `key` and `value`
//!   are `@sensitive`): a value the model marks sensitive is withheld from every clause of
//!   this service, and a clause quoting the collection would be the one place it could
//!   still be disclosed;
//! * a list of structures (`NotificationSettings`, `NotificationSettingKeys`,
//!   `MappingRules`), whose elements have no scalar rendering of their own.
//!
//! Both are the fail-safe direction: the clause still names the member, the requirement
//! and the bound that was broken.

use ::std::sync::Arc;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::service_common::validation::refusing_violations_with_echoes_over;
use ::service_common::validation_message::quoting_the_submitted_collection;
use ::smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, OperationRoutingTable, Refusal,
    Service, Violation,
};

/// The list members a clause of this service quotes the submitted list of: the bounded
/// lists of scalars the model declares, which are the ones the collecting walk has a
/// bracketed rendering for and whose elements carry nothing the model marks `@sensitive`
/// (the module header above names every other list of this model and why it keeps the
/// framework's clause). Both are declared by `CreateProfile` and by `UpdateProfile`
/// (`smithy_query(query="shape", shape="CreateProfileRequest,UpdateProfileRequest")`).
const QUOTED_LIST_MEMBERS: &[&str] = &["roleArns", "managedPolicyArns"];

/// The largest `@length` maximum a list a clause quotes declares: `RoleArnList` is
/// `@length(0, 250)` and `ManagedPolicyList` `@length(0, 50)`
/// (`smithy_query(query="shape", shape="RoleArnList,ManagedPolicyList")`).
const LARGEST_QUOTED_LIST_MAXIMUM: usize = 250;

/// Every other position one request of this service can fill, past the elements of the
/// list a clause quotes: the members the widest input declares (`CreateProfileRequest`,
/// nine) plus one element past the maximum of the second bounded list it carries
/// (`ManagedPolicyList`, `@length(0, 50)`).
const OTHER_POSITIONS: usize = 9 + 51;

/// The member positions the collecting walk may visit while recording how a clause can
/// state its value.
///
/// A LIST renders only when every one of its elements was walked, so a list ONE element
/// past its declared maximum — which is the smallest list that breaks the bound at all —
/// has to be reachable within this count, past every other position the same request can
/// fill. Computed from the model's own declared maxima, so the ceiling is a model constant
/// and no response is sized by the request; the per-value and per-message byte bounds stay
/// the shared walk's, and a list past them keeps the framework's summary.
const MAX_ECHOED_POSITIONS: usize = LARGEST_QUOTED_LIST_MAXIMUM + 1 + OTHER_POSITIONS;

/// Whether a clause stated at `member_path` quotes the list the request submitted.
///
/// The member is read off the LAST segment of the path, the way every other restatement of
/// a clause of this service reads it ([`super::validation`]).
fn quotes_the_submitted_list(member_path: &str) -> bool {
    member_path
        .split('.')
        .next_back()
        .is_some_and(|member| QUOTED_LIST_MEMBERS.contains(&member))
}

/// One modelled operation, served so that a constraint clause about a list member states
/// the list the request carried.
///
/// It makes no routing decision and holds no state of its own: the operation it wraps is
/// the one the generated routing table names for the same key, built from the SAME shared
/// engine, and a request this wrapper does not refuse is handed to it untouched — so the
/// framework still owns the whole deserialize -> validate -> handle -> serialize sequence
/// and every status, shape and message it answers with.
///
/// The cost is DELIBERATE: the request's body is read twice, here and in the operation's
/// own sequence, because the framework hands no already-built input to an operation. Only
/// the operations whose model declares one of those list members pay it
/// ([`quoting_submitted_lists`]).
struct QuotingSubmittedLists<Op> {
    operation: Op,
}

impl<Op> QuotingSubmittedLists<Op> {
    /// Wrap one operation, already constructed from the shared engine.
    fn new(operation: Op) -> Self {
        Self { operation }
    }
}

impl<Op> ErasedOperation for QuotingSubmittedLists<Op>
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
            // The whole refusal is decided — and the borrow of the definition it is
            // rendered through taken and dropped — BEFORE the await below:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding one across that
            // await would make this future non-`Send`.
            if let Some(refused) = quoting_refusal::<Op>(svc, req) {
                return refused;
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

/// The refusal `req` earns from `Op`'s own constraints when at least one of its clauses
/// quotes a list the request submitted — or `None`, which is every other request,
/// including one these constraints refuse only by clauses this changes nothing about, so
/// the framework's own channel answers it exactly as before.
fn quoting_refusal<Op: Operation>(svc: &dyn Service, req: &RtRequest) -> Option<RtResponse> {
    let refusing = refusing_violations_with_echoes_over::<Op>(
        svc.definition().protocol(),
        req,
        MAX_ECHOED_POSITIONS,
    );
    let mut quoted_a_list = false;
    let mut violations: Vec<Violation> = Vec::with_capacity(refusing.len());
    for (violation, echo) in refusing {
        if !quotes_the_submitted_list(&violation.path) {
            violations.push(violation);
            continue;
        }
        let quoted = quoting_the_submitted_collection(violation.clone(), echo);
        quoted_a_list |= quoted.value != violation.value;
        violations.push(quoted);
    }
    if !quoted_a_list {
        return None;
    }
    // Every clause, the envelope over them and the error code are the ones this service
    // words every other refusal with, so the only difference from the refusal the
    // framework's own channel would have rendered is the list a clause about a list quotes.
    let stated = super::validation::refused(&Refusal::Invalid(violations.clone()))?;
    Some(svc.definition().refusal_response(
        ::aws_smithy_schema_server::ProtocolError::Invalid(violations),
        Some(stated),
        req,
    ))
}

/// The modelled operations whose input declares one of the list members a clause quotes,
/// each served through the layer above and built from the SAME shared engine, so a wrapped
/// operation reads and writes the one store.
///
/// Keyed by the routing key the protocol resolves — the operation's own modelled name — so
/// a wrapped operation cannot land under a key the protocol never resolves. Every other
/// operation is absent from this table and resolves through the generated one untouched.
pub(super) fn quoting_submitted_lists<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
) -> OperationRoutingTable {
    let mut table = OperationRoutingTable::new();
    table.add_operation(
        <crate::ops::create_profile::CreateProfile<Eng> as Operation>::NAME,
        Box::new(QuotingSubmittedLists::new(
            crate::ops::create_profile::CreateProfile::new(Arc::clone(engine)),
        )),
    );
    table.add_operation(
        <crate::ops::update_profile::UpdateProfile<Eng> as Operation>::NAME,
        Box::new(QuotingSubmittedLists::new(
            crate::ops::update_profile::UpdateProfile::new(Arc::clone(engine)),
        )),
    );
    table
}
