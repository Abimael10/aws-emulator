// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the table control-plane STATE MACHINE: which calls leave work
//! for the clock to finish, which calls are refused while that work is outstanding, and
//! what a later reader sees.
//!
//! The rules pinned here:
//!
//! 1. only the part of an `UpdateTable` that asks DynamoDB to move capacity — the
//!    table's capacity mode, its provisioned throughput, its table class or one of its
//!    secondary indexes — is asynchronous ("while it's executing, the table status
//!    changes from ACTIVE to UPDATING" — the `UpdateTable` doc); a request that only
//!    flips the deletion-protection flag is applied with the call, so the table is
//!    reported `ACTIVE` and the next control-plane call is served;
//! 2. an `UpdateTable` that names no updatable member is invalid, not a conflict:
//!    `ValidationException`, whatever status the table is in;
//! 3. a `DeleteTable` is accepted once — a second one on a `DELETING` table conflicts
//!    with `ResourceInUseException` — and the table really disappears, after which
//!    `DescribeTable` answers `ResourceNotFoundException` and the name is free again;
//! 4. `BillingModeSummary.LastUpdateToPayPerRequestDateTime` reports the instant
//!    `PAY_PER_REQUEST` TOOK EFFECT as the capacity mode, so it is absent while the
//!    call that sets that mode is still settling and stable afterwards;
//!
//! 5. a control-plane call is AUTHORIZED before it is served, against the policies in
//!    force on the principal its access key id speaks for, for `dynamodb:<Action>` on the
//!    ARN of what it addresses — the table, the ARN a table being created would carry, or
//!    a backup of one. An explicit `Deny` refuses it whatever any grant says; otherwise it
//!    needs an `Allow` about it, and with none it is refused with IAM's implicit deny and
//!    the call has no effect. The refusal is `AccessDeniedException` with HTTP 400 (the
//!    status the service framework behind DynamoDB answers an authorization refusal with),
//!    worded from the request: the principal, the action and the resource. So an execution
//!    role carrying a CRUD-shaped grant — the item, query and scan actions plus
//!    `DescribeTable`, which is what the published SAM `DynamoDBCrudPolicy` shorthand
//!    expands to — can read and write the table's items and cannot destroy the table,
//!    change its settings, re-tag it or copy it.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use, and each rule is asserted over a
//! SET of inputs — the kind of update, the kind of table, the capacity mode, the
//! operation, the resource it addresses and what the caller's policy says — rather than
//! one recorded example.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::state::STATUS_TRANSITION;
use smithy_server_runtime::MultiServiceRouter;

// One harness for the whole test target: the router, the signed request and the two
// assertions every suite makes come from `crate::emulator` rather than a fourth copy.
use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok, REGION};

/// Wait out one transient span, so whatever the clock owes is due.
async fn settle() {
    tokio::time::sleep(STATUS_TRANSITION + Duration::from_millis(150)).await;
}

/// The `TableStatus` a description reports.
fn status_of(description: &Value) -> &Value {
    &description["TableStatus"]
}

/// Create a `PROVISIONED` table carrying one global secondary index, and wait for it to
/// become `ACTIVE`.
async fn create_active_table(router: &MultiServiceRouter, name: &str) -> Value {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "Part", "AttributeType": "S"},
                {"AttributeName": "Owner", "AttributeType": "S"},
            ],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 5, "WriteCapacityUnits": 5},
            "GlobalSecondaryIndexes": [{
                "IndexName": "ByOwner",
                "KeySchema": [{"AttributeName": "Owner", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
                "ProvisionedThroughput": {"ReadCapacityUnits": 5, "WriteCapacityUnits": 5},
            }],
        }),
    )
    .await;
    settle().await;
    let described = ok(router, "DescribeTable", json!({"TableName": name})).await;
    assert_eq!(
        status_of(&described["Table"]),
        &json!("ACTIVE"),
        "case setup {name}: the table becomes ACTIVE once the transition is due, got {described}"
    );
    described
}

