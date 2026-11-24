// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the write half of a stack's entitlement lifecycle over the wire:
//! `UpdateEntitlement` and `DeleteEntitlement`.
//!
//! Every case drives the real awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * `UpdateEntitlement` writes every member the request supplies onto the stored
//!   entitlement, leaves the members it omits exactly as they were, never moves the
//!   `(StackName, Name)` identity it addresses the record by, and reports the stored
//!   record back — so a later `DescribeEntitlements` answers identically;
//! * `DeleteEntitlement` removes exactly the entitlement that identity names, so every
//!   later read or write of it is refused as an entitlement that can't be found (HTTP 400
//!   `EntitlementNotFoundException`), the name is free again, and no other stored
//!   entitlement is touched;
//! * both operations address an entitlement OF a stack, so a request naming a stack the
//!   account does not hold is refused with `ResourceNotFoundException` and writes nothing.
//!
//! The status a not-found refusal of this service leaves with is the one
//! `tests/all/not_found_refusal_status.rs` pins for every operation that reports it.

use serde_json::{json, Value};

use crate::entitlements::{
    attributes, emulator_with_stack, entitlement, entitlement_rows, entitlements,
    NOT_FOUND_SENTENCE, STACK, STACK_KIND,
};
use crate::not_found_refusal_status::REFUSAL_STATUS;
use crate::refusal_messages::{not_found_sentence, refusal};

// The rows below name the stack a request addresses an entitlement of; the resource word
// its refusal carries and the status this service refuses over are the ones the sibling
// suites state (`tests/all/entitlements.rs`, `tests/all/not_found_refusal_status.rs`).

/// `UpdateEntitlement` bodies spanning the class, minus the identity each case fills in:
/// the rows differ along WHICH of the three optional members the request carries (none,
/// one, two, all of them), the enum member's value (`AppVisibility` is one of
/// `ALL` | `ASSOCIATED`), the number of attributes the replacement list carries
/// (`@length` 1..) and the length of the description (`@length` 0..256) — every member
/// and constraint from `smithy_query(query="operation", operation="UpdateEntitlement")`.
fn update_rows() -> Vec<(&'static str, Value)> {
    vec![
        ("no optional member at all", json!({})),
        (
            "the description alone",
            json!({ "Description": "the engineering applications" }),
        ),
        (
            // `Description` is `@length` 0..256, so the empty string is a description an
            // entitlement can carry rather than the absence of one.
            "an emptied description",
            json!({ "Description": "" }),
        ),
        (
            "the longest description",
            json!({ "Description": "d".repeat(256) }),
        ),
        (
            "the visibility alone",
            json!({ "AppVisibility": "ASSOCIATED" }),
        ),
        (
            "the other visibility alone",
            json!({ "AppVisibility": "ALL" }),
        ),
        (
            "one replacement attribute",
            json!({ "Attributes": attributes(1) }),
        ),
        (
            "several replacement attributes",
            json!({ "Attributes": attributes(4) }),
        ),
        (
            "every member at once",
            json!({
                "Description": "everything replaced",
                "AppVisibility": "ASSOCIATED",
                "Attributes": attributes(2),
            }),
        ),
    ]
}

/// The members of a stored `Entitlement` an update can replace, as
/// `smithy_query(query="operation", operation="UpdateEntitlement")` lists them.
const UPDATABLE_MEMBERS: [&str; 3] = ["Description", "AppVisibility", "Attributes"];

/// A `CreateEntitlement` body for `name` in `stack`, from the shared row `row`.
fn create_body(row: usize, stack: &str, name: &str) -> Value {
    let mut body = entitlement_rows()[row].1.clone();
    body["Name"] = json!(name);
    body["StackName"] = json!(stack);
    body
}

