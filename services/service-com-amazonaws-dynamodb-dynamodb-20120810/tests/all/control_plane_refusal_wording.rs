// SPDX-License-Identifier: CC0-1.0

//! WHAT the control plane SAYS when it turns a table change down: the sentence AWS words
//! each refusal with, driven end to end over this service's own wire protocol.
//!
//! Every rule here is about MESSAGE TEXT and ERROR CLASS, which `ValidationException`,
//! `ResourceInUseException` and `LimitExceededException` model none of — the model carries
//! no message member text at all — so the wording is the whole contract, and a client that
//! branches on it sees a difference whenever the text is not AWS's. The rules:
//!
//! * a `Projection` that states `NonKeyAttributes` under a `ProjectionType` which projects
//!   no such list is refused over the MEMBER PAIR, naming the type the request stated and
//!   which of the two members it carried — the mirror image of the sentence an `INCLUDE`
//!   projection naming none earns (`Projection$ProjectionType` /
//!   `Projection$NonKeyAttributes`, read with
//!   `smithy_query(query="shape",shape="com.amazonaws.dynamodb#Projection")`);
//! * an `OnDemandThroughput` ceiling outside the values its member takes — "set the value
//!   of MaxReadRequestUnits as greater than or equal to 1 ... to remove the maximum ... set
//!   the value of MaxReadRequestUnits to -1"
//!   (`smithy_query(query="shape",shape="com.amazonaws.dynamodb#OnDemandThroughput")`) —
//!   is refused as a RANGE failure naming that member, with nothing of the value sent;
//! * a `GlobalSecondaryIndexUpdates` list that names one index in more than one entry is
//!   refused over the rule it broke, naming that index;
//! * a `Create` naming an index the table already carries is refused with the fixed
//!   sentence for that work, naming neither index nor table;
//! * a second change to a table whose update has not settled is refused as the resource in
//!   use, and WHAT is still running decides the sentence: a capacity move reads as the
//!   table's IOPS being updated, every other unsettled update as the table being updated.
//!
//! Each test drives its rule over a TABLE of inputs — every projection type but `INCLUDE`,
//! both ceiling members over several values outside the range and the values inside it,
//! every combination of actions that can name one index twice, and both kinds of work in
//! flight — rather than the one request each was reported for.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The projection types the model declares (`Projection$ProjectionType`), read with
/// `smithy_query(query="shape",shape="com.amazonaws.dynamodb#Projection")`.
const PROJECTION_TYPES: [&str; 3] = ["KEYS_ONLY", "INCLUDE", "ALL"];

/// The sentence a projection naming `NonKeyAttributes` under `projection_type` earns.
fn attributes_outside_include(projection_type: &str) -> String {
    format!(
        "One or more parameter values were invalid: ProjectionType is {projection_type}, but \
         NonKeyAttributes is specified"
    )
}

/// The sentence a ceiling value outside the range `member` takes earns.
fn ceiling_outside_range(member: &str) -> String {
    format!(
        "One or more parameter values were invalid: Requested {member} for OnDemandThroughput \
         for table is outside of valid range"
    )
}

/// The sentence a list naming `index` in more than one entry earns.
fn one_update_per_index(index: &str) -> String {
    format!(
        "One or more parameter values were invalid: Only one global secondary index update per \
         index is allowed simultaneously. Index: {index}"
    )
}

/// The refusal (code, message) of a call that must be turned down.
async fn refused(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> (String, String) {
    let (status, answered) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?} ({operation}): expected the call to be refused with 400, got {status} \
         with {answered}"
    );
    (error_code(&answered), message(&answered))
}

/// A `CreateTable` body for an on-demand table with one global secondary index whose
/// projection is `projection`.
fn create_with_projection(name: &str, projection: Value) -> Value {
    json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [
            {"AttributeName": "pk", "AttributeType": "S"},
            {"AttributeName": "gk", "AttributeType": "S"},
        ],
        "BillingMode": "PAY_PER_REQUEST",
        "GlobalSecondaryIndexes": [{
            "IndexName": "by-gk",
            "KeySchema": [{"AttributeName": "gk", "KeyType": "HASH"}],
            "Projection": projection,
        }],
    })
}