#[tokio::test]
async fn an_update_is_asynchronous_only_where_it_moves_capacity() {
    let router = emulator();

    // Each row is one kind of update. `UPDATING` rows ask DynamoDB to move capacity (the
    // capacity mode, the table's throughput, its storage class, an index); the `ACTIVE`
    // rows only flip the deletion-protection flag, which the service applies with the
    // call. The second column is the status the response reports, the third whether the
    // NEXT control-plane call is served straight away.
    let cases: [(&str, Value, &str, bool); 6] = [
        (
            "deletion protection on",
            json!({"DeletionProtectionEnabled": true}),
            "ACTIVE",
            true,
        ),
        (
            "deletion protection off",
            json!({"DeletionProtectionEnabled": false}),
            "ACTIVE",
            true,
        ),
        (
            "provisioned throughput moved",
            json!({"ProvisionedThroughput": {"ReadCapacityUnits": 9, "WriteCapacityUnits": 11}}),
            "UPDATING",
            false,
        ),
        (
            "capacity mode switched",
            json!({"BillingMode": "PAY_PER_REQUEST"}),
            "UPDATING",
            false,
        ),
        (
            "table class changed",
            json!({"TableClass": "STANDARD_INFREQUENT_ACCESS"}),
            "UPDATING",
            false,
        ),
        (
            "index throughput moved",
            json!({"GlobalSecondaryIndexUpdates": [{"Update": {
                "IndexName": "ByOwner",
                "ProvisionedThroughput": {"ReadCapacityUnits": 7, "WriteCapacityUnits": 7},
            }}]}),
            "UPDATING",
            false,
        ),
    ];

    for (case, update, expected_status, next_call_served) in cases {
        let name = format!("Settling{}", case.replace(' ', ""));
        create_active_table(&router, &name).await;

        let mut request = update.clone();
        request["TableName"] = json!(name);
        let updated = ok(&router, "UpdateTable", request).await;
        let description = &updated["TableDescription"];
        assert_eq!(
            status_of(description),
            &json!(expected_status),
            "case {case}: expected TableStatus {expected_status}, got {updated}"
        );

        // The next control-plane call, issued immediately: served when the update left
        // nothing outstanding, refused with ResourceInUseException while capacity moves
        // ("While it's UPDATING, you can't issue another UpdateTable request").
        let (status, body) = call(
            &router,
            "UpdateTable",
            json!({"TableName": name, "DeletionProtectionEnabled": false}),
        )
        .await;
        if next_call_served {
            assert_eq!(
                (status, status_of(&body["TableDescription"]).clone()),
                (200, json!("ACTIVE")),
                "case {case}: the next UpdateTable is served on a settled table, got {status} \
                 with {body}"
            );
        } else {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ResourceInUseException"),
                "case {case}: a second UpdateTable while capacity moves is refused, got {status} \
                 with {body}"
            );
        }

        // Whatever the row, the table settles on its own and is then mutable again.
        settle().await;
        let described = ok(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            status_of(&described["Table"]),
            &json!("ACTIVE"),
            "case {case}: the table settles to ACTIVE, got {described}"
        );
        let dropped = ok(&router, "DeleteTable", json!({"TableName": name})).await;
        assert_eq!(
            status_of(&dropped["TableDescription"]),
            &json!("DELETING"),
            "case {case}: a settled table can be deleted, got {dropped}"
        );
    }
}

#[tokio::test]
async fn an_update_naming_no_member_is_invalid_whatever_the_table_is_doing() {
    let router = emulator();
    create_active_table(&router, "Empty").await;

    // Rows differ in what the request carries BESIDES the table name: nothing, a
    // declaration that changes nothing on its own (AttributeDefinitions only declares
    // the attributes another member's key schema uses), and lists that ask for no entry.
    let refused: [(&str, Value); 4] = [
        ("table name only", json!({})),
        (
            "attribute definitions only",
            json!({"AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}]}),
        ),
        ("an empty index update list", json!({"GlobalSecondaryIndexUpdates": []})),
        (
            "an empty list and a declaration",
            json!({
                "AttributeDefinitions": [],
                "GlobalSecondaryIndexUpdates": [],
            }),
        ),
    ];
    for (case, extra) in &refused {
        let mut request = extra.clone();
        request["TableName"] = json!("Empty");
        let (status, body) = call(&router, "UpdateTable", request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case}: an update that asks for nothing is invalid, got {status} with {body}"
        );
        let message = body["message"].as_str().unwrap_or_default().to_string();
        // The refusal has to name the alternatives, or the caller cannot fix the
        // request: the members are the operation's own updatable input members.
        for alternative in ["At least one of", "ProvisionedThroughput", "TableClass"] {
            assert!(
                message.contains(alternative),
                "case {case}: the refusal lists the alternatives (missing {alternative:?}), got \
                 {body}"
            );
        }
    }

    // A request that DOES name one of them is accepted, so the refusal above is about
    // the empty request and not about the table.
    for (case, extra) in [
        ("deletion protection", json!({"DeletionProtectionEnabled": false})),
        ("table class", json!({"TableClass": "STANDARD"})),
        (
            "throughput",
            json!({"ProvisionedThroughput": {"ReadCapacityUnits": 6, "WriteCapacityUnits": 6}}),
        ),
    ] {
        settle().await;
        let mut request = extra.clone();
        request["TableName"] = json!("Empty");
        let (status, body) = call(&router, "UpdateTable", request).await;
        assert_eq!(
            status, 200,
            "case {case}: an update naming a member is accepted, got {status} with {body}"
        );
    }

    // The same refusal while the table is in a transitional state: an empty update is
    // PERMANENTLY invalid, so it is answered before the in-use check — a conflict answer
    // would tell the caller to retry a request that can never succeed.
    let transitional: [(&str, Value); 2] = [
        (
            "while CREATING",
            json!({
                "TableName": "Fresh",
                "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
                "BillingMode": "PAY_PER_REQUEST",
            }),
        ),
        (
            "while UPDATING",
            json!({
                "TableName": "Moving",
                "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
                "BillingMode": "PROVISIONED",
                "ProvisionedThroughput": {"ReadCapacityUnits": 3, "WriteCapacityUnits": 3},
            }),
        ),
    ];
    for (case, create) in transitional {
        let name = create["TableName"].as_str().unwrap_or_default().to_string();
        ok(&router, "CreateTable", create.clone()).await;
        if case == "while UPDATING" {
            settle().await;
            ok(
                &router,
                "UpdateTable",
                json!({
                    "TableName": name,
                    "ProvisionedThroughput": {"ReadCapacityUnits": 8, "WriteCapacityUnits": 8},
                }),
            )
            .await;
        }
        let (status, body) = call(&router, "UpdateTable", json!({"TableName": name})).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case}: an empty update is invalid rather than a conflict, got {status} with \
             {body}"
        );
    }
}

