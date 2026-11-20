// SPDX-License-Identifier: CC0-1.0

//! The violations of ONE request that the framework's validating walk does not report, read
//! off the request body.
//!
//! That walk finds every bound the model declares — a `@length`, a `@range`, an enumerated
//! member's value set, an absent `@required` member — and hands them to
//! [`super::EmulatedService::override_refusal`] as violations, which that seam words as one
//! clause per violated member inside the `<N> validation error(s) detected: ` envelope. A
//! request can break a constraint that walk never reports in two ways, and a refusal that
//! reported only what the walk found would state one clause where AWS states several, with
//! a count a client reads violations off that is short:
//!
//! 1. the constraint is one the model does NOT declare, so no walk could find it — the
//!    first half below, which reads the request and words the clause AWS states for it;
//! 2. the constraint IS declared, but the walk never ran: the framework deserializes the
//!    body before it walks the built input, and a member the input requires and the body
//!    omits stops the first step — the second half below, which reads those bounds off the
//!    generated input SCHEMA and states the clauses that walk would have.
//!
//! Both halves read the request BODY, because that is all either is about — no stored state
//! decides any of them — and this is the one layer that has the body in hand before the
//! operation runs ([`super::OperationScopedClauseForm`]).
//!
//! A clause of the FIRST half keeps the wording AWS states for it rather than the form the
//! served operation states the WALK's clauses in (`super`'s "Which form a constraint
//! clause takes"): the walk is not what found it, so there is no declared bound to render
//! from — exactly as an enumerated member's value set and an absent member keep the
//! reader's own rendering whichever operation was served. A violation of the SECOND half is
//! the walk's own, so it is handed to that renderer and takes the served operation's form
//! like any other.

use serde_json::Value;

use aws_smithy_runtime_api::http::Request as RtRequest;
use aws_smithy_schema_server::constraints::{EnumTrait, LengthTrait, Violation, ViolationKind};
use smithy_server_runtime::{DeserializableStruct, ServerHttpProtocol};

/// The operation whose elements must each state one of a set of members the model leaves
/// all four optional, and the member list they arrive in.
const TRANSACT_WRITE_ITEMS: &str = "TransactWriteItems";

/// The list member that carries those elements, as the request spells it on the wire.
const TRANSACT_ITEMS_MEMBER: &str = "TransactItems";

/// The four members one element may state, as the model spells them
/// (`smithy_query(query="shape", shape="com.amazonaws.dynamodb#TransactWriteItem")`).
const TRANSACT_ITEM_ACTIONS: [&str; 4] = ["ConditionCheck", "Put", "Delete", "Update"];

/// The member a table NAME arrives in, as the model spells it on the wire.
const TABLE_NAME_MEMBER: &str = "TableName";

/// The clauses the `TableName` of a request that NAMES the table it creates earns, read off
/// `req`'s body.
///
/// Empty for an operation that does not hold the member to the table-name form
/// (`super::TABLE_NAME_FORM_OPERATIONS`), for a request that carries no name, and for a
/// name that takes the form. The clauses are read HERE, off the body, because they answer
/// for a refusal the validating WALK may have raised about ANY member: that walk reports
/// the bound the model declares on a member that admits a table's ARN as well as its name,
/// which is wider than the form a NAME takes, and the clauses a caller reads are this
/// form's — so the whole refusal is stated from the name, and the handler's own check words
/// the identical clauses for the request that reaches it
/// (`crate::state::table_name_form_clauses`).
pub fn table_name_form_clauses(operation: &str, req: &RtRequest) -> Vec<String> {
    if !super::holds_table_name_to_its_form(operation) {
        return Vec::new();
    }
    let Some(body) = req.body().bytes() else {
        return Vec::new();
    };
    let Ok(document) = ::serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let Some(name) = document.get(TABLE_NAME_MEMBER).and_then(Value::as_str) else {
        // A member the request does not carry is the absent member this service's own
        // parameter check states, which is a different refusal.
        return Vec::new();
    };
    crate::state::table_name_form_clauses(crate::state::TABLE_NAME_MEMBER_PATH, name)
}

