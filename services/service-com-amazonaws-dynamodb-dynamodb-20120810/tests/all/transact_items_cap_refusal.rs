// SPDX-License-Identifier: CC0-1.0

//! What a TRANSACTION whose `TransactItems` list is past the 100-action cap is refused
//! with, driven end to end.
//!
//! RULE under test: a list member carrying more elements than the `@length` maximum the
//! model declares for it is refused 400 `ValidationException` whose message is the
//! framework's constraint envelope over ONE clause that states exactly two things — the
//! member path, as the operation that was served names it, and the bound that was broken —
//! and echoes nothing of the rejected list. Both transaction operations declare that bound
//! on the same member (`smithy_query(query="operation",
//! operation="TransactGetItems,TransactWriteItems")`: `TransactItems` is `@required`, and
//! `smithy_query(query="shape", shape="com.amazonaws.dynamodb#TransactGetItemList")` and
//! `TransactWriteItemList` are both `@length{min:1,max:100}`), so both state the same
//! sentence, and neither writes anything.
//!
//! The rows differ along what the rule generalises over: the operation (the transactional
//! read and the transactional write), the ACTION its elements state (`Get`, `Put`,
//! `Delete`, `Update`, `ConditionCheck`, and a list mixing two of them), how far past the
//! bound the list went (one past it, half again as many, and twice the bound), how many
//! tables the actions name, and what the items carry (a bare key, and attributes of
//! several datatypes) — because none of that belongs in the message. The bound itself is
//! driven AT the bound, where the call is served, and one past it.
//!
//! Every call goes over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<Operation>`) through the same `MultiServiceRouter` the
//! emulator's front doors use.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The tables the rows address, each keyed on one string attribute.
const TABLE: &str = "TransactCap";

/// The second table, so a list naming two tables is refused the same way.
const OTHER: &str = "TransactCapOther";

/// That key attribute.
const KEY: &str = "pk";

/// The cap both transactional lists declare.
const CAP: usize = 100;

/// The whole message a `TransactItems` list past that cap earns.
fn expected_message() -> String {
    format!(
        "1 validation error detected: Value at 'TransactItems' failed to satisfy constraint: \
         Member must have length less than or equal to {CAP}"
    )
}

/// The key of element `index`, as a `Key` map.
fn key(index: usize) -> Value {
    json!({KEY: {"S": format!("k{index}")}})
}

/// The table element `index` names: every third one addresses the second table, so a list
/// spanning two tables is refused by the same clause.
fn table_of(index: usize) -> &'static str {
    if index.is_multiple_of(3) {
        OTHER
    } else {
        TABLE
    }
}

/// An item carrying the key of `index` and attributes of several datatypes, so what the
/// elements carry cannot reach the message.
fn item(index: usize) -> Value {
    json!({
        KEY: {"S": format!("k{index}")},
        "note": {"S": format!("note {index}")},
        "tally": {"N": index.to_string()},
        "flag": {"BOOL": index.is_multiple_of(2)},
        "tags": {"SS": ["a", "b"]},
        "doc": {"M": {"leaf": {"S": "deep"}}},
    })
}

/// The action kinds a row's elements state, each as it builds element `index`.
#[derive(Clone, Copy)]
enum Action {
    Get,
    Put,
    Delete,
    Update,
    ConditionCheck,
    /// A `Put` at an even position and a `Delete` at an odd one.
    PutAndDelete,
}

impl Action {
    /// Element `index` of a list of this kind.
    fn element(self, index: usize) -> Value {
        let table = table_of(index);
        match self {
            Self::Get => json!({"Get": {"TableName": table, "Key": key(index)}}),
            Self::Put => json!({"Put": {"TableName": table, "Item": item(index)}}),
            Self::Delete => json!({"Delete": {"TableName": table, "Key": key(index)}}),
            Self::Update => json!({"Update": {
                "TableName": table,
                "Key": key(index),
                "UpdateExpression": "SET tally = :n",
                "ExpressionAttributeValues": {":n": {"N": index.to_string()}},
            }}),
            Self::ConditionCheck => json!({"ConditionCheck": {
                "TableName": table,
                "Key": key(index),
                "ConditionExpression": "attribute_not_exists(note)",
            }}),
            Self::PutAndDelete => {
                if index.is_multiple_of(2) {
                    Self::Put.element(index)
                } else {
                    Self::Delete.element(index)
                }
            }
        }
    }

    /// `count` elements of this kind.
    fn list(self, count: usize) -> Value {
        Value::Array((0..count).map(|index| self.element(index)).collect())
    }
}