#[tokio::test]
async fn a_delete_is_accepted_once_and_the_table_then_disappears() {
    let router = emulator();

    // Rows differ in the kind of table being torn down: on-demand, provisioned,
    // provisioned with a secondary index, and one holding items.
    let cases: [(&str, Value, bool); 4] = [
        (
            "on-demand table",
            json!({
                "TableName": "DropOnDemand",
                "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            false,
        ),
        (
            "provisioned table",
            json!({
                "TableName": "DropProvisioned",
                "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
                "BillingMode": "PROVISIONED",
                "ProvisionedThroughput": {"ReadCapacityUnits": 1, "WriteCapacityUnits": 1},
            }),
            false,
        ),
        (
            "table with a secondary index",
            json!({
                "TableName": "DropIndexed",
                "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
                "AttributeDefinitions": [
                    {"AttributeName": "Part", "AttributeType": "S"},
                    {"AttributeName": "Owner", "AttributeType": "S"},
                ],
                "BillingMode": "PAY_PER_REQUEST",
                "GlobalSecondaryIndexes": [{
                    "IndexName": "ByOwner",
                    "KeySchema": [{"AttributeName": "Owner", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "KEYS_ONLY"},
                }],
            }),
            false,
        ),
        (
            "table holding items",
            json!({
                "TableName": "DropWithItems",
                "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            true,
        ),
    ];

    for (case, create, with_items) in cases {
        let name = create["TableName"].as_str().unwrap_or_default().to_string();
        ok(&router, "CreateTable", create.clone()).await;
        settle().await;
        if with_items {
            ok(
                &router,
                "PutItem",
                json!({"TableName": name, "Item": {"Part": {"S": "one"}}}),
            )
            .await;
        }

        let dropped = ok(&router, "DeleteTable", json!({"TableName": name})).await;
        assert_eq!(
            status_of(&dropped["TableDescription"]),
            &json!("DELETING"),
            "case {case}: the accepted delete reports DELETING, got {dropped}"
        );

        // A second delete is a conflict, not a second acceptance: the table is a
        // resource which is still in use until the deletion completes.
        let (status, body) = call(&router, "DeleteTable", json!({"TableName": name})).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ResourceInUseException"),
            "case {case}: a second DeleteTable while DELETING conflicts, got {status} with {body}"
        );
        let deleting = ok(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            status_of(&deleting["Table"]),
            &json!("DELETING"),
            "case {case}: the table is still readable as DELETING, got {deleting}"
        );

        // Once the deletion is due the table is GONE: a reader can observe completion.
        settle().await;
        for operation in ["DescribeTable", "DeleteTable"] {
            let (status, body) = call(&router, operation, json!({"TableName": name})).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ResourceNotFoundException"),
                "case {case}: {operation} on the deleted table reports it gone, got {status} \
                 with {body}"
            );
        }
        // ...and the name is free, so a recreate does not collide.
        let recreated = ok(&router, "CreateTable", create.clone()).await;
        assert_eq!(
            status_of(&recreated["TableDescription"]),
            &json!("CREATING"),
            "case {case}: the deleted name can be used again, got {recreated}"
        );
    }
}

#[tokio::test]
async fn deletion_protection_refuses_the_delete_until_an_update_clears_it() {
    let router = emulator();

    // The flag can be set at create time or by a later update; both leave the table
    // undeletable until an update clears it.
    let cases: [(&str, bool); 2] = [("protected at create", true), ("protected by update", false)];
    for (case, protected_at_create) in cases {
        let name = format!("Protected{}", case.replace(' ', ""));
        ok(
            &router,
            "CreateTable",
            json!({
                "TableName": name,
                "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
                "BillingMode": "PAY_PER_REQUEST",
                "DeletionProtectionEnabled": protected_at_create,
            }),
        )
        .await;
        settle().await;
        if !protected_at_create {
            let guarded = ok(
                &router,
                "UpdateTable",
                json!({"TableName": name, "DeletionProtectionEnabled": true}),
            )
            .await;
            assert_eq!(
                status_of(&guarded["TableDescription"]),
                &json!("ACTIVE"),
                "case {case}: flipping the flag leaves the table settled, got {guarded}"
            );
        }

        let (status, body) = call(&router, "DeleteTable", json!({"TableName": name})).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case}: a protected table is refused rather than deleted, got {status} with \
             {body}"
        );
        let still_there = ok(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            (
                status_of(&still_there["Table"]).clone(),
                still_there["Table"]["DeletionProtectionEnabled"].clone(),
            ),
            (json!("ACTIVE"), json!(true)),
            "case {case}: the refused delete left the table exactly as it was, got {still_there}"
        );

        // Clearing the flag is served straight away, and the delete then goes through to
        // completion.
        ok(
            &router,
            "UpdateTable",
            json!({"TableName": name, "DeletionProtectionEnabled": false}),
        )
        .await;
        let dropped = ok(&router, "DeleteTable", json!({"TableName": name})).await;
        assert_eq!(
            status_of(&dropped["TableDescription"]),
            &json!("DELETING"),
            "case {case}: the unprotected table is deleted, got {dropped}"
        );
        settle().await;
        let (status, body) = call(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case}: the delete completes and the table is gone, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn pay_per_request_is_dated_from_the_call_that_set_the_mode() {
    let router = emulator();

    /// The `LastUpdateToPayPerRequestDateTime` of a description, when present.
    fn dated(description: &Value) -> Option<f64> {
        description["BillingModeSummary"]["LastUpdateToPayPerRequestDateTime"].as_f64()
    }

    // A table's capacity mode at CREATE time: only a table that has been on-demand
    // carries a `BillingModeSummary` at all, and the instant it reports is the instant of
    // the call that set the mode — for a create, the table's own creation instant.
    let cases: [(&str, &str, bool); 2] = [
        ("created on-demand", "PAY_PER_REQUEST", true),
        ("created provisioned", "PROVISIONED", false),
    ];
    for (case, mode, on_demand) in cases {
        let name = format!("Dated{}", case.replace(' ', ""));
        let mut create = json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
            "BillingMode": mode,
        });
        if mode == "PROVISIONED" {
            create["ProvisionedThroughput"] =
                json!({"ReadCapacityUnits": 2, "WriteCapacityUnits": 2});
        }
        let created = ok(&router, "CreateTable", create).await;
        assert_eq!(
            (
                created["TableDescription"]["TableThroughputModeSummary"]
                    ["TableThroughputMode"]
                    .clone(),
                dated(&created["TableDescription"]),
            ),
            (
                json!(mode),
                on_demand.then(|| {
                    created["TableDescription"]["CreationDateTime"]
                        .as_f64()
                        .unwrap_or_default()
                }),
            ),
            "case {case}: the create response reports the mode, and the creation instant \
             for a table it created on-demand, got {created}"
        );

        settle().await;
        let described = ok(&router, "DescribeTable", json!({"TableName": name})).await;
        let first = dated(&described["Table"]);
        assert_eq!(
            first.is_some(),
            on_demand,
            "case {case}: expected dated={on_demand} once ACTIVE, got {described}"
        );
        if let Some(instant) = first {
            let creation = described["Table"]["CreationDateTime"]
                .as_f64()
                .unwrap_or_default();
            assert_eq!(
                instant, creation,
                "case {case}: a table created on-demand had the mode set when it was \
                 created, got {instant} against {creation}"
            );
        }
        // Reading twice reports the same instant: it records when the mode was set, and
        // a read does not re-set it.
        let again = ok(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            dated(&again["Table"]),
            first,
            "case {case}: the instant is stable across reads, got {again}"
        );
    }

    // Switching an existing table's mode: the capacity move is asynchronous, but the
    // instant the member reports is the instant of the CALL, so the switch's own response
    // already carries it and every later description reports the same one. Switching back
    // to PROVISIONED does not erase when PAY_PER_REQUEST was last set.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": "Switched",
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 2, "WriteCapacityUnits": 2},
        }),
    )
    .await;
    settle().await;
    let switched = ok(
        &router,
        "UpdateTable",
        json!({"TableName": "Switched", "BillingMode": "PAY_PER_REQUEST"}),
    )
    .await;
    let switch_echo = dated(&switched["TableDescription"]);
    assert_eq!(
        (
            status_of(&switched["TableDescription"]).clone(),
            switch_echo.is_some(),
        ),
        (json!("UPDATING"), true),
        "case switch to on-demand: the capacity move is still in flight, and the instant \
         the switch was asked for is already reported, got {switched}"
    );
    settle().await;
    let in_effect = ok(&router, "DescribeTable", json!({"TableName": "Switched"})).await;
    let switch_instant = dated(&in_effect["Table"]);
    assert!(
        switch_instant == switch_echo && status_of(&in_effect["Table"]) == &json!("ACTIVE"),
        "case switch to on-demand: the instant the switch reported stands once it settles, \
         got {in_effect}"
    );

    ok(
        &router,
        "UpdateTable",
        json!({
            "TableName": "Switched",
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 4, "WriteCapacityUnits": 4},
        }),
    )
    .await;
    settle().await;
    let back = ok(&router, "DescribeTable", json!({"TableName": "Switched"})).await;
    assert_eq!(
        (
            back["Table"]["BillingModeSummary"]["BillingMode"].clone(),
            dated(&back["Table"]),
        ),
        (json!("PROVISIONED"), switch_instant),
        "case switch back to provisioned: the recorded instant is when PAY_PER_REQUEST was last \
         set, got {back}"
    );
}

