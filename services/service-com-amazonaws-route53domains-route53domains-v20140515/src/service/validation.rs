// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a request the boundary constraint layer refused, before any
//! handler ran.
//!
//! The framework walks the constraints the model declares and hands the service the
//! refusal STRUCTURALLY — one [`Violation`] per offending value, carrying the dotted
//! member path, what it failed and the value it held — leaving the words to the service,
//! because no model records them. The envelope ("`<N> validation error(s) detected: `"
//! over one clause per violation), the member paths and every requirement sentence are the
//! fleet's one home for them (`service_common::validation_message`), so a clause this
//! service states cannot drift from another service's for the same constraint. Only the
//! error CODE is this service's own: Route 53 Domains has one client-input error,
//! `InvalidInput`, and every operation in the model lists it, so that is the code a client
//! branches on.
//!
//! Five parts of that rendering are not the framework's to know, and each is answered
//! here by delegating to the shared renderer that owns it:
//!
//! * **A clause quotes the value the REQUEST carried, whole.** The framework's walk elides
//!   a long value at a ceiling of its own, behind a marker naming the byte count, and it
//!   has no rendering for a COLLECTION at all — a `@length` on a list is checked on the
//!   member before the walk descends into its elements, so what it stores is a summary of
//!   the size (`<n> items`). Neither is text AWS emits: a constraint clause quotes the
//!   value the request carried, however long it was, and a list as `[<element>, …]`.
//! * **A value the model WITHHOLDS is stated by no clause.** A member the model marks
//!   `@sensitive`, or one nested inside a shape it marks so, is one the service never
//!   discloses: its clause names the member and the requirement and states no value at
//!   all. WHICH members those are is read off this service's own model
//!   ([`WITHHELD_MEMBERS`]), so nothing here lists a member name.
//! * **A constraint broken by a LIST's ELEMENTS is stated at the LIST member** — no
//!   element position and no `member` step — with the element shape's WHOLE declared
//!   constraint set nested inside `Member must satisfy constraint: [ … ]` and the list
//!   itself as the value. The set is read off the MODEL ([`INPUT_SCHEMAS`]) because the
//!   refusal carries only the constraints that failed.
//! * **An enumerated type's value set is listed in the order this front door walks its own
//!   table of those values**, keyed on each value's text
//!   (`service_common::validation_message::in_hashed_enum_order`, the fleet's one home for
//!   that order) — neither the order the model declares them in nor lexicographic.
//! * **A `@pattern` requirement quotes the expression WITHOUT the whole-value anchors** a
//!   model writes around one (`service_common::validation::unanchored`, the tree's one
//!   statement of that rendering). The check always runs the model's pattern as written.
//!
//! Nothing here re-checks anything and no constraint is re-implemented: every violation
//! comes from the framework's own machinery (the protocol's deserializer for an absent
//! `@required` member, `constraints::validate` over the built input) through the tree's
//! shared collecting walk, which also owns every bound on what one clause may quote. So a
//! request the framework would accept is never refused by this, and one it would refuse is
//! never accepted.

use std::collections::HashMap;
use std::sync::LazyLock;

use aws_smithy_runtime_api::http::Request as RtRequest;
use aws_smithy_schema::Schema;
use service_common::validation::{refusing_violations_with_mapped_echoes, unanchored};
use service_common::validation_message::{
    aws_validation_refusal_over_elements_stating, element_constraint_set, element_set_requirement,
    hashed_enum_order, in_hashed_enum_order, quoting_the_submitted_collection,
    quoting_the_whole_submitted_value, requirement,
};
use smithy_server_runtime::{
    Operation, Refusal, Refused, ServerHttpProtocol, Violation, ViolationKind,
};

/// The one client-input error this model declares, which every operation lists: a request
/// refused against the model's own rules comes back under that code rather than the
/// framework's generic `ValidationException`, because that is the code a Route 53 Domains
/// client branches on (`smithy_query(query="service_info")` lists it as this service's
/// common error).
const INVALID_INPUT: &str = "InvalidInput";

/// This service's Smithy model: the one source for which of its members are `@sensitive`,
/// and so for which values a refusal states none.
const MODEL_JSON: &str =
    include_str!("../../smithy-model/com-amazonaws-route53domains-route53domains-v20140515.json");