/// The clauses this service's own reading of `req` found for the operation it is being
/// served as, in the order the input declares the members they are about — so they join
/// the walk's clauses in the one order the refusal states its members in.
///
/// Empty for every request that violates none of them, which is every request of every
/// operation that has no such constraint.
pub fn service_checked_clauses(operation: &str, req: &RtRequest) -> Vec<String> {
    if operation != TRANSACT_WRITE_ITEMS {
        return Vec::new();
    }
    let Some(body) = req.body().bytes() else {
        return Vec::new();
    };
    let Ok(document) = ::serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    transact_items_clauses(&document)
}

/// The clause a `TransactWriteItems` body earns when one of its elements states none of
/// the four actions.
///
/// ONE clause however many elements state none: the clause names the LIST member, which
/// is violated once — the path AWS reports it under names no element position.
///
/// Only the element that states NONE of them is read here. An element that states SEVERAL
/// states no single action either, and that one is refused by the handler, which has the
/// decoded actions in hand and words it as that defect
/// (`crate::state::exactly_one_operation`, reached from `Requested::of`): it is not read
/// off the body, because a body carrying it reaches the handler — nothing the validating
/// walk finds can mask it.
fn transact_items_clauses(document: &Value) -> Vec<String> {
    let Some(elements) = document.get(TRANSACT_ITEMS_MEMBER).and_then(Value::as_array) else {
        // No list at all is the absent `@required` member the walk reports, and a member
        // that is not a list does not decode into the operation's input at all.
        return Vec::new();
    };
    let states_no_action = elements.iter().any(|element| {
        element.as_object().is_some_and(|members| {
            TRANSACT_ITEM_ACTIONS
                .iter()
                .all(|action| members.get(*action).is_none_or(Value::is_null))
        })
    });
    if states_no_action {
        return vec![crate::state::transact_item_states_no_action_clause()];
    }
    Vec::new()
}

// =============================================================================
// A CAPPED LIST PAST ITS MAXIMUM
//
// A list member carrying more elements than the model admits is a violation the validating
// WALK reports — so the request is refused before the handler that has the decoded list in
// hand runs — and AWS's recorded refusals of it state the REJECTED LIST:
// `Value '[TransactGetItem(get=Get(key={pk=AttributeValue(s=…, n=null, …)}, …))]' at
// 'transactItems' failed to satisfy constraint: …`, the same object rendering a refused
// `BatchWriteItem` request map is quoted in, and
// `Value '[ParameterizedStatement(statement=…, …)]' at 'transactStatements' …` for the
// PartiQL transaction's own list. That walk hands
// [`super::EmulatedService::override_refusal`] its own summary of the collection (`<n>
// items`) rather than the collection, so there is nothing there to render from.
//
// The list is therefore read HERE, where the request still is: decoded by the framework's
// OWN deserializer for the served operation's input — the same protocol, the same input
// schema and the same generated reader the dispatch that follows uses, so this layer
// cannot disagree with it — and rendered by the one renderer this crate states that form
// with (`crate::state::rendered_transact_get_items`,
// `crate::state::rendered_transact_write_items` and
// `crate::state::rendered_parameterized_statements`). The BOUND is read off the same
// generated input schema the walk reads it off, never off a second transcription of the
// model.
//
// Only a list past the MAXIMUM is stated this way, which is the class the recorded
// refusals carry. A list below the member's minimum is the empty list, whose recorded
// refusal states the member and the bound alone, so it keeps the walk's own clause
// (`tests/all/collection_cap_constraint_reports.rs` pins the two side by side).
// =============================================================================

/// The other operation whose `TransactItems` list is quoted back that way.
const TRANSACT_GET_ITEMS: &str = "TransactGetItems";

/// The PartiQL transaction, whose own statement list is quoted back the same way.
const EXECUTE_TRANSACTION: &str = "ExecuteTransaction";