// ---------------------------------------------------------------------------------------
// Rule 5: who may make a control-plane call.
//
// The calls below need the two services the decision is read from beside this one on the
// same engine — the role lives in IAM's store and its session credential is minted by STS,
// exactly as `core/router` assembles the three — so they build their own router rather than
// the single-service one the state-machine tests above drive.
// ---------------------------------------------------------------------------------------

/// The account every generated emulator presents.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The session name every `AssumeRole` below asks for, which is the last segment of the
/// assumed-role ARN a refusal names.
const SESSION: &str = "probe";

/// The policy-language version every document below states.
const POLICY_VERSION: &str = "2012-10-17";

/// The action set the published `DynamoDBCrudPolicy` SAM shorthand grants over one table:
/// the item, query and scan actions plus `DescribeTable`, and nothing that changes the
/// table itself ("AWS SAM policy templates", `DynamoDBCrudPolicy`, whose entries are the
/// ones this crate's own expansion carries).
const CRUD_ACTIONS: &[&str] = &[
    "dynamodb:GetItem",
    "dynamodb:DeleteItem",
    "dynamodb:PutItem",
    "dynamodb:Scan",
    "dynamodb:Query",
    "dynamodb:UpdateItem",
    "dynamodb:BatchWriteItem",
    "dynamodb:BatchGetItem",
    "dynamodb:DescribeTable",
    "dynamodb:ConditionCheckItem",
];

