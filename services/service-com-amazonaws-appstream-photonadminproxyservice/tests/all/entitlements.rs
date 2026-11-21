// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a stack's application entitlements over the wire.
//!
//! Every case drives the real awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * `CreateEntitlement` persists the entitlement under the `(StackName, Name)` identity
//!   the request names and reports the stored record back, so a later
//!   `DescribeEntitlements` answers identically — including the attributes, in the order
//!   they were supplied;
//! * an entitlement belongs to a stack that exists, and a stack holds one entitlement per
//!   name — the same name is still free in another stack;
//! * `DescribeEntitlements` reports the named entitlement of a stack, every entitlement of
//!   a stack when the read names none, and pages them exactly once;
//! * an entitlement does not outlive the stack it is associated with.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::refusal_messages::{not_found_sentence, refusal};
use crate::stack_fleet_lifecycle::Emulator;

/// The stack the single-stack cases create their entitlements in.
const STACK: &str = "entitled-stack";

/// The resource word AWS's refusal sentences use for a stack, as the crate states it
/// (`crate::state::STACK_KIND`).
const STACK_KIND: &str = "stack";

/// AWS's `EntitlementAlreadyExistsException` and `EntitlementNotFoundException`
/// sentences: an entitlement refusal states what is true of the requested entitlement and
/// echoes nothing of the request — no name, no stack, no account.
const ALREADY_EXISTS_SENTENCE: &str = "The requested entitlement already exists.";
const NOT_FOUND_SENTENCE: &str = "The requested entitlement was not found.";

/// The `PrincipalTag` attribute names an entitlement can match on, as
/// `smithy_query(query="doc", shape="EntitlementAttribute", member="Name")` lists the
/// valid values: `roles`, `department`, `organization`, `groups`, `title`, `costCenter`
/// and `userType`.
const ATTRIBUTE_NAMES: [&str; 7] = [
    "roles",
    "department",
    "organization",
    "groups",
    "title",
    "costCenter",
    "userType",
];

/// One attribute per name in the catalog above, so a row can carry as many of them as it
/// wants to span the list's length.
fn attributes(count: usize) -> Value {
    Value::Array(
        ATTRIBUTE_NAMES
            .iter()
            .take(count)
            .enumerate()
            .map(|(index, name)| json!({ "Name": name, "Value": format!("value-{index}") }))
            .collect(),
    )
}

/// `CreateEntitlement` request bodies spanning the class, minus the `StackName` each case
/// fills in: the rows differ along the enum member's value (`AppVisibility` is one of
/// `ALL` | `ASSOCIATED`), the number of attributes the list carries (`@length` 1..), the
/// presence and the length of the optional description (`@length` 0..256) and the shape of
/// the name (`^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$`) — every member and constraint from
/// `smithy_query(query="operation", operation="CreateEntitlement")`.
fn entitlement_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "required members only",
            json!({
                "Name": "minimal",
                "AppVisibility": "ALL",
                "Attributes": attributes(1),
            }),
        ),
        (
            "selected apps with a description",
            json!({
                "Name": "selected",
                "Description": "the finance applications",
                "AppVisibility": "ASSOCIATED",
                "Attributes": attributes(2),
            }),
        ),
        (
            "every attribute of the catalog",
            json!({
                "Name": "9entitlement_v1.2-beta",
                "Description": "",
                "AppVisibility": "ASSOCIATED",
                "Attributes": attributes(ATTRIBUTE_NAMES.len()),
            }),
        ),
        (
            // The longest name a create admits (the pattern's first character plus 100)
            // and the longest description (`@length` 0..256).
            "the longest name and description",
            json!({
                "Name": format!("z{}", "9".repeat(100)),
                "Description": "d".repeat(256),
                "AppVisibility": "ALL",
                "Attributes": attributes(3),
            }),
        ),
    ]
}

/// An emulator holding one stack, ready for its entitlements.
async fn emulator_with_stack(case: &str, stacks: &[&str]) -> Emulator {
    let emulator = Emulator::new();
    for stack in stacks {
        emulator
            .ok(case, "CreateStack", json!({ "Name": stack }))
            .await;
    }
    emulator
}