#[tokio::test]
async fn update_replaces_the_supplied_members_and_leaves_the_others() {
    for (row_index, (row, _)) in entitlement_rows().into_iter().enumerate() {
        for (update, request) in update_rows() {
            let case = format!("{row} updated by {update:?}");
            let emulator = emulator_with_stack(&case, &[STACK]).await;
            let name = "updated-entitlement";
            let created = entitlement(
                &case,
                &emulator
                    .ok(
                        &case,
                        "CreateEntitlement",
                        create_body(row_index, STACK, name),
                    )
                    .await,
            );

            let mut body = request.clone();
            body["Name"] = json!(name);
            body["StackName"] = json!(STACK);
            let updated = entitlement(
                &case,
                &emulator.ok(&case, "UpdateEntitlement", body).await,
            );

            // The identity members address the record; an update never moves it.
            assert_eq!(
                (&updated["Name"], &updated["StackName"]),
                (&json!(name), &json!(STACK)),
                "case {case:?}: an update must not move the entitlement, got {updated}"
            );

            // A member the request carried replaced the stored field; one it omitted kept
            // the value the create stored.
            for member in UPDATABLE_MEMBERS {
                let expected = request
                    .get(member)
                    .cloned()
                    .unwrap_or_else(|| created.get(member).cloned().unwrap_or(Value::Null));
                let actual = updated.get(member).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected {member} {expected}, got {actual}"
                );
            }

            // The entitlement was created once and modified by this call, so its creation
            // instant is untouched and it was last modified no earlier than that.
            assert_eq!(
                updated["CreatedTime"], created["CreatedTime"],
                "case {case:?}: an update must not re-create the entitlement, got {updated}"
            );
            let created_at = created["CreatedTime"]
                .as_f64()
                .unwrap_or_else(|| panic!("case {case:?}: expected a CreatedTime, got {created}"));
            let modified_at = updated["LastModifiedTime"]
                .as_f64()
                .unwrap_or_else(|| panic!("case {case:?}: expected a LastModifiedTime, got {updated}"));
            assert!(
                modified_at >= created_at,
                "case {case:?}: expected a LastModifiedTime at or after {created_at}, \
                 got {modified_at}"
            );

            // The update answered from the stored record, so the read reports it exactly —
            // and reading it twice reports the same record.
            for attempt in 1..=2 {
                let case = format!("{case} read {attempt}");
                let read = emulator
                    .ok(
                        &case,
                        "DescribeEntitlements",
                        json!({ "StackName": STACK, "Name": name }),
                    )
                    .await;
                let listed = entitlements(&case, &read);
                assert_eq!(
                    listed,
                    vec![updated.clone()],
                    "case {case:?}: expected the updated entitlement, got {read}"
                );
            }
        }
    }
}