/// The list member of `ExecuteTransaction`, as the model spells it on the wire.
const TRANSACT_STATEMENTS_MEMBER: &str = "TransactStatements";

/// The clause one capped list earns, beside the member path the walk reports that list
/// under — so the refusal substitutes it for the walk's own clause about THAT member and
/// leaves every other clause of the same refusal alone.
#[derive(Debug, Clone)]
pub struct CappedListClause {
    /// The member path the validating walk reports the violated list under.
    pub path: &'static str,
    /// The clause this service words for it, with the rejected list rendered from the
    /// request.
    pub clause: String,
}

/// The clause the served operation's capped list earns for carrying more elements than the
/// model admits, with the rejected list rendered from the request itself.
///
/// `None` for an operation that has no such list, for a body no input can be read from, and
/// for a list inside its declared bound — including an empty one, which keeps the walk's
/// clause.
pub fn capped_list_clause(
    operation: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<CappedListClause> {
    let (path, rendered, requirement) = match operation {
        TRANSACT_GET_ITEMS => {
            let schema = crate::ops::types::TRANSACT_GET_ITEMS_INPUT_SCHEMA;
            let input: crate::ops::types::TransactGetItemsInput = decoded(protocol, req, schema)?;
            let requirement =
                past_declared_maximum(schema, TRANSACT_ITEMS_MEMBER, input.transact_items.len())?;
            (
                crate::state::TRANSACT_ITEMS_MEMBER_PATH,
                crate::state::rendered_transact_get_items(&input.transact_items),
                requirement,
            )
        }
        TRANSACT_WRITE_ITEMS => {
            let schema = crate::ops::types::TRANSACT_WRITE_ITEMS_INPUT_SCHEMA;
            let input: crate::ops::types::TransactWriteItemsInput = decoded(protocol, req, schema)?;
            let requirement =
                past_declared_maximum(schema, TRANSACT_ITEMS_MEMBER, input.transact_items.len())?;
            (
                crate::state::TRANSACT_ITEMS_MEMBER_PATH,
                crate::state::rendered_transact_write_items(&input.transact_items),
                requirement,
            )
        }
        EXECUTE_TRANSACTION => {
            let schema = crate::ops::types::EXECUTE_TRANSACTION_INPUT_SCHEMA;
            let input: crate::ops::types::ExecuteTransactionInput = decoded(protocol, req, schema)?;
            let requirement = past_declared_maximum(
                schema,
                TRANSACT_STATEMENTS_MEMBER,
                input.transact_statements.len(),
            )?;
            (
                crate::state::TRANSACT_STATEMENTS_MEMBER_PATH,
                crate::state::rendered_parameterized_statements(&input.transact_statements),
                requirement,
            )
        }
        _ => return None,
    };
    Some(CappedListClause {
        path,
        clause: crate::state::constraint_clause(path, Some(&rendered), &requirement),
    })
}

/// `req`'s body read as the input `schema` declares, by the framework's own deserializer
/// for it — `None` for a body no input can be read from, which is a refusal of its own
/// (`super::request_layer`).
fn decoded<I: DeserializableStruct>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
    schema: &'static ::aws_smithy_schema::Schema,
) -> Option<I> {
    let mut deserializer = protocol.deserialize_input(req, schema).ok()?;
    I::deserialize(&mut *deserializer).ok()
}

/// The requirement phrase for the `@length` MAXIMUM `member` of `input` declares, when
/// `elements` is past it — read off the generated schema the validating walk reads it off.
/// `None` when the member declares no maximum, and when the list is inside it.
fn past_declared_maximum(
    input: &'static ::aws_smithy_schema::Schema,
    member: &str,
    elements: usize,
) -> Option<String> {
    let declared_member = input
        .members()
        .iter()
        .find(|declared| declared.member_name() == Some(member))?;
    let declared =
        declared_trait::<LengthTrait>(declared_member.traits()?, &LengthTrait::TRAIT_ID)?;
    let max = declared.max?;
    let length = i64::try_from(elements).unwrap_or(i64::MAX);
    (length > max).then(|| crate::state::max_length_requirement(usize::try_from(max).unwrap_or(0)))
}