/// The `Entitlement` of a create or describe body.
fn entitlement(case: &str, payload: &Value) -> Value {
    payload
        .get("Entitlement")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an Entitlement, got {payload}"))
}

/// The `Entitlements` list of a describe body.
fn entitlements(case: &str, payload: &Value) -> Vec<Value> {
    payload
        .get("Entitlements")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected Entitlements, got {payload}"))
}

#[tokio::test]
async fn create_reports_the_request_and_describe_answers_identically() {
    for (case, request) in entitlement_rows() {
        let emulator = emulator_with_stack(case, &[STACK]).await;
        let mut body = request.clone();
        body["StackName"] = json!(STACK);

        let created = entitlement(case, &emulator.ok(case, "CreateEntitlement", body).await);

        // Every member the request supplied is reported back, under the identity the
        // request named.
        assert_eq!(
            created["Name"], request["Name"],
            "case {case:?}: expected the requested Name, got {created}"
        );
        assert_eq!(
            created["StackName"],
            json!(STACK),
            "case {case:?}: expected the requested StackName, got {created}"
        );
        assert_eq!(
            created["AppVisibility"], request["AppVisibility"],
            "case {case:?}: expected the requested AppVisibility, got {created}"
        );
        assert_eq!(
            created["Attributes"], request["Attributes"],
            "case {case:?}: expected the requested Attributes, in order, got {created}"
        );
        assert_eq!(
            created.get("Description").cloned().unwrap_or(Value::Null),
            request.get("Description").cloned().unwrap_or(Value::Null),
            "case {case:?}: expected the requested Description, got {created}"
        );

        // An entitlement nothing has modified yet was last modified when it was created.
        let created_time = created["CreatedTime"].clone();
        assert!(
            created_time.is_number(),
            "case {case:?}: expected a CreatedTime, got {created}"
        );
        assert_eq!(
            created["LastModifiedTime"], created_time,
            "case {case:?}: a fresh entitlement was last modified when it was created, \
             got {created}"
        );

        // The read answers from the stored record, so it reports the create byte for byte
        // — and reading it twice reports the same record, so no read moves anything.
        for attempt in 1..=2 {
            let case = format!("{case} read {attempt}");
            let read = emulator
                .ok(
                    &case,
                    "DescribeEntitlements",
                    json!({ "StackName": STACK, "Name": request["Name"] }),
                )
                .await;
            let listed = entitlements(&case, &read);
            assert_eq!(
                listed,
                vec![created.clone()],
                "case {case:?}: expected the stored entitlement, got {read}"
            );
        }
    }
}

#[tokio::test]
async fn one_stacks_entitlements_are_all_reported_when_the_read_names_none() {
    let case = "every row in one stack";
    let emulator = emulator_with_stack(case, &[STACK]).await;

    let mut created = Vec::new();
    for (row, request) in entitlement_rows() {
        let mut body = request.clone();
        body["StackName"] = json!(STACK);
        created.push(entitlement(
            row,
            &emulator.ok(row, "CreateEntitlement", body).await,
        ));
    }

    let read = emulator
        .ok(case, "DescribeEntitlements", json!({ "StackName": STACK }))
        .await;
    let listed = entitlements(case, &read);
    let expected: BTreeSet<String> = created
        .iter()
        .map(|entitlement| entitlement.to_string())
        .collect();
    let actual: BTreeSet<String> = listed
        .iter()
        .map(|entitlement| entitlement.to_string())
        .collect();
    assert_eq!(
        actual, expected,
        "case {case:?}: expected every stored entitlement, got {read}"
    );
    assert_eq!(
        read.get("NextToken"),
        None,
        "case {case:?}: a page that holds every record carries no NextToken, got {read}"
    );
}