/// The members of this model whose value a refusal WITHHOLDS, read off [`MODEL_JSON`]
/// rather than listed here.
///
/// This model marks the contact a caller submits — `ContactDetail` is `@sensitive`, so
/// every value inside one is withheld at whatever depth a clause names it — and the
/// personal members of one (a name, an address, a phone number, an email) in their own
/// right.
static WITHHELD_MEMBERS: LazyLock<service_common::sensitive_members::WithheldMembers> =
    LazyLock::new(|| service_common::sensitive_members::WithheldMembers::from_model(MODEL_JSON));

/// Every operation INPUT shape of this service, as the generated schema statics: the
/// shapes a collection member's element constraints are read off.
///
/// All of them, not the ones one refusal happened to cite: an element constraint set is a
/// property of an input shape, so leaving an operation out would word its collection
/// clauses with less than the model declares. Regenerate with
/// `grep -h 'const INPUT_SCHEMA' src/ops/*.rs`, which names exactly these statics; an
/// input shape missing from this list is not a wrong clause, only a clause that states the
/// constraint that failed rather than the element shape's whole set.
const INPUT_SCHEMAS: &[&Schema] = {
    use crate::ops::types as t;
    &[
        t::ACCEPT_DOMAIN_TRANSFER_FROM_ANOTHER_AWS_ACCOUNT_INPUT_SCHEMA,
        t::ASSOCIATE_DELEGATION_SIGNER_TO_DOMAIN_INPUT_SCHEMA,
        t::CANCEL_DOMAIN_TRANSFER_TO_ANOTHER_AWS_ACCOUNT_INPUT_SCHEMA,
        t::CHECK_DOMAIN_AVAILABILITY_INPUT_SCHEMA,
        t::CHECK_DOMAIN_TRANSFERABILITY_INPUT_SCHEMA,
        t::DELETE_DOMAIN_INPUT_SCHEMA,
        t::DELETE_TAGS_FOR_DOMAIN_INPUT_SCHEMA,
        t::DISABLE_DOMAIN_AUTO_RENEW_INPUT_SCHEMA,
        t::DISABLE_DOMAIN_TRANSFER_LOCK_INPUT_SCHEMA,
        t::DISASSOCIATE_DELEGATION_SIGNER_FROM_DOMAIN_INPUT_SCHEMA,
        t::ENABLE_DOMAIN_AUTO_RENEW_INPUT_SCHEMA,
        t::ENABLE_DOMAIN_TRANSFER_LOCK_INPUT_SCHEMA,
        t::GET_CONTACT_REACHABILITY_STATUS_INPUT_SCHEMA,
        t::GET_DOMAIN_DETAIL_INPUT_SCHEMA,
        t::GET_DOMAIN_PRICES_INPUT_SCHEMA,
        t::GET_DOMAIN_SUGGESTIONS_INPUT_SCHEMA,
        t::GET_OPERATION_DETAIL_INPUT_SCHEMA,
        t::LIST_DOMAINS_INPUT_SCHEMA,
        t::LIST_OPERATIONS_INPUT_SCHEMA,
        t::LIST_PRICES_INPUT_SCHEMA,
        t::LIST_TAGS_FOR_DOMAIN_INPUT_SCHEMA,
        t::PUSH_DOMAIN_INPUT_SCHEMA,
        t::REGISTER_DOMAIN_INPUT_SCHEMA,
        t::REJECT_DOMAIN_TRANSFER_FROM_ANOTHER_AWS_ACCOUNT_INPUT_SCHEMA,
        t::RENEW_DOMAIN_INPUT_SCHEMA,
        t::RESEND_CONTACT_REACHABILITY_EMAIL_INPUT_SCHEMA,
        t::RESEND_OPERATION_AUTHORIZATION_INPUT_SCHEMA,
        t::RETRIEVE_DOMAIN_AUTH_CODE_INPUT_SCHEMA,
        t::TRANSFER_DOMAIN_INPUT_SCHEMA,
        t::TRANSFER_DOMAIN_TO_ANOTHER_AWS_ACCOUNT_INPUT_SCHEMA,
        t::UPDATE_DOMAIN_CONTACT_INPUT_SCHEMA,
        t::UPDATE_DOMAIN_CONTACT_PRIVACY_INPUT_SCHEMA,
        t::UPDATE_DOMAIN_NAMESERVERS_INPUT_SCHEMA,
        t::UPDATE_TAGS_FOR_DOMAIN_INPUT_SCHEMA,
        t::VIEW_BILLING_INPUT_SCHEMA,
    ]
};