#[tokio::test]
async fn non_key_attributes_outside_include_are_refused_over_the_member_pair() {
    let router = emulator();

    // Every projection type but INCLUDE, over the three places a `Projection` is
    // declared: with the table's global index, with its local index, and with an index an
    // UpdateTable adds. The attribute list varies too — one name and several — because
    // the rule is about the member being STATED rather than about what it holds.
    create_active_table_named(&router, "proj-subject").await;
    for projection_type in PROJECTION_TYPES.iter().filter(|kind| **kind != "INCLUDE") {
        for attributes in [json!(["note"]), json!(["note", "rank"])] {
            let projection = json!({
                "ProjectionType": projection_type,
                "NonKeyAttributes": attributes.clone(),
            });
            let case = format!("{projection_type} with NonKeyAttributes {attributes}");

            let (code, stated) = refused(
                &router,
                &case,
                "CreateTable",
                create_with_projection(&format!("proj-{projection_type}"), projection.clone()),
            )
            .await;
            assert_eq!(
                (code.as_str(), stated.as_str()),
                (
                    "ValidationException",
                    attributes_outside_include(projection_type).as_str()
                ),
                "case {case:?} (CreateTable, global index): expected the member-pair sentence, \
                 got {code} with {stated:?}"
            );

            let (code, stated) = refused(
                &router,
                &case,
                "CreateTable",
                json!({
                    "TableName": format!("proj-lsi-{projection_type}"),
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"},
                                  {"AttributeName": "sk", "KeyType": "RANGE"}],
                    "AttributeDefinitions": [
                        {"AttributeName": "pk", "AttributeType": "S"},
                        {"AttributeName": "sk", "AttributeType": "S"},
                        {"AttributeName": "lk", "AttributeType": "S"},
                    ],
                    "BillingMode": "PAY_PER_REQUEST",
                    "LocalSecondaryIndexes": [{
                        "IndexName": "by-lk",
                        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"},
                                      {"AttributeName": "lk", "KeyType": "RANGE"}],
                        "Projection": projection.clone(),
                    }],
                }),
            )
            .await;
            assert_eq!(
                (code.as_str(), stated.as_str()),
                (
                    "ValidationException",
                    attributes_outside_include(projection_type).as_str()
                ),
                "case {case:?} (CreateTable, local index): expected the same sentence as for a \
                 global index, got {code} with {stated:?}"
            );

            let (code, stated) = refused(
                &router,
                &case,
                "UpdateTable",
                json!({
                    "TableName": "proj-subject",
                    "AttributeDefinitions": [{"AttributeName": "gk", "AttributeType": "S"}],
                    "GlobalSecondaryIndexUpdates": [{"Create": {
                        "IndexName": format!("added-{projection_type}"),
                        "KeySchema": [{"AttributeName": "gk", "KeyType": "HASH"}],
                        "Projection": projection.clone(),
                    }}],
                }),
            )
            .await;
            assert_eq!(
                (code.as_str(), stated.as_str()),
                (
                    "ValidationException",
                    attributes_outside_include(projection_type).as_str()
                ),
                "case {case:?} (UpdateTable Create): expected the same sentence as for a create, \
                 got {code} with {stated:?}"
            );
        }
    }

    // The pair that DOES hold together is served, so the rows above are about the
    // contradiction and not about the member being present at all.
    ok(
        &router,
        "CreateTable",
        create_with_projection(
            "proj-include",
            json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["note"]}),
        ),
    )
    .await;
}