#[tokio::test]
async fn a_name_one_stack_holds_is_still_free_in_another() {
    let case = "the same name in two stacks";
    let other_stack = "other-entitled-stack";
    let emulator = emulator_with_stack(case, &[STACK, other_stack]).await;

    for (row, request) in entitlement_rows() {
        for stack in [STACK, other_stack] {
            let case = format!("{row} in {stack}");
            let mut body = request.clone();
            body["StackName"] = json!(stack);
            let created = entitlement(
                &case,
                &emulator.ok(&case, "CreateEntitlement", body).await,
            );
            assert_eq!(
                created["StackName"],
                json!(stack),
                "case {case:?}: expected the entitlement of {stack}, got {created}"
            );
        }

        // Each stack reports its own record under that name, so the two identities are
        // distinct resources rather than one shared by both stacks.
        for stack in [STACK, other_stack] {
            let case = format!("{row} read from {stack}");
            let read = emulator
                .ok(
                    &case,
                    "DescribeEntitlements",
                    json!({ "StackName": stack, "Name": request["Name"] }),
                )
                .await;
            let listed = entitlements(&case, &read);
            assert_eq!(
                listed.len(),
                1,
                "case {case:?}: expected exactly the stack's own entitlement, got {read}"
            );
            assert_eq!(
                listed[0]["StackName"],
                json!(stack),
                "case {case:?}: expected the entitlement of {stack}, got {read}"
            );
        }
    }
}

#[tokio::test]
async fn a_second_create_of_one_identity_is_refused_as_already_existing() {
    for (row, request) in entitlement_rows() {
        let emulator = emulator_with_stack(row, &[STACK]).await;
        let mut body = request.clone();
        body["StackName"] = json!(STACK);
        emulator.ok(row, "CreateEntitlement", body.clone()).await;

        let name = request["Name"].as_str().expect("a name").to_string();
        let message = refusal(
            &emulator,
            row,
            "CreateEntitlement",
            body,
            400,
            "EntitlementAlreadyExistsException",
        )
        .await;
        assert_eq!(
            message, ALREADY_EXISTS_SENTENCE,
            "case {row:?}: expected message {ALREADY_EXISTS_SENTENCE:?}, got {message:?}"
        );
        assert!(
            !message.contains(name.as_str()),
            "case {row:?}: the refusal must not name the entitlement {name:?}, \
             got {message:?}"
        );
    }
}

#[tokio::test]
async fn an_entitlement_belongs_to_a_stack_that_exists() {
    // Rows: the stacks a request can name that this account holds none of — an absent
    // one, and one whose name differs from the stored stack only in case.
    for absent in ["absent-stack", "Entitled-Stack", "0123456789"] {
        let emulator = emulator_with_stack(absent, &[STACK]).await;
        let expected = not_found_sentence(STACK_KIND, absent);

        for (operation, body) in [
            (
                "CreateEntitlement",
                json!({
                    "Name": "anything",
                    "StackName": absent,
                    "AppVisibility": "ALL",
                    "Attributes": attributes(1),
                }),
            ),
            (
                "DescribeEntitlements",
                json!({ "StackName": absent }),
            ),
        ] {
            let case = format!("{operation} for {absent:?}");
            let message = refusal(
                &emulator,
                &case,
                operation,
                body,
                400,
                "ResourceNotFoundException",
            )
            .await;
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
        }

        // Nothing was created for the stack the account does not hold, so the stack it
        // does hold is still empty.
        let read = emulator
            .ok(absent, "DescribeEntitlements", json!({ "StackName": STACK }))
            .await;
        let listed = entitlements(absent, &read);
        assert!(
            listed.is_empty(),
            "case {absent:?}: a refused create must persist nothing, got {read}"
        );
    }
}