/// The operation a list of actions of this kind belongs to.
fn operation_for(action: Action) -> &'static str {
    match action {
        Action::Get => "TransactGetItems",
        _ => "TransactWriteItems",
    }
}

/// The rows: the case, the action its elements state, and the element count.
fn rows() -> Vec<(String, Action, usize)> {
    let actions: [(&str, Action); 6] = [
        ("gets", Action::Get),
        ("puts", Action::Put),
        ("deletes", Action::Delete),
        ("updates", Action::Update),
        ("condition checks", Action::ConditionCheck),
        ("puts and deletes", Action::PutAndDelete),
    ];
    let counts = [CAP + 1, CAP + CAP / 2, CAP * 2];
    actions
        .into_iter()
        .flat_map(|(named, action)| {
            counts
                .into_iter()
                .map(move |count| (format!("{count} {named}"), action, count))
        })
        .collect()
}

#[tokio::test]
async fn a_transaction_past_the_action_cap_is_refused_with_the_member_and_the_bound() {
    let router = emulator();
    for table in [TABLE, OTHER] {
        create_active_table(&router, table, &[(KEY, "HASH", "S")]).await;
    }

    let expected = expected_message();
    for (case, action, count) in rows() {
        let operation = operation_for(action);
        let (status, answered) = call(
            &router,
            operation,
            json!({"TransactItems": action.list(count)}),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?} ({operation}): expected 400 ValidationException, got {status} with \
             {answered}"
        );
        assert_eq!(
            message(&answered),
            expected,
            "case {case:?} ({operation}): expected the member and the bound alone, got {:?}",
            message(&answered)
        );
    }
}

#[tokio::test]
async fn the_refusal_states_nothing_of_the_list_it_rejected() {
    let router = emulator();
    for table in [TABLE, OTHER] {
        create_active_table(&router, table, &[(KEY, "HASH", "S")]).await;
    }

    // Nothing a caller sent, and nothing the emulator computed about the request, may reach
    // the message: not a key, an attribute name, a table name or an expression the list
    // carried, not the count of elements, and not the object rendering of a request
    // structure.
    let forbidden = [
        "k0", "note", "tally", "TransactCap", "AttributeValue", "TransactGetItem",
        "TransactWriteItem", "workloadProfileName", "101 items", "attribute_not_exists",
    ];
    for (case, action, count) in rows() {
        let operation = operation_for(action);
        let (_, answered) = call(
            &router,
            operation,
            json!({"TransactItems": action.list(count)}),
        )
        .await;
        let stated = message(&answered);
        for leaked in forbidden {
            assert!(
                !stated.contains(leaked),
                "case {case:?} ({operation}): the clause must not state {leaked:?}, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_refused_transaction_writes_nothing_and_the_list_at_the_cap_is_applied() {
    let router = emulator();
    for table in [TABLE, OTHER] {
        create_active_table(&router, table, &[(KEY, "HASH", "S")]).await;
    }

    // One past the cap: refused, and no item of either table was written.
    let (status, answered) = call(
        &router,
        "TransactWriteItems",
        json!({"TransactItems": Action::Put.list(CAP + 1)}),
    )
    .await;
    assert_eq!(
        status, 400,
        "a list past the cap is refused, got {status} with {answered}"
    );
    for table in [TABLE, OTHER] {
        let scanned = ok(
            &router,
            "Scan",
            json!({"TableName": table, "ConsistentRead": true}),
        )
        .await;
        assert_eq!(
            scanned["Count"], json!(0),
            "the refused transaction must write nothing to {table}, got {scanned}"
        );
    }

    // AT the cap the transaction is served and applied in full, which is what makes the
    // rows above about the bound and not an off-by-one.
    ok(
        &router,
        "TransactWriteItems",
        json!({"TransactItems": Action::Put.list(CAP)}),
    )
    .await;
    let mut written = 0_i64;
    for table in [TABLE, OTHER] {
        let scanned = ok(
            &router,
            "Scan",
            json!({"TableName": table, "ConsistentRead": true}),
        )
        .await;
        written += scanned["Count"].as_i64().unwrap_or_default();
    }
    assert_eq!(
        written,
        i64::try_from(CAP).unwrap_or_default(),
        "a transaction at the cap must apply every one of its actions, got {written} items"
    );

    // And the transactional READ at the cap is served too, over the items just written.
    let read = ok(
        &router,
        "TransactGetItems",
        json!({"TransactItems": Action::Get.list(CAP)}),
    )
    .await;
    assert_eq!(
        read["Responses"].as_array().map(Vec::len),
        Some(CAP),
        "a read transaction at the cap reports one ItemResponse per element, got {read}"
    );
}
