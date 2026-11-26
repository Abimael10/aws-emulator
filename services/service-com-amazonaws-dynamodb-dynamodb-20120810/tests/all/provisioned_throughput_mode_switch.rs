// SPDX-License-Identifier: CC0-1.0

//! Which sentence refuses a table left in the `PROVISIONED` billing mode with no capacity to
//! run it on, over the two operations that can put it there — driven end to end over the
//! wire.
//!
//! The rule under test: a table in the provisioned mode has to carry a read and a write
//! capacity, and WHICH sentence a request that leaves it without one earns follows the
//! operation that would have supplied it. A CREATE has no stored pair to fall back on, so it
//! is told about the two CAPACITY members it needed ("If you set BillingMode as PROVISIONED,
//! you must specify this property", `CreateTable$ProvisionedThroughput`), while an UPDATE
//! names the MEMBER the request had to carry ("When switching from pay-per-request to
//! provisioned capacity, initial provisioned capacity values must be set",
//! `UpdateTable$BillingMode`). Either way the call is refused 400 `ValidationException` and
//! nothing is written: the table keeps the mode and the capacity it had.
//!
//! The rows differ along what the rule generalises over: the operation, which of the two
//! members states the mode (`BillingMode` and the `TableThroughputMode` alias both carry it),
//! whether the table the update addresses already holds a capacity pair, and whether the
//! request supplies one — so the accepted side of the same rule is asserted beside the
//! refused one.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// AWS's sentence for a CREATE that asks for the provisioned mode and states no capacity.
const CREATE_SENTENCE: &str =
    "One or more parameter values were invalid: ReadCapacityUnits and WriteCapacityUnits must \
     both be specified when BillingMode is PROVISIONED";

/// AWS's sentence for an UPDATE that moves a table into the provisioned mode and states no
/// capacity: it names the member the request had to carry.
const UPDATE_SENTENCE: &str =
    "One or more parameter values were invalid: ProvisionedThroughput must be specified when \
     BillingMode is PROVISIONED";

/// A capacity pair a request can carry.
fn capacity(read: i64, write: i64) -> Value {
    json!({"ReadCapacityUnits": read, "WriteCapacityUnits": write})
}

/// A `CreateTable` body: one `pk` partition key, in `mode`, with `extra` merged over it.
fn create_body(name: &str, mode_member: &str, mode: &str, extra: Value) -> Value {
    let mut body = json!({
        "TableName": name,
        mode_member: mode,
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
    });
    if let Some(members) = extra.as_object() {
        for (member, held) in members {
            body[member] = held.clone();
        }
    }
    body
}

/// Create one on-demand table and wait for it, so an update can address it.
async fn on_demand_table(router: &smithy_server_runtime::MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateTable",
        create_body(name, "BillingMode", "PAY_PER_REQUEST", json!({})),
    )
    .await;
    describe_when_active(router, "the table an update addresses", name).await;
}

/// The two members of this API that state a table's billing mode: `BillingMode` and the
/// `TableThroughputMode` alias beside it
/// (`smithy_query(query="operation", operation="CreateTable,UpdateTable")`).
const MODE_MEMBERS: [&str; 2] = ["BillingMode", "TableThroughputMode"];

#[tokio::test]
async fn a_provisioned_declaration_with_no_capacity_is_refused_in_its_operations_words() {
    let router = emulator();

    // The CREATE half: whichever member states the mode, the create is told about the two
    // capacity members, and a create that states them is served.
    for (index, member) in MODE_MEMBERS.iter().enumerate() {
        let case = format!("a provisioned create stating the mode in {member}");
        let name = format!("ProvisionedCreate{index}");
        let (status, refused) = call(
            &router,
            "CreateTable",
            create_body(&name, member, "PROVISIONED", json!({})),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} with {refused}"
        );
        let stated = message(&refused);
        assert_eq!(
            stated, CREATE_SENTENCE,
            "case {case:?}: expected {CREATE_SENTENCE:?}, got {stated:?}"
        );
        // Nothing was created, which the read is what proves.
        let (read_status, read) = call(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            (read_status, error_code(&read).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: a refused create must leave no table behind, got {read_status} \
             with {read}"
        );

        // The accepted side: the same declaration with the pair stated is served, and the
        // stored table reports it.
        let served = format!("Provisioned{index}");
        let created = ok(
            &router,
            "CreateTable",
            create_body(
                &served,
                member,
                "PROVISIONED",
                json!({"ProvisionedThroughput": capacity(3, 4)}),
            ),
        )
        .await;
        assert_eq!(
            created["TableDescription"]["ProvisionedThroughput"]["ReadCapacityUnits"],
            json!(3),
            "case {case:?}: expected the capacity the create stated, got {created}"
        );
    }

    // The UPDATE half: a table with no stored pair, moved into the provisioned mode with no
    // capacity, is told about the MEMBER it had to carry — whichever member states the mode.
    for (index, member) in MODE_MEMBERS.iter().enumerate() {
        let case = format!("a provisioned update stating the mode in {member}");
        let name = format!("OnDemand{index}");
        on_demand_table(&router, &name).await;
        let (status, refused) = call(
            &router,
            "UpdateTable",
            json!({"TableName": name.clone(), member.to_string(): "PROVISIONED"}),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} with {refused}"
        );
        let stated = message(&refused);
        assert_eq!(
            stated, UPDATE_SENTENCE,
            "case {case:?}: expected {UPDATE_SENTENCE:?}, got {stated:?}"
        );
        assert_ne!(
            stated, CREATE_SENTENCE,
            "case {case:?}: the update states its own sentence, not the create's"
        );
        // The refused update left the table's mode alone.
        let unchanged = ok(&router, "DescribeTable", json!({"TableName": name.clone()})).await;
        assert_eq!(
            unchanged["Table"]["BillingModeSummary"]["BillingMode"],
            json!("PAY_PER_REQUEST"),
            "case {case:?}: a refused update must leave the mode alone, got {unchanged}"
        );

        // The accepted side: the same update carrying the pair moves the table over.
        let updated = ok(
            &router,
            "UpdateTable",
            json!({
                "TableName": name.clone(),
                member.to_string(): "PROVISIONED",
                "ProvisionedThroughput": capacity(5, 6),
            }),
        )
        .await;
        assert_eq!(
            updated["TableDescription"]["ProvisionedThroughput"]["WriteCapacityUnits"],
            json!(6),
            "case {case:?}: expected the capacity the update stated, got {updated}"
        );
    }
}

/// A table that is ALREADY provisioned carries a pair of its own, so an update that re-states
/// the mode and no capacity is not missing anything: it keeps the table's pair and is served.
#[tokio::test]
async fn an_update_of_an_already_provisioned_table_keeps_its_capacity() {
    let router = emulator();
    let name = "AlreadyProvisioned";
    ok(
        &router,
        "CreateTable",
        create_body(
            name,
            "BillingMode",
            "PROVISIONED",
            json!({"ProvisionedThroughput": capacity(7, 8)}),
        ),
    )
    .await;
    describe_when_active(&router, "the provisioned table", name).await;

    let case = "an update re-stating the mode with no capacity";
    let updated = ok(
        &router,
        "UpdateTable",
        json!({"TableName": name, "BillingMode": "PROVISIONED"}),
    )
    .await;
    let reported = &updated["TableDescription"]["ProvisionedThroughput"];
    assert_eq!(
        (
            reported["ReadCapacityUnits"].clone(),
            reported["WriteCapacityUnits"].clone()
        ),
        (json!(7), json!(8)),
        "case {case:?}: expected the table's own pair to be kept, got {updated}"
    );
}