#[tokio::test]
async fn an_on_demand_ceiling_outside_its_range_is_refused_over_its_member() {
    let router = emulator();
    create_active_table_named(&router, "ceiling-subject").await;

    // Both members, over the values no ceiling can hold: zero, the negative that is not
    // the removal value, and a figure far below it. The sentence names the member and
    // nothing of the value, so every row of one member reads alike.
    for member in ["MaxReadRequestUnits", "MaxWriteRequestUnits"] {
        for value in [0_i64, -2, -1_000_000, i64::MIN] {
            let ceiling = json!({member: value});
            let case = format!("{member} = {value}");

            let (code, stated) = refused(
                &router,
                &case,
                "CreateTable",
                json!({
                    "TableName": "ceiling-created",
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                    "BillingMode": "PAY_PER_REQUEST",
                    "OnDemandThroughput": ceiling.clone(),
                }),
            )
            .await;
            assert_eq!(
                (code.as_str(), stated.as_str()),
                ("ValidationException", ceiling_outside_range(member).as_str()),
                "case {case:?} (CreateTable): expected the range sentence, got {code} with \
                 {stated:?}"
            );

            let (code, stated) = refused(
                &router,
                &case,
                "UpdateTable",
                json!({"TableName": "ceiling-subject", "OnDemandThroughput": ceiling.clone()}),
            )
            .await;
            assert_eq!(
                (code.as_str(), stated.as_str()),
                ("ValidationException", ceiling_outside_range(member).as_str()),
                "case {case:?} (UpdateTable): expected the same sentence as for a create, got \
                 {code} with {stated:?}"
            );
        }
    }

    // The values the member DOES take are served: 1 or more sets a maximum, and -1
    // removes one, so the refusals above are about the range and not about the member.
    for (case, ceiling) in [
        ("the documented minimum", json!({"MaxReadRequestUnits": 1})),
        (
            "both members set",
            json!({"MaxReadRequestUnits": 10, "MaxWriteRequestUnits": 20}),
        ),
        (
            "the removal value",
            json!({"MaxReadRequestUnits": -1, "MaxWriteRequestUnits": -1}),
        ),
    ] {
        let answered = ok(
            &router,
            "UpdateTable",
            json!({"TableName": "ceiling-subject", "OnDemandThroughput": ceiling}),
        )
        .await;
        assert!(
            answered["TableDescription"].is_object(),
            "case {case:?}: a ceiling inside the range is served, got {answered}"
        );
        describe_when_active(&router, case, "ceiling-subject").await;
    }
}

/// One `GlobalSecondaryIndexUpdates` entry per action, over the index `name`.
fn update_entry(action: &str, name: &str) -> Value {
    match action {
        "Create" => json!({"Create": {
            "IndexName": name,
            "KeySchema": [{"AttributeName": "gk", "KeyType": "HASH"}],
            "Projection": {"ProjectionType": "ALL"},
        }}),
        "Delete" => json!({"Delete": {"IndexName": name}}),
        _ => json!({"Update": {
            "IndexName": name,
            "ProvisionedThroughput": {"ReadCapacityUnits": 2, "WriteCapacityUnits": 2},
        }}),
    }
}

#[tokio::test]
async fn a_list_naming_one_index_twice_is_refused_over_that_index() {
    let router = emulator();
    create_active_table_named(&router, "updates-subject").await;

    // Every way one list can name one index more than once: each pair of actions, the
    // same action twice, and three entries over one name. The index name varies too,
    // since it is the one variable part of the sentence.
    let combinations = [
        (vec!["Delete", "Delete"], "idx-a"),
        (vec!["Create", "Create"], "idx-b"),
        (vec!["Update", "Update"], "idx-c"),
        (vec!["Create", "Delete"], "idx-d"),
        (vec!["Update", "Delete"], "idx.e-1"),
        (vec!["Delete", "Delete", "Delete"], "idx-f"),
    ];
    for (actions, index) in combinations {
        let entries: Vec<Value> = actions
            .iter()
            .map(|action| update_entry(action, index))
            .collect();
        let case = format!("{actions:?} over {index}");
        let (code, stated) = refused(
            &router,
            &case,
            "UpdateTable",
            json!({
                "TableName": "updates-subject",
                "AttributeDefinitions": [{"AttributeName": "gk", "AttributeType": "S"}],
                "GlobalSecondaryIndexUpdates": entries,
            }),
        )
        .await;
        assert_eq!(
            (code.as_str(), stated.as_str()),
            ("ValidationException", one_update_per_index(index).as_str()),
            "case {case:?}: expected the one-update-per-index sentence, got {code} with {stated:?}"
        );
    }

    // A list naming two DIFFERENT indexes breaks no such rule: it is held to the limit on
    // how much index lifecycle work one call carries, which is the control plane's limit
    // error and not this sentence.
    let (code, stated) = refused(
        &router,
        "one delete of each of two indexes",
        "UpdateTable",
        json!({
            "TableName": "updates-subject",
            "GlobalSecondaryIndexUpdates": [
                update_entry("Delete", "idx-a"),
                update_entry("Delete", "idx-b"),
            ],
        }),
    )
    .await;
    assert_eq!(
        code, "LimitExceededException",
        "case one delete of each of two indexes: expected the per-call limit error, got {code} \
         with {stated:?}"
    );
}