// =============================================================================
// THE BOUNDS THE VALIDATING WALK NEVER GOT TO
//
// The framework reads a request in two steps: it DESERIALIZES the body into the
// operation's input, and then walks that input for every bound the model declares. A
// member the input REQUIRES and the body does not carry is found by the first step, which
// cannot complete — so the walk never runs, and every other bound the same body breaks is
// found by nobody. One request, several independent violations, one of them reported.
//
// AWS reports them together: a request breaking four of its input's constraints is refused
// with four clauses in one counted envelope, so a client fixing a malformed request sees
// every defect of it rather than one per round trip. That ORDER — deserialize, then walk —
// is the vendored framework's (`vendor/smithy-server-runtime`'s operation sequence, which
// this tree does not author), so the bounds that walk did not get to are read HERE off the
// body instead.
//
// They are read off the SAME generated input schema the walk reads them off
// (`crate::ops::types`'s `*_INPUT_SCHEMA`, where codegen hangs each member's `@length`,
// `@pattern` and value set), never off a second transcription of the model: a bound the
// model changes moves in both places at once, and the clause a defect earns here is the
// clause that walk would have stated — the member path, the rejected value and the
// requirement — so it is WORDED by the one renderer that words the walk's clauses
// (`super::violation_clause`) and counted in the one envelope beside them
// (`super::validation_envelope`).
//
// What is read is the TOP-LEVEL STRING members of the served input: those are the members
// a body carrying an absent required member can still break a declared bound of. The
// violations are produced for every such request and used only by a refusal that reports
// an absent required member, which is the one refusal whose walk never ran.
// =============================================================================

/// The input schema of the operation whose body is read for those bounds, or `None` for an
/// operation this reads nothing of.
///
/// The operations named are the ones whose input declares a required member BESIDES the
/// table name: a request that omits the table name is refused by this service's own
/// parameter check, which states its one sentence and nothing else
/// (`super::SERVICE_REQUIRED_MEMBERS`), so the walk has nothing to be masked by there. An
/// operation later found to mask a violation this way needs its own arm — the schema is all
/// an arm carries, so no bound, no value set and no member name is written out here.
/// `tests/all/request_shape_validation_classes.rs` reads the aggregate back over the
/// operations that carry one.
fn body_read_input(operation: &str) -> Option<&'static ::aws_smithy_schema::Schema> {
    Some(match operation {
        "DeleteItem" => crate::ops::types::DELETE_ITEM_INPUT_SCHEMA,
        "GetItem" => crate::ops::types::GET_ITEM_INPUT_SCHEMA,
        "PutItem" => crate::ops::types::PUT_ITEM_INPUT_SCHEMA,
        "UpdateItem" => crate::ops::types::UPDATE_ITEM_INPUT_SCHEMA,
        _ => return None,
    })
}

/// The violations of `operation`'s declared bounds this service can still read off `req`'s
/// body, kept for the one refusal that needs them.
///
/// `None` for every operation whose body this reads nothing of. The body is COPIED and not
/// read here: the refusal these answer for is the one whose deserialization aborted, so
/// reading the body eagerly would make every well-formed request of these operations pay a
/// second full parse of an item it already parsed once.
pub fn readable_body(operation: &str, req: &RtRequest) -> Option<ReadableBody> {
    let input = body_read_input(operation)?;
    Some(ReadableBody {
        input,
        body: req.body().bytes()?.to_vec(),
    })
}

/// One request's body beside the schema its bounds are declared in.
#[derive(Debug)]
pub struct ReadableBody {
    /// The generated schema of the operation's input, where codegen hung each member's
    /// declared bounds.
    input: &'static ::aws_smithy_schema::Schema,
    /// The body as it arrived.
    body: Vec<u8>,
}