/// One control-plane call: the operation, the IAM action it performs, and the body that
/// addresses the table.
struct ControlCall {
    operation: &'static str,
    action: &'static str,
    body: Value,
}

/// Every control-plane call these rows drive against the table `table`, each with the
/// action it performs — `dynamodb:<OperationName>` ("Actions defined by Amazon DynamoDB",
/// IAM Service Authorization Reference) — and the body that addresses that table. The
/// class spans a describe, a change, a tag write and read, a backup, a policy read and the
/// delete.
fn control_calls(table: &str) -> Vec<ControlCall> {
    vec![
        ControlCall {
            operation: "DescribeTable",
            action: "dynamodb:DescribeTable",
            body: json!({"TableName": table}),
        },
        ControlCall {
            operation: "UpdateTable",
            action: "dynamodb:UpdateTable",
            body: json!({"TableName": table, "DeletionProtectionEnabled": true}),
        },
        ControlCall {
            operation: "UpdateTimeToLive",
            action: "dynamodb:UpdateTimeToLive",
            body: json!({
                "TableName": table,
                "TimeToLiveSpecification": {"Enabled": true, "AttributeName": "ttl"},
            }),
        },
        ControlCall {
            operation: "DescribeTimeToLive",
            action: "dynamodb:DescribeTimeToLive",
            body: json!({"TableName": table}),
        },
        ControlCall {
            operation: "DescribeContinuousBackups",
            action: "dynamodb:DescribeContinuousBackups",
            body: json!({"TableName": table}),
        },
        ControlCall {
            operation: "DescribeKinesisStreamingDestination",
            action: "dynamodb:DescribeKinesisStreamingDestination",
            body: json!({"TableName": table}),
        },
        ControlCall {
            operation: "DescribeContributorInsights",
            action: "dynamodb:DescribeContributorInsights",
            body: json!({"TableName": table}),
        },
        ControlCall {
            operation: "TagResource",
            action: "dynamodb:TagResource",
            body: json!({
                "ResourceArn": table_resource_arn(table),
                "Tags": [{"Key": "owner", "Value": "probe"}],
            }),
        },
        ControlCall {
            operation: "ListTagsOfResource",
            action: "dynamodb:ListTagsOfResource",
            body: json!({"ResourceArn": table_resource_arn(table)}),
        },
        ControlCall {
            operation: "CreateBackup",
            action: "dynamodb:CreateBackup",
            body: json!({"TableName": table, "BackupName": "control-plane-probe"}),
        },
        ControlCall {
            operation: "GetResourcePolicy",
            action: "dynamodb:GetResourcePolicy",
            body: json!({"ResourceArn": table_resource_arn(table)}),
        },
        // The DELETE comes last, so a row whose policy allows everything still finds the
        // table there for every call above it.
        ControlCall {
            operation: "DeleteTable",
            action: "dynamodb:DeleteTable",
            body: json!({"TableName": table}),
        },
    ]
}