#[tokio::test]
async fn delete_removes_exactly_the_named_entitlement() {
    // Rows: the entitlement deleted out of a stack that holds several, spanning the shared
    // create rows — and the same names stored in ANOTHER stack, which the delete must
    // leave alone because the stack is part of the identity.
    let other_stack = "other-entitled-stack";
    let names = ["first", "second", "third"];

    for (target_index, target) in names.iter().enumerate() {
        let case = format!("deleting {target:?}");
        let emulator = emulator_with_stack(&case, &[STACK, other_stack]).await;
        for (index, name) in names.iter().enumerate() {
            for stack in [STACK, other_stack] {
                emulator
                    .ok(
                        &case,
                        "CreateEntitlement",
                        create_body(index % entitlement_rows().len(), stack, name),
                    )
                    .await;
            }
        }

        // What the stacks held before the delete, read back through the implemented
        // reader, so "untouched" is asserted against the records themselves.
        let before = |stack: &str| {
            let emulator = &emulator;
            let case = case.clone();
            let stack = stack.to_string();
            async move {
                let read = emulator
                    .ok(&case, "DescribeEntitlements", json!({ "StackName": stack }))
                    .await;
                entitlements(&case, &read)
            }
        };
        let stored_elsewhere = before(other_stack).await;
        let stored_here = before(STACK).await;

        let deleted = emulator
            .ok(
                &case,
                "DeleteEntitlement",
                json!({ "StackName": STACK, "Name": target }),
            )
            .await;
        assert!(
            deleted.get("Entitlement").is_none(),
            "case {case:?}: the modelled result carries no members, got {deleted}"
        );

        // Every read and write of the removed identity is refused as an entitlement that
        // can't be found — a 400 whose sentence names nothing of the request.
        for (operation, body) in [
            (
                "DescribeEntitlements",
                json!({ "StackName": STACK, "Name": target }),
            ),
            (
                "DeleteEntitlement",
                json!({ "StackName": STACK, "Name": target }),
            ),
            (
                "UpdateEntitlement",
                json!({ "StackName": STACK, "Name": target, "AppVisibility": "ALL" }),
            ),
        ] {
            let case = format!("{case}: {operation} after the delete");
            let message = refusal(
                &emulator,
                &case,
                operation,
                body,
                REFUSAL_STATUS,
                "EntitlementNotFoundException",
            )
            .await;
            assert_eq!(
                message, NOT_FOUND_SENTENCE,
                "case {case:?}: expected message {NOT_FOUND_SENTENCE:?}, got {message:?}"
            );
            assert!(
                !message.contains(target) && !message.contains(STACK),
                "case {case:?}: the refusal must name neither {target:?} nor {STACK:?}, \
                 got {message:?}"
            );
        }

        // The stack's other entitlements are exactly what they were, and the other stack's
        // record of the SAME name is untouched: the identity is the stack and the name.
        let left_here = before(STACK).await;
        let expected_here: Vec<Value> = stored_here
            .iter()
            .filter(|record| record["Name"] != json!(target))
            .cloned()
            .collect();
        assert_eq!(
            left_here, expected_here,
            "case {case:?}: expected the stack's other entitlements unchanged, \
             got {left_here:?}"
        );
        let left_elsewhere = before(other_stack).await;
        assert_eq!(
            left_elsewhere, stored_elsewhere,
            "case {case:?}: another stack's entitlements must be untouched, \
             got {left_elsewhere:?}"
        );

        // The name is free again, so a create under it is a create rather than a
        // duplicate — and it reports a fresh record.
        let recreated = entitlement(
            &case,
            &emulator
                .ok(
                    &case,
                    "CreateEntitlement",
                    create_body(target_index % entitlement_rows().len(), STACK, target),
                )
                .await,
        );
        assert_eq!(
            recreated["Name"],
            json!(target),
            "case {case:?}: expected the re-created entitlement, got {recreated}"
        );
    }
}

#[tokio::test]
async fn update_and_delete_address_an_entitlement_of_a_stack_that_exists() {
    // Rows: the stacks a request can name that this account holds none of — an absent
    // one, one whose name differs from the stored stack only in case, and a digits-only
    // name the pattern admits.
    for absent in ["absent-stack", "Entitled-Stack", "0123456789"] {
        let emulator = emulator_with_stack(absent, &[STACK]).await;
        let name = "entitled";
        let stored = entitlement(
            absent,
            &emulator
                .ok(absent, "CreateEntitlement", create_body(0, STACK, name))
                .await,
        );
        let expected = not_found_sentence(STACK_KIND, absent);

        for (operation, body) in [
            (
                "DeleteEntitlement",
                json!({ "StackName": absent, "Name": name }),
            ),
            (
                "UpdateEntitlement",
                json!({ "StackName": absent, "Name": name, "Description": "moved" }),
            ),
        ] {
            let case = format!("{operation} for {absent:?}");
            let message = refusal(
                &emulator,
                &case,
                operation,
                body,
                REFUSAL_STATUS,
                "ResourceNotFoundException",
            )
            .await;
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
        }

        // Nothing was written for the stack the account does not hold: the stored
        // entitlement of the stack it does hold is exactly as it was.
        let read = emulator
            .ok(
                absent,
                "DescribeEntitlements",
                json!({ "StackName": STACK, "Name": name }),
            )
            .await;
        let listed = entitlements(absent, &read);
        assert_eq!(
            listed,
            vec![stored],
            "case {absent:?}: a refused write must change nothing, got {read}"
        );
    }
}
