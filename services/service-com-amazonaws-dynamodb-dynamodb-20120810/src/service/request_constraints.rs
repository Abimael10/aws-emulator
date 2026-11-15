// SPDX-License-Identifier: CC0-1.0

//! The input constraints THIS SERVICE checks itself, read off the request body.
//!
//! The framework's validating walk finds every bound the model declares — a `@length`, a
//! `@range`, an enumerated member's value set, an absent `@required` member — and hands
//! them to [`super::EmulatedService::override_refusal`] as violations, which that seam
//! words as one clause per violated member inside the
//! `<N> validation error(s) detected: ` envelope. A constraint the model does NOT declare
//! is invisible to that walk, so a request violating one of those AND one the walk found
//! would be refused reporting only the second: one clause where AWS states two, and a
//! count a client reads violations off that is short by one.
//!
//! This module is the other half of that aggregate: the violations this service finds by
//! READING the request, each as the clause the envelope counts. The request body is where
//! they are found because that is all they are about — no stored state decides any of
//! them — and this is the one layer that has the body in hand before the operation runs
//! ([`super::OperationScopedClauseForm`]).
//!
//! A clause raised here keeps the wording AWS states for it rather than the form the
//! served operation states the WALK's clauses in (`super`'s "Which form a constraint
//! clause takes"): the walk is not what found it, so there is no declared bound to render
//! from — exactly as an enumerated member's value set and an absent member keep the
//! reader's own rendering whichever operation was served.

use serde_json::Value;

use aws_smithy_runtime_api::http::Request as RtRequest;

/// The operation whose elements must each state one of a set of members the model leaves
/// all four optional, and the member list they arrive in.
const TRANSACT_WRITE_ITEMS: &str = "TransactWriteItems";

/// The list member that carries those elements, as the request spells it on the wire.
const TRANSACT_ITEMS_MEMBER: &str = "TransactItems";

/// The four members one element may state, as the model spells them
/// (`smithy_query(query="shape", shape="com.amazonaws.dynamodb#TransactWriteItem")`).
const TRANSACT_ITEM_ACTIONS: [&str; 4] = ["ConditionCheck", "Put", "Delete", "Update"];

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