/// `violation` with no value at all where the model withholds the value of the member its
/// clause names, and returned unchanged for every other member.
///
/// The same clause the framework's own walk writes for a `@sensitive` LEAF — the member and
/// the requirement, and no value — applied to the members the walk cannot recognise: the
/// trait that decides it sits on the SHAPE a member targets, which the runtime schemas the
/// walk carries do not hold.
fn withholding_a_withheld_value(violation: &Violation) -> Violation {
    if !WITHHELD_MEMBERS.withholds_value(&violation.path) {
        return violation.clone();
    }
    Violation {
        path: violation.path.clone(),
        kind: violation.kind.clone(),
        value: None,
        sensitive: true,
    }
}

/// The requirement a clause stated at the collection member `collection` nests: the element
/// shape's whole declared constraint set, with an enumerated type's value set in the order
/// this front door lists one.
///
/// The set itself is the shared reading of this model's own schemas
/// ([`element_constraint_set`] over [`INPUT_SCHEMAS`]), constraint for constraint, so it
/// carries the constraints an element SATISFIED as well as the one it broke — which no
/// violation can. `None` where no input shape declares an element constraint set for that
/// member, which leaves the clause the framework's own.
fn element_set(collection: &str) -> Option<String> {
    // The member the model constrained is the path's last segment, declared by the shape
    // that nests it.
    let member = collection.split('.').next_back()?;
    let declared = element_constraint_set(INPUT_SCHEMAS, member)?;
    let requirements: Vec<String> = declared
        .into_iter()
        .map(|kind| match kind {
            ViolationKind::EnumValue(values) => requirement(ViolationKind::EnumValue(
                hashed_enum_order(&values)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            )),
            other => requirement(other),
        })
        .collect();
    Some(element_set_requirement(&requirements))
}

/// The refusal this service states for `violations`, or `None` when there is nothing to
/// refuse.
///
/// `lists` answers the value a clause stated at a list member quotes — the list the request
/// carried, keyed by the member path the walk reports that list at — and is EMPTY on the
/// path that has no request in hand, where such a clause states no value at all rather than
/// one rebuilt from the violations: the subject is the value the member held, which only
/// the request carries.
pub(super) fn stated(violations: &[Violation], lists: &HashMap<String, String>) -> Option<Refused> {
    let stated: Vec<Violation> = violations
        .iter()
        .map(withholding_a_withheld_value)
        .map(|violation| in_hashed_enum_order(&violation))
        .collect();
    let refused = aws_validation_refusal_over_elements_stating(
        &Refusal::Invalid(stated),
        // A `@pattern` requirement quotes the EXPRESSION a value has to satisfy, without
        // the pair of anchors a model writes around a whole-value match: `^`/`$` is how
        // Smithy says "the whole value", which "must satisfy regular expression pattern"
        // already says. The CHECK always runs the model's pattern as written.
        unanchored,
        |collection| lists.get(collection).cloned(),
        element_set,
    )?;
    Some(Refused {
        code: INVALID_INPUT.to_string(),
        message: refused.message,
    })
}

/// The refusal this service states for `req` under the operation `O`, and the violations it
/// is about — or `None` when the request breaks no modelled constraint, which hands it to
/// the operation untouched.
///
/// The collecting half is the tree's shared one
/// (`service_common::validation::refusing_violations_with_mapped_echoes`): ONE read of the
/// request yields the framework's own violations together with the echo each clause states
/// its value from, and every list of scalars the request carried, keyed by the member path
/// a clause names that list at. Nothing is re-checked here.
pub(super) fn stated_from_request<O: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<(Vec<Violation>, Refused)> {
    let (refusing, lists) = refusing_violations_with_mapped_echoes::<O>(protocol, req);
    let violations: Vec<Violation> = refusing
        .into_iter()
        .map(|(violation, echo)| {
            // A value the walk WITHHELD is restored nowhere: the value is the one thing
            // such a clause must not disclose, so neither restoring rule below is asked
            // about it and the request's bytes never leave the walk.
            if violation.sensitive {
                return violation;
            }
            // A collection's clause states the collection, a scalar's the bytes submitted;
            // each rule touches only its own clause, so the two compose rather than
            // overlap.
            let quoted = quoting_the_submitted_collection(violation, echo.clone());
            quoting_the_whole_submitted_value(quoted, &echo)
        })
        .collect();
    let refused = stated(&violations, &lists)?;
    Some((violations, refused))
}