impl ReadableBody {
    /// The violations of the input's declared bounds this body breaks.
    ///
    /// Empty for every request that breaks none of them. The order several of them are
    /// stated in is the refusal's ([`super::in_stated_member_order`]), like the walk's own.
    pub fn violations(&self) -> Vec<Violation> {
        let Ok(document) = ::serde_json::from_slice::<Value>(&self.body) else {
            return Vec::new();
        };
        self.input
            .members()
            .iter()
            .flat_map(|member| broken_bounds(member, &document))
            .collect()
    }
}

/// Every bound `member` declares that the value the body carries for it breaks.
///
/// The member is read by the name the model spells it under, which is the name this
/// protocol puts on the wire (`awsJson` reads a struct member by its member name), and only
/// a STRING value is read: every bound below is a string member's, and a member carrying
/// another datatype does not decode into the input at all, which is a different refusal.
fn broken_bounds(member: &::aws_smithy_schema::Schema, document: &Value) -> Vec<Violation> {
    let Some(traits) = member.traits() else {
        return Vec::new();
    };
    let Some(value) = member
        .member_name()
        .and_then(|name| document.get(name))
        .and_then(Value::as_str)
    else {
        return Vec::new();
    };
    // In the order the walk runs a leaf's traits, so a member breaking two of its bounds
    // earns the same clauses in the same order whichever reader found them. A `@pattern` is
    // not among them: no top-level member of the inputs above declares one, and compiling a
    // declared expression is the validating walk's own business — it holds the one compiled
    // pattern cache in the tree.
    let kinds = broken_length(traits, value)
        .into_iter()
        .chain(broken_value_set(traits, value));
    kinds
        .map(|kind| Violation {
            // The path the validating walk reports a top-level member under.
            path: ::aws_smithy_schema_server::constraints::path::member(
                member.member_name().unwrap_or_default(),
            ),
            kind,
            // Held to the same ceiling the walk holds its echoed values to, so a caller
            // cannot amplify a request into an arbitrarily long response
            // ([`crate::state::MAX_ECHOED_CLAUSE_VALUE`]).
            value: Some(crate::state::echoed_clause_value(value).into_owned()),
            sensitive: false,
        })
        .collect()
}

/// The `@length` bound `value` breaks, counted in CHARACTERS — the length the walk counts
/// for a string.
fn broken_length(traits: &::aws_smithy_schema::TraitMap, value: &str) -> Option<ViolationKind> {
    let declared = declared_trait::<LengthTrait>(traits, &LengthTrait::TRAIT_ID)?;
    let length = i64::try_from(value.chars().count()).unwrap_or(i64::MAX);
    match (declared.min, declared.max) {
        (Some(min), _) if length < min => Some(ViolationKind::MinLength(min)),
        (_, Some(max)) if length > max => Some(ViolationKind::MaxLength(max)),
        _ => None,
    }
}

/// The value set `value` falls outside, in the order the model declares it: the set a
/// clause STATES is then the renderer's business (`super::stated_enum_order`), exactly as
/// for the walk's own clause.
fn broken_value_set(traits: &::aws_smithy_schema::TraitMap, value: &str) -> Option<ViolationKind> {
    let declared = declared_trait::<EnumTrait>(traits, &EnumTrait::TRAIT_ID)?;
    let admitted = declared.values.iter().any(|admissible| {
        if declared.ignore_case {
            admissible.eq_ignore_ascii_case(value)
        } else {
            *admissible == value
        }
    });
    (!admitted).then(|| {
        ViolationKind::EnumValue(declared.values.iter().map(|v| (*v).to_string()).collect())
    })
}

/// One constraint trait of a member schema, as codegen hung it there.
fn declared_trait<'t, T: 'static>(
    traits: &'t ::aws_smithy_schema::TraitMap,
    id: &::aws_smithy_schema::ShapeId,
) -> Option<&'t T> {
    traits.get(id)?.as_any().downcast_ref::<T>()
}