/// The emulator the authorization rows drive: this service, IAM and STS over ONE engine.
fn authorizing_emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Unsigned requests: the `Authorization` header still names the Region and the access
    // key id, which is what the service reads the caller's identity from.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// Send one DynamoDB operation as the session `access_key_id` names.
async fn as_session(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/dynamodb/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Post one awsQuery form body to `signing_name` as the account's own credential.
async fn query(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Percent-encode one `application/x-www-form-urlencoded` component through the maintained
/// encoder rather than by hand.
fn form_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of the first `<name>…</name>` element of an XML body.
fn element(case: &str, body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let (_, rest) = body
        .split_once(&open)
        .unwrap_or_else(|| panic!("case {case:?}: the answer must carry <{name}>, got {body}"));
    let (text, _) = rest
        .split_once(&close)
        .unwrap_or_else(|| panic!("case {case:?}: <{name}> must be closed, got {body}"));
    text.to_string()
}

/// A trust policy letting the account assume the role.
fn trust_policy() -> String {
    json!({
        "Version": POLICY_VERSION,
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create one IAM role carrying `document` as its inline policy (when it carries one) and
/// answer the access key id of a session of it — the credential a call signs with, exactly
/// as a function's execution role is reached.
async fn session_of(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    document: Option<&str>,
) -> String {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={}&AssumeRolePolicyDocument={}",
            form_encode(role),
            form_encode(&trust_policy())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({role}) must answer 200, got {status} with {body}"
    );
    let role_arn = element(case, &body, "Arn");
    if let Some(document) = document {
        let (status, body) = query(
            router,
            "iam",
            format!(
                "Action=PutRolePolicy&Version=2010-05-08&RoleName={}&PolicyName=probe&\
                 PolicyDocument={}",
                form_encode(role),
                form_encode(document)
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutRolePolicy({role}) must answer 200, got {status} with {body}"
        );
    }
    let (status, body) = query(
        router,
        "sts",
        format!(
            "Action=AssumeRole&Version=2011-06-15&RoleArn={}&RoleSessionName={SESSION}",
            form_encode(&role_arn)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole({role_arn}) must answer 200, got {status} with {body}"
    );
    element(case, &body, "AccessKeyId")
}

/// One identity-based statement list, as a document.
fn identity_policy(statements: Value) -> String {
    json!({"Version": POLICY_VERSION, "Statement": statements}).to_string()
}

/// The ARN of one table of this account in [`REGION`], in the format AWS documents for a
/// DynamoDB table (`arn:aws:dynamodb:<region>:<account>:table/<name>`).
fn table_resource_arn(name: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{name}")
}

/// The principal an assumed-role session of `role` speaks for, as `sts:GetCallerIdentity`
/// reports it and a refusal names it.
fn session_principal(role: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{SESSION}")
}

/// The sentence AWS refuses an unauthorized request with.
fn refusal(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// The reason clause of IAM's implicit deny for `action`.
fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// Create one on-demand table as the account's own credential and wait for it to be ACTIVE.
async fn created_table(router: &MultiServiceRouter, case: &str, name: &str) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, case, name).await;
}

#[tokio::test]
async fn a_crud_shaped_grant_serves_the_items_and_refuses_the_table_itself() {
    let router = authorizing_emulator();
    let case = "a CRUD-shaped grant";
    let table = "CrudScoped";
    let role = "ddb-crud-scoped";
    created_table(&router, case, table).await;

    let document = identity_policy(json!([{
        "Effect": "Allow",
        "Action": CRUD_ACTIONS,
        "Resource": [
            table_resource_arn(table),
            format!("{}/index/*", table_resource_arn(table)),
        ],
    }]));
    let session = session_of(&router, case, role, Some(&document)).await;

    // The DATA plane the grant is for is served, so the row really does hold the grant.
    let (status, written) = as_session(
        &router,
        &session,
        "PutItem",
        json!({"TableName": table, "Item": {"pk": {"S": "k0"}}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?} / PutItem: the grant covers the item write, got {status} with {written}"
    );

    for ControlCall {
        operation,
        action,
        body,
    } in control_calls(table)
    {
        let (status, answered) = as_session(&router, &session, operation, body.clone()).await;
        if CRUD_ACTIONS.contains(&action) {
            assert_eq!(
                status, 200,
                "case {case:?} / {operation}: the grant covers {action}, so the call is served, \
                 got {status} with {answered}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "AccessDeniedException"),
            "case {case:?} / {operation}: expected the denial of {action}, got {status} with \
             {answered}"
        );
        assert_eq!(
            message(&answered),
            refusal(
                &session_principal(role),
                action,
                &table_resource_arn(table),
                &no_grant(action),
            ),
            "case {case:?} / {operation}: expected the refusal to be built from the request, got \
             {:?}",
            message(&answered)
        );
    }

    // Nothing the refused calls asked for happened: the table is still there, carries no
    // tag, no TTL and no deletion protection, and the account holds no backup of it.
    let described = ok(&router, "DescribeTable", json!({"TableName": table})).await;
    assert_eq!(
        (
            status_of(&described["Table"]).clone(),
            described["Table"]["DeletionProtectionEnabled"].clone(),
        ),
        (json!("ACTIVE"), json!(false)),
        "case {case:?}: the refused DeleteTable and UpdateTable changed nothing, got {described}"
    );
    let tags = ok(
        &router,
        "ListTagsOfResource",
        json!({"ResourceArn": table_resource_arn(table)}),
    )
    .await;
    assert_eq!(
        tags["Tags"],
        json!([]),
        "case {case:?}: the refused TagResource stored no tag, got {tags}"
    );
    let ttl = ok(&router, "DescribeTimeToLive", json!({"TableName": table})).await;
    assert_eq!(
        ttl["TimeToLiveDescription"]["TimeToLiveStatus"],
        json!("DISABLED"),
        "case {case:?}: the refused UpdateTimeToLive stored no specification, got {ttl}"
    );
    let backups = ok(&router, "ListBackups", json!({"TableName": table})).await;
    assert_eq!(
        backups["BackupSummaries"],
        json!([]),
        "case {case:?}: the refused CreateBackup catalogued nothing, got {backups}"
    );
}

#[tokio::test]
async fn a_role_granted_nothing_is_refused_every_control_plane_call() {
    let router = authorizing_emulator();
    let case = "a role the account grants nothing";
    let table = "GrantedNothing";
    let role = "ddb-control-none";
    created_table(&router, case, table).await;
    let session = session_of(&router, case, role, None).await;

    for ControlCall {
        operation,
        action,
        body,
    } in control_calls(table)
    {
        let (status, answered) = as_session(&router, &session, operation, body.clone()).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "AccessDeniedException"),
            "case {case:?} / {operation}: expected the denial of {action}, got {status} with \
             {answered}"
        );
        assert_eq!(
            message(&answered),
            refusal(
                &session_principal(role),
                action,
                &table_resource_arn(table),
                &no_grant(action),
            ),
            "case {case:?} / {operation}: expected the refusal to be built from the request, got \
             {:?}",
            message(&answered)
        );
    }
}

#[tokio::test]
async fn a_wildcard_grant_serves_every_control_plane_call() {
    let router = authorizing_emulator();
    let case = "a wildcard over this service";
    let table = "WildcardGranted";
    created_table(&router, case, table).await;
    let document = identity_policy(json!([{
        "Effect": "Allow",
        "Action": "dynamodb:*",
        "Resource": "*",
    }]));
    let session = session_of(&router, case, "ddb-control-wildcard", Some(&document)).await;

    for ControlCall {
        operation,
        action,
        body,
    } in control_calls(table)
    {
        // The delete is driven below instead: the `UpdateTable` above turns deletion
        // protection ON, which is a setting of the table rather than an authorization
        // decision, and it has to be cleared before any caller may delete the table.
        if operation == "DeleteTable" {
            continue;
        }
        let (status, answered) = as_session(&router, &session, operation, body.clone()).await;
        // Authorization is past for every one of them. What answers then is the
        // operation's own business: a table carrying no resource-based policy has none to
        // report, which is `GetResourcePolicy`'s own modelled refusal and not a denial.
        if operation == "GetResourcePolicy" {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "PolicyNotFoundException"),
                "case {case:?} / {operation}: the wildcard covers {action}, so the answer is the \
                 operation's own refusal for a table with no policy, got {status} with {answered}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?} / {operation}: the wildcard covers {action}, so the call is served, \
             got {status} with {answered}"
        );
    }

    ok(
        &router,
        "UpdateTable",
        json!({"TableName": table, "DeletionProtectionEnabled": false}),
    )
    .await;
    let (status, answered) =
        as_session(&router, &session, "DeleteTable", json!({"TableName": table})).await;
    assert_eq!(
        status, 200,
        "case {case:?} / DeleteTable: the wildcard covers dynamodb:DeleteTable, so the call is \
         served, got {status} with {answered}"
    );
}

#[tokio::test]
async fn an_explicit_deny_refuses_one_action_of_a_wildcard_grant() {
    let router = authorizing_emulator();
    let case = "an explicit deny over a wildcard allow";
    let table = "ExplicitlyDenied";
    let role = "ddb-control-denied";
    created_table(&router, case, table).await;
    let document = identity_policy(json!([
        {"Effect": "Allow", "Action": "dynamodb:*", "Resource": "*"},
        {"Effect": "Deny", "Action": "dynamodb:DeleteTable", "Resource": "*"},
    ]));
    let session = session_of(&router, case, role, Some(&document)).await;

    let (status, described) = as_session(
        &router,
        &session,
        "DescribeTable",
        json!({"TableName": table}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the deny names one action, so describing the table is served, got \
         {status} with {described}"
    );

    let (status, answered) =
        as_session(&router, &session, "DeleteTable", json!({"TableName": table})).await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "AccessDeniedException"),
        "case {case:?}: an explicit deny refuses the delete, got {status} with {answered}"
    );
    assert_eq!(
        message(&answered),
        refusal(
            &session_principal(role),
            "dynamodb:DeleteTable",
            &table_resource_arn(table),
            "with an explicit deny in an identity-based policy",
        ),
        "case {case:?}: the refusal names the document that denied it, got {:?}",
        message(&answered)
    );
    let described = ok(&router, "DescribeTable", json!({"TableName": table})).await;
    assert_eq!(
        status_of(&described["Table"]).clone(),
        json!("ACTIVE"),
        "case {case:?}: the refused DeleteTable left the table ACTIVE, got {described}"
    );
}

#[tokio::test]
async fn a_call_that_addresses_a_resource_of_its_own_is_decided_on_that_arn() {
    let router = authorizing_emulator();
    let case = "a create and a backup, which address a resource of their own";
    let role = "ddb-control-absent";
    let session = session_of(&router, case, role, None).await;

    // A CREATE addresses the ARN the table WOULD carry: there is no table yet, so the
    // decision rests on the caller's own documents and the refusal names that ARN.
    let created = "NotCreated";
    let (status, answered) = as_session(
        &router,
        &session,
        "CreateTable",
        json!({
            "TableName": created,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "AccessDeniedException"),
        "case {case:?} / CreateTable: expected the denial, got {status} with {answered}"
    );
    assert_eq!(
        message(&answered),
        refusal(
            &session_principal(role),
            "dynamodb:CreateTable",
            &table_resource_arn(created),
            &no_grant("dynamodb:CreateTable"),
        ),
        "case {case:?} / CreateTable: the refusal names the ARN the create addressed, got {:?}",
        message(&answered)
    );
    let (status, missing) = call(&router, "DescribeTable", json!({"TableName": created})).await;
    assert_eq!(
        (status, error_code(&missing).as_str()),
        (400, "ResourceNotFoundException"),
        "case {case:?}: the refused create registered no table, got {status} with {missing}"
    );

    // A call that addresses a BACKUP is decided on the BACKUP's own ARN, which is read off
    // the backup the account's own credential made rather than built here.
    let table = "BackedUp";
    created_table(&router, case, table).await;
    let backup = ok(
        &router,
        "CreateBackup",
        json!({"TableName": table, "BackupName": "control-plane-probe"}),
    )
    .await;
    let backup_arn = backup["BackupDetails"]["BackupArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !backup_arn.is_empty(),
        "case {case:?}: the backup reports its own ARN, got {backup}"
    );
    for (operation, action) in [
        ("DescribeBackup", "dynamodb:DescribeBackup"),
        ("DeleteBackup", "dynamodb:DeleteBackup"),
    ] {
        let (status, answered) = as_session(
            &router,
            &session,
            operation,
            json!({"BackupArn": backup_arn}),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "AccessDeniedException"),
            "case {case:?} / {operation}: expected the denial of {action}, got {status} with \
             {answered}"
        );
        assert_eq!(
            message(&answered),
            refusal(
                &session_principal(role),
                action,
                &backup_arn,
                &no_grant(action),
            ),
            "case {case:?} / {operation}: the refusal names the backup the call addressed, got \
             {:?}",
            message(&answered)
        );
    }
    let still = ok(&router, "DescribeBackup", json!({"BackupArn": backup_arn})).await;
    assert_eq!(
        still["BackupDescription"]["BackupDetails"]["BackupArn"],
        json!(backup_arn),
        "case {case:?}: the refused DeleteBackup removed nothing, got {still}"
    );
}