#[tokio::test]
async fn a_create_of_an_index_the_table_carries_states_the_work_it_cannot_do() {
    let router = emulator();
    // A table carrying one global secondary index, so a create naming it has something to
    // collide with.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": "existing-index",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "gk", "AttributeType": "S"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
            "GlobalSecondaryIndexes": [{
                "IndexName": "by-gk",
                "KeySchema": [{"AttributeName": "gk", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    describe_when_active(&router, "the table with an index", "existing-index").await;

    // The sentence interpolates nothing, so a create that differs in its key schema, its
    // projection or its capacity earns exactly the same text as one that repeats the
    // index's own declaration.
    let rows = [
        (
            "the same declaration",
            json!([{"AttributeName": "gk", "AttributeType": "S"}]),
            json!({"Create": {
                "IndexName": "by-gk",
                "KeySchema": [{"AttributeName": "gk", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }}),
        ),
        (
            "a different key schema and projection",
            json!([{"AttributeName": "hk", "AttributeType": "S"}]),
            json!({"Create": {
                "IndexName": "by-gk",
                "KeySchema": [{"AttributeName": "hk", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }}),
        ),
    ];
    for (case, definitions, entry) in rows {
        let (code, stated) = refused(
            &router,
            case,
            "UpdateTable",
            json!({
                "TableName": "existing-index",
                "AttributeDefinitions": definitions,
                "GlobalSecondaryIndexUpdates": [entry],
            }),
        )
        .await;
        assert_eq!(
            (code.as_str(), stated.as_str()),
            (
                "ValidationException",
                "Attempting to create an index which already exists"
            ),
            "case {case:?}: expected the fixed sentence for that work, got {code} with {stated:?}"
        );
    }
}

#[tokio::test]
async fn a_second_change_names_the_work_the_table_is_still_doing() {
    let router = emulator();
    let name = "in-flight";
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 5, "WriteCapacityUnits": 5},
        }),
    )
    .await;
    describe_when_active(&router, "the provisioned table", name).await;

    // A CAPACITY MOVE in flight: whatever the second call asks for, the refusal names the
    // work still running — the table's IOPS — and the table it is running on.
    let iops = format!(
        "Attempt to change a resource which is still in use: Table IOPS are currently being \
         updated. Table: {name}"
    );
    let plain =
        format!("Attempt to change a resource which is still in use: Table is being updated: {name}");

    for (case, first, second) in [
        (
            "a provisioned-throughput move",
            json!({"TableName": name,
                   "ProvisionedThroughput": {"ReadCapacityUnits": 10, "WriteCapacityUnits": 10}}),
            json!({"TableName": name,
                   "ProvisionedThroughput": {"ReadCapacityUnits": 11, "WriteCapacityUnits": 11}}),
        ),
        (
            "a capacity-mode switch",
            json!({"TableName": name, "BillingMode": "PAY_PER_REQUEST"}),
            json!({"TableName": name, "DeletionProtectionEnabled": true}),
        ),
    ] {
        ok(&router, "UpdateTable", first).await;
        let (code, stated) = refused(&router, case, "UpdateTable", second).await;
        assert_eq!(
            (code.as_str(), stated.as_str()),
            ("ResourceInUseException", iops.as_str()),
            "case {case:?}: expected the IOPS sentence while a capacity move runs, got {code} \
             with {stated:?}"
        );
        describe_when_active(&router, case, name).await;
    }

    // An unsettled update that moves NO capacity figure is the other sentence: the table
    // is being updated, and the table is what the refusal names.
    for (case, first) in [
        (
            "a table-class change",
            json!({"TableName": name, "TableClass": "STANDARD_INFREQUENT_ACCESS"}),
        ),
        (
            "a warm-throughput change",
            json!({"TableName": name,
                   "WarmThroughput": {"ReadUnitsPerSecond": 12000, "WriteUnitsPerSecond": 4000}}),
        ),
    ] {
        ok(&router, "UpdateTable", first).await;
        let (code, stated) = refused(
            &router,
            case,
            "UpdateTable",
            json!({"TableName": name, "DeletionProtectionEnabled": false}),
        )
        .await;
        assert_eq!(
            (code.as_str(), stated.as_str()),
            ("ResourceInUseException", plain.as_str()),
            "case {case:?}: expected the table-being-updated sentence, got {code} with {stated:?}"
        );
        describe_when_active(&router, case, name).await;
    }
}

/// Create an on-demand table keyed on `pk` and wait out the transition the clock owes it.
async fn create_active_table_named(router: &MultiServiceRouter, name: &str) {
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
    describe_when_active(router, name, name).await;
}