#[tokio::test]
async fn a_name_the_stack_holds_no_entitlement_under_is_refused_as_not_found() {
    let case = "an unknown entitlement name";
    let emulator = emulator_with_stack(case, &[STACK]).await;
    let mut body = entitlement_rows()[0].1.clone();
    body["StackName"] = json!(STACK);
    emulator.ok(case, "CreateEntitlement", body).await;

    // Rows: an absent name, the stored name in another case, and a name stored in a
    // DIFFERENT stack — none of them is an entitlement of this stack.
    let other_stack = "other-entitled-stack";
    emulator
        .ok(case, "CreateStack", json!({ "Name": other_stack }))
        .await;
    let mut elsewhere = entitlement_rows()[1].1.clone();
    elsewhere["StackName"] = json!(other_stack);
    emulator.ok(case, "CreateEntitlement", elsewhere).await;

    for absent in [
        "absent",
        "Minimal",
        entitlement_rows()[1].1["Name"].as_str().expect("a name"),
    ] {
        let case = format!("{case}: {absent:?}");
        let message = refusal(
            &emulator,
            &case,
            "DescribeEntitlements",
            json!({ "StackName": STACK, "Name": absent }),
            400,
            "EntitlementNotFoundException",
        )
        .await;
        assert_eq!(
            message, NOT_FOUND_SENTENCE,
            "case {case:?}: expected message {NOT_FOUND_SENTENCE:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn following_the_token_walks_every_entitlement_exactly_once() {
    let case = "paging one stack's entitlements";
    let emulator = emulator_with_stack(case, &[STACK]).await;

    // Enough records to need several pages at every page size under test.
    let stored: Vec<String> = (0..7).map(|index| format!("ent-{index}")).collect();
    for (index, name) in stored.iter().enumerate() {
        let app_visibility = if index % 2 == 0 { "ALL" } else { "ASSOCIATED" };
        emulator
            .ok(
                case,
                "CreateEntitlement",
                json!({
                    "Name": name,
                    "StackName": STACK,
                    "AppVisibility": app_visibility,
                    "Attributes": attributes(index % ATTRIBUTE_NAMES.len() + 1),
                }),
            )
            .await;
    }

    // Rows: page sizes spanning one record per page up to more than the store holds.
    for max_results in [1_usize, 2, 3, 7, 50] {
        let case = format!("{case} at MaxResults {max_results}");
        let mut seen: Vec<String> = Vec::new();
        let mut next_token: Option<String> = None;
        // A page holds at least one record, so this terminates well inside the bound.
        for _ in 0..stored.len() + 1 {
            let mut body = json!({ "StackName": STACK, "MaxResults": max_results });
            if let Some(token) = &next_token {
                body["NextToken"] = json!(token);
            }
            let read = emulator.ok(&case, "DescribeEntitlements", body).await;
            let page = entitlements(&case, &read);
            assert!(
                !page.is_empty() && page.len() <= max_results,
                "case {case:?}: a page holds 1..={max_results} records, got {read}"
            );
            for entitlement in page {
                seen.push(
                    entitlement["Name"]
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: expected a Name"))
                        .to_string(),
                );
            }
            next_token = read
                .get("NextToken")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            if next_token.is_none() {
                break;
            }
        }
        assert_eq!(
            next_token, None,
            "case {case:?}: the walk must end, got a further token {next_token:?}"
        );
        let unique: BTreeSet<&String> = seen.iter().collect();
        assert_eq!(
            unique.len(),
            seen.len(),
            "case {case:?}: every record is reported once, got {seen:?}"
        );
        let expected: BTreeSet<&String> = stored.iter().collect();
        assert_eq!(
            unique, expected,
            "case {case:?}: expected every stored entitlement, got {seen:?}"
        );
    }
}

#[tokio::test]
async fn a_page_smaller_than_one_record_is_refused() {
    let case = "MaxResults below the published floor";
    let emulator = emulator_with_stack(case, &[STACK]).await;

    for max_results in [0_i64, -1, i64::from(i32::MIN)] {
        let case = format!("{case}: {max_results}");
        let message = refusal(
            &emulator,
            &case,
            "DescribeEntitlements",
            json!({ "StackName": STACK, "MaxResults": max_results }),
            400,
            "ValidationException",
        )
        .await;
        let expected = format!("MaxResults must be at least 1, was {max_results}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn an_entitlement_does_not_outlive_its_stack() {
    for (row, request) in entitlement_rows() {
        let emulator = emulator_with_stack(row, &[STACK]).await;
        let mut body = request.clone();
        body["StackName"] = json!(STACK);
        emulator.ok(row, "CreateEntitlement", body.clone()).await;

        emulator
            .ok(row, "DeleteStack", json!({ "Name": STACK }))
            .await;
        // A stack created again under the same name starts with no entitlements, so the
        // name is free again rather than held by a record of the deleted stack.
        emulator
            .ok(row, "CreateStack", json!({ "Name": STACK }))
            .await;

        let read = emulator
            .ok(row, "DescribeEntitlements", json!({ "StackName": STACK }))
            .await;
        let listed = entitlements(row, &read);
        assert!(
            listed.is_empty(),
            "case {row:?}: the deleted stack's entitlements must be gone, got {read}"
        );
        emulator.ok(row, "CreateEntitlement", body).await;
    }
}
