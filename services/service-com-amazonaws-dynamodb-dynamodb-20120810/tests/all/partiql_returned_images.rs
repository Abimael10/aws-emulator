// SPDX-License-Identifier: CC0-1.0

//! What a PartiQL write addressed BY ITS KEY answers, and how a value refused on the
//! member that carries a table's name or ARN is worded.
//!
//! THE RULE a single-item delete follows: a `DELETE` addresses exactly ONE item by its
//! complete primary key — the equalities its `WHERE` clause states on the table's key
//! attributes say which item the statement is about, not what must hold of it — and what
//! the clause states BESIDES them is a condition on THAT item. A key that holds no item
//! therefore decides the statement by itself, whatever else the clause states: there is
//! nothing to remove and nothing a condition can have failed, so the statement is served
//! having removed nothing and reports no image. A delete addressed by key is idempotent
//! ("Unless you specify conditions, the DeleteItem is an idempotent operation; running it
//! multiple times on the same item or attribute does not result in an error response" —
//! the `DeleteItem` doc), and `ConditionalCheckFailedException` is only what an item that
//! IS stored and fails the condition earns. The rows span what the rule generalises
//! over: which surface runs the statement (`ExecuteStatement`, `BatchExecuteStatement`,
//! `ExecuteTransaction` — which reports no per-statement image at all, so a `RETURNING`
//! clause is refused there), whether the addressed key holds an item, whether the clause
//! states a condition besides the key, which FORM that condition takes and whether the
//! stored item holds it, which `RETURNING` form the statement asks for, and how many key
//! attributes the table has.
//!
//! THE RULE a refused table reference follows: `TableName` targets `TableArn`, so an ARN
//! is accepted wherever a name is, and a value neither form admits is refused as a
//! constraint violation on that member — the member, the value the request sent, and the
//! requirement it broke. Which requirement depends on what is wrong with the value: a
//! string spelled as an ARN that is not a well-formed one breaks the ARN form (which
//! states the ARN's fields and the resource name inside it), and an ARN of this service
//! that names a resource of another kind breaks the resource type.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok, REGION};

/// The two-key table the delete rows address: a composite primary key, so a statement
/// has to pin both attributes to name one item.
const PAIRED: &str = "ReturnedImagesPaired";

/// The same rule over a table whose primary key is a partition key ALONE, which is the
/// other key schema a statement can address.
const SOLO: &str = "ReturnedImagesSolo";

/// The account every ARN in this suite names: the one this emulator serves.
const ACCOUNT: &str = "000000000000";

/// Create both tables and wait out the documented `CREATING` -> `ACTIVE` transition.
async fn tables() -> MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": PAIRED,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": SOLO,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "the paired table", PAIRED).await;
    describe_when_active(&router, "the solo table", SOLO).await;
    router
}

/// Store the subject item of both tables: `(pk, sk) = (p1, s1)` with the attributes the
/// conditions below speak about.
async fn seed(router: &MultiServiceRouter) {
    ok(
        router,
        "PutItem",
        json!({
            "TableName": PAIRED,
            "Item": {
                "pk": {"S": "p1"},
                "sk": {"S": "s1"},
                "qty": {"N": "4"},
                "note": {"S": "n1"},
            },
        }),
    )
    .await;
    ok(
        router,
        "PutItem",
        json!({"TableName": SOLO, "Item": {"pk": {"S": "p1"}, "qty": {"N": "4"}}}),
    )
    .await;
}

/// One delete row: the statement, and what the surface running it must answer — either
/// the images it reports, or the refusal it earns.
struct DeleteRow {
    case: &'static str,
    statement: String,
    /// The items the statement reports: the image of what it removed, or nothing.
    images: Vec<Value>,
    /// `Some(code)` when the statement is refused instead of served.
    refused: Option<&'static str>,
}

/// The rows differ in whether the addressed key holds an item, in whether the clause
/// states a condition BESIDES the key and whether the stored item satisfies it, in which
/// `RETURNING` form is asked for, and in how many key attributes the table has.
fn delete_table() -> Vec<DeleteRow> {
    vec![
        DeleteRow {
            case: "the key holds the item, ALL OLD",
            statement: format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 's1' RETURNING ALL OLD *"
            ),
            images: vec![json!({
                "pk": {"S": "p1"},
                "sk": {"S": "s1"},
                "qty": {"N": "4"},
                "note": {"S": "n1"},
            })],
            refused: None,
        },
        DeleteRow {
            // "Only RETURNING ALL OLD * is allowed in DELETE statements": the clause
            // that asks for what an UPDATE modified is refused, and the item stays.
            case: "the key holds the item, MODIFIED OLD",
            statement: format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 's1' RETURNING MODIFIED \
                 OLD *"
            ),
            images: Vec::new(),
            refused: Some("ValidationException"),
        },
        DeleteRow {
            case: "the key holds the item and no RETURNING clause is stated",
            statement: format!("DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 's1'"),
            images: Vec::new(),
            refused: None,
        },
        DeleteRow {
            case: "the key holds no item, ALL OLD",
            statement: format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 's1' RETURNING ALL \
                 OLD *"
            ),
            images: Vec::new(),
            refused: None,
        },
        DeleteRow {
            case: "the sort key names no item of a partition that holds one",
            statement: format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 'elsewhere' RETURNING ALL \
                 OLD *"
            ),
            images: Vec::new(),
            refused: None,
        },
        DeleteRow {
            case: "the key holds no item, no RETURNING clause",
            statement: format!("DELETE FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 's1'"),
            images: Vec::new(),
            refused: None,
        },
        DeleteRow {
            case: "a condition beside the key that the stored item satisfies",
            statement: format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 's1' AND qty = 4 \
                 RETURNING ALL OLD *"
            ),
            images: vec![json!({
                "pk": {"S": "p1"},
                "sk": {"S": "s1"},
                "qty": {"N": "4"},
                "note": {"S": "n1"},
            })],
            refused: None,
        },
        DeleteRow {
            case: "a condition beside the key that the stored item fails",
            statement: format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 's1' AND qty = 99 \
                 RETURNING ALL OLD *"
            ),
            images: Vec::new(),
            refused: Some("ConditionalCheckFailedException"),
        },
        DeleteRow {
            case: "a condition on an attribute the stored item does not carry",
            statement: format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 's1' AND unseen = 'x'"
            ),
            images: Vec::new(),
            refused: Some("ConditionalCheckFailedException"),
        },
        DeleteRow {
            // The key decides the statement: a condition no item can satisfy still has
            // no item to be held to, so the delete is served having removed nothing.
            case: "a condition no item can satisfy, over a key that holds nothing",
            statement: format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 's1' AND qty = 99"
            ),
            images: Vec::new(),
            refused: None,
        },
        DeleteRow {
            case: "a condition the STORED item of another key satisfies, over a key that \
                   holds nothing, RETURNING ALL OLD",
            statement: format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 'elsewhere' AND \
                 note = 'n1' RETURNING ALL OLD *"
            ),
            images: Vec::new(),
            refused: None,
        },
        DeleteRow {
            case: "two conditions beside a key that holds nothing",
            statement: format!(
                "DELETE FROM \"{SOLO}\" WHERE pk = 'ghost' AND qty = 4 AND qty < 9 \
                 RETURNING ALL OLD *"
            ),
            images: Vec::new(),
            refused: None,
        },
        DeleteRow {
            case: "a condition an absent item satisfies, over a key that holds nothing",
            statement: format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 's1' AND qty IS MISSING"
            ),
            images: Vec::new(),
            refused: None,
        },
        DeleteRow {
            case: "a partition-key-only table, the key holds the item",
            statement: format!(
                "DELETE FROM \"{SOLO}\" WHERE pk = 'p1' RETURNING ALL OLD *"
            ),
            images: vec![json!({"pk": {"S": "p1"}, "qty": {"N": "4"}})],
            refused: None,
        },
        DeleteRow {
            case: "a partition-key-only table, the key holds no item",
            statement: format!(
                "DELETE FROM \"{SOLO}\" WHERE pk = 'ghost' RETURNING ALL OLD *"
            ),
            images: Vec::new(),
            refused: None,
        },
        DeleteRow {
            case: "a partition-key-only table with a condition the item fails",
            statement: format!("DELETE FROM \"{SOLO}\" WHERE pk = 'p1' AND qty = 99"),
            images: Vec::new(),
            refused: Some("ConditionalCheckFailedException"),
        },
    ]
}

#[tokio::test]
async fn a_statement_delete_addressed_by_key_is_idempotent() {
    let router = tables().await;
    for row in delete_table() {
        // Every row starts from the same stored state, so what it asserts is its own
        // clause and nothing a previous row left behind.
        seed(&router).await;
        let (status, body) = call(
            &router,
            "ExecuteStatement",
            json!({"Statement": row.statement}),
        )
        .await;
        match row.refused {
            None => {
                assert_eq!(
                    status, 200,
                    "case {}: expected the statement served, got {status} with {body}",
                    row.case
                );
                assert_eq!(
                    body["Items"],
                    Value::Array(row.images.clone()),
                    "case {}: expected the images {:?}, got {body}",
                    row.case,
                    row.images
                );
            }
            Some(code) => {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, code),
                    "case {}: expected 400 {code}, got {status} with {body}",
                    row.case
                );
            }
        }
    }
}

/// The seeded item of the two-key table, as a read reports it: what a delete that
/// removed it reports, and what a delete that removed nothing leaves behind.
fn stored() -> Value {
    json!({
        "pk": {"S": "p1"},
        "sk": {"S": "s1"},
        "qty": {"N": "4"},
        "note": {"S": "n1"},
    })
}

/// One condition FORM a delete can state beside the key it addresses, and whether the
/// stored item satisfies it. The forms are the ones the `WHERE` grammar states: a
/// comparison, a function call, a membership list, a presence test, a disjunction and a
/// conjunction of several terms.
struct GuardRow {
    case: &'static str,
    /// The condition, stated beside the key equalities that address the item.
    condition: &'static str,
    /// Whether the seeded `(p1, s1)` item satisfies it.
    holds: bool,
}

/// The rows span the condition forms and, for each, both answers the ADDRESSED item can
/// give: one the stored item satisfies and one it fails.
fn guard_table() -> Vec<GuardRow> {
    vec![
        GuardRow {
            case: "an equality on a string attribute the item satisfies",
            condition: "note = 'n1'",
            holds: true,
        },
        GuardRow {
            case: "an equality on a string attribute the item fails",
            condition: "note = 'elsewhere'",
            holds: false,
        },
        GuardRow {
            case: "a numeric comparison the item satisfies",
            condition: "qty > 1",
            holds: true,
        },
        GuardRow {
            case: "a numeric comparison the item fails",
            condition: "qty > 9",
            holds: false,
        },
        GuardRow {
            case: "a function call the item satisfies",
            condition: "begins_with(note, 'n')",
            holds: true,
        },
        GuardRow {
            case: "a function call the item fails",
            condition: "begins_with(note, 'z')",
            holds: false,
        },
        GuardRow {
            case: "a membership list the item satisfies",
            condition: "qty IN [4, 5]",
            holds: true,
        },
        GuardRow {
            case: "a membership list the item fails",
            condition: "qty IN [5, 6]",
            holds: false,
        },
        GuardRow {
            case: "a presence test the item satisfies",
            condition: "note IS NOT MISSING",
            holds: true,
        },
        GuardRow {
            case: "a presence test the item fails",
            condition: "unseen IS NOT MISSING",
            holds: false,
        },
        GuardRow {
            case: "a disjunction one branch of which the item satisfies",
            condition: "(qty = 99 OR note = 'n1')",
            holds: true,
        },
        GuardRow {
            case: "a disjunction no branch of which the item satisfies",
            condition: "(qty = 99 OR note = 'elsewhere')",
            holds: false,
        },
        GuardRow {
            case: "several terms the item satisfies",
            condition: "qty = 4 AND note = 'n1'",
            holds: true,
        },
        GuardRow {
            case: "several terms one of which the item fails",
            condition: "qty = 4 AND note = 'elsewhere'",
            holds: false,
        },
    ]
}

#[tokio::test]
async fn a_guarded_delete_of_a_key_that_holds_no_item_is_served_having_removed_nothing() {
    let router = tables().await;
    for row in guard_table() {
        // Every row starts from the same stored state, so what it asserts is its own
        // clause and nothing a previous row left behind.
        seed(&router).await;
        let (status, body) = call(
            &router,
            "ExecuteStatement",
            json!({
                "Statement": format!(
                    "DELETE FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 's1' AND {} \
                     RETURNING ALL OLD *",
                    row.condition,
                ),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {}, over a key that holds no item: expected the statement served, got \
             {status} with {body}",
            row.case
        );
        assert_eq!(
            body["Items"],
            json!([]),
            "case {}, over a key that holds no item: expected no image, got {body}",
            row.case
        );
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": PAIRED, "Key": {"pk": {"S": "p1"}, "sk": {"S": "s1"}}}),
        )
        .await;
        assert_eq!(
            read["Item"],
            stored(),
            "case {}, over a key that holds no item: the stored item is untouched, got \
             {read}",
            row.case
        );

        // The asymmetry the rule states: the SAME condition over the key that DOES hold
        // an item is held to that item.
        let (status, body) = call(
            &router,
            "ExecuteStatement",
            json!({
                "Statement": format!(
                    "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 's1' AND {} \
                     RETURNING ALL OLD *",
                    row.condition,
                ),
            }),
        )
        .await;
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": PAIRED, "Key": {"pk": {"S": "p1"}, "sk": {"S": "s1"}}}),
        )
        .await;
        if row.holds {
            assert_eq!(
                status, 200,
                "case {}, over the key that holds the item: expected the statement served, \
                 got {status} with {body}",
                row.case
            );
            assert_eq!(
                body["Items"],
                json!([stored()]),
                "case {}, over the key that holds the item: expected its image, got {body}",
                row.case
            );
            assert!(
                read.get("Item").is_none(),
                "case {}, over the key that holds the item: it is removed, got {read}",
                row.case
            );
        } else {
            assert_eq!(
                (status, error_code(&body).as_str(), message(&body).as_str()),
                (
                    400,
                    "ConditionalCheckFailedException",
                    "The conditional request failed"
                ),
                "case {}, over the key that holds the item: expected the conditional \
                 refusal, got {status} with {body}",
                row.case
            );
            assert_eq!(
                read["Item"],
                stored(),
                "case {}, over the key that holds the item: the refused statement wrote \
                 nothing, got {read}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn a_guarded_delete_of_an_absent_key_is_served_on_every_statement_surface() {
    let router = tables().await;
    seed(&router).await;
    let guarded = format!(
        "DELETE FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 's1' AND note = 'n1' \
         RETURNING ALL OLD *"
    );
    let batched = ok(
        &router,
        "BatchExecuteStatement",
        json!({"Statements": [{"Statement": guarded.clone()}]}),
    )
    .await;
    let slot = batched["Responses"][0].clone();
    assert_eq!(
        slot["Error"]["Code"].as_str(),
        None,
        "case a batched guarded delete of an absent key: expected the slot served, got \
         {slot}"
    );
    assert_eq!(
        slot.get("Item").cloned().unwrap_or(Value::Null),
        Value::Null,
        "case a batched guarded delete of an absent key: expected no image, got {slot}"
    );

    // A transaction reports no per-statement image at all, so the same statement asks
    // there for something that surface does not serve and is refused behind its slot;
    // the statement WITHOUT the clause is served, having removed nothing.
    let (status, body) = call(
        &router,
        "ExecuteTransaction",
        json!({"TransactStatements": [{"Statement": guarded}]}),
    )
    .await;
    assert_eq!(
        (status, message(&body).as_str()),
        (
            400,
            "Validation failed in TransactStatements[0]: RETURNING clause is not supported in \
             ExecuteTransaction."
        ),
        "case a transactional guarded delete asking for an image: expected the clause \
         refused, got {status} with {body}"
    );
    let (status, body) = call(
        &router,
        "ExecuteTransaction",
        json!({"TransactStatements": [{"Statement": format!(
            "DELETE FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 's1' AND note = 'n1'"
        )}]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a transactional guarded delete of an absent key: expected the transaction \
         applied, got {status} with {body}"
    );

    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": PAIRED, "Key": {"pk": {"S": "p1"}, "sk": {"S": "s1"}}}),
    )
    .await;
    assert_eq!(
        read["Item"],
        stored(),
        "case a guarded delete of an absent key on every surface: the stored item is \
         untouched, got {read}"
    );
}

#[tokio::test]
async fn what_a_served_delete_removed_is_what_a_later_read_reports() {
    let router = tables().await;
    seed(&router).await;
    // The delete of a key that holds no item leaves the stored item alone: nothing is
    // removed, so the item the SAME partition holds is still there afterwards.
    let answered = ok(
        &router,
        "ExecuteStatement",
        json!({
            "Statement": format!(
                "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 'elsewhere' RETURNING ALL \
                 OLD *"
            ),
        }),
    )
    .await;
    assert_eq!(
        answered["Items"],
        json!([]),
        "case a delete that removed nothing: no image is reported, got {answered}"
    );
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": PAIRED, "Key": {"pk": {"S": "p1"}, "sk": {"S": "s1"}}}),
    )
    .await;
    assert_eq!(
        read["Item"],
        json!({
            "pk": {"S": "p1"},
            "sk": {"S": "s1"},
            "qty": {"N": "4"},
            "note": {"S": "n1"},
        }),
        "case a delete that removed nothing: the stored item is untouched, got {read}"
    );

    // The delete of the key that DOES hold it removes it, and running the same statement
    // again is served exactly the same way — with no image, because there is nothing
    // left to report.
    for (case, images) in [
        ("the first delete", json!([{
            "pk": {"S": "p1"},
            "sk": {"S": "s1"},
            "qty": {"N": "4"},
            "note": {"S": "n1"},
        }])),
        ("the same delete again", json!([])),
    ] {
        let answered = ok(
            &router,
            "ExecuteStatement",
            json!({
                "Statement": format!(
                    "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 's1' RETURNING ALL \
                     OLD *"
                ),
            }),
        )
        .await;
        assert_eq!(
            answered["Items"], images,
            "case {case}: expected the images {images}, got {answered}"
        );
    }
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": PAIRED, "Key": {"pk": {"S": "p1"}, "sk": {"S": "s1"}}}),
    )
    .await;
    assert!(
        read.get("Item").is_none(),
        "case the same delete again: the item is gone, got {read}"
    );
}

#[tokio::test]
async fn a_batched_delete_of_a_key_that_holds_no_item_is_served_in_its_own_slot() {
    let router = tables().await;
    seed(&router).await;
    let answered = ok(
        &router,
        "BatchExecuteStatement",
        json!({
            "Statements": [
                {"Statement": format!(
                    "DELETE FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 's1' RETURNING ALL \
                     OLD *"
                )},
                {"Statement": format!(
                    "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 's1' AND qty = 99"
                )},
                {"Statement": format!(
                    "DELETE FROM \"{SOLO}\" WHERE pk = 'p1' RETURNING ALL OLD *"
                )},
            ],
        }),
    )
    .await;
    let slots = answered["Responses"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    // (the slot, the error code it must carry, the item it must report)
    let expected: [(&str, Option<&str>, Value); 3] = [
        ("a key that holds no item", None, Value::Null),
        (
            "a condition the stored item fails",
            Some("ConditionalCheckFailed"),
            Value::Null,
        ),
        (
            "a key that holds the item",
            None,
            json!({"pk": {"S": "p1"}, "qty": {"N": "4"}}),
        ),
    ];
    for (at, (case, code, item)) in expected.into_iter().enumerate() {
        let slot = slots.get(at).cloned().unwrap_or(Value::Null);
        assert_eq!(
            slot["Error"]["Code"].as_str(),
            code,
            "case {case}: expected Error.Code {code:?}, got {slot}"
        );
        assert_eq!(
            slot.get("Item").cloned().unwrap_or(Value::Null),
            item,
            "case {case}: expected the item {item}, got {slot}"
        );
    }
}

#[tokio::test]
async fn a_transactional_delete_of_a_key_that_holds_no_item_does_not_cancel() {
    let router = tables().await;
    seed(&router).await;
    // One transaction, two deletes: one addresses a key that holds no item and the other
    // the item that is there. Neither states a condition, so nothing cancels and the
    // stored item is gone afterwards.
    let (status, body) = call(
        &router,
        "ExecuteTransaction",
        json!({
            "TransactStatements": [
                {"Statement": format!(
                    "DELETE FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 's1'"
                )},
                {"Statement": format!("DELETE FROM \"{SOLO}\" WHERE pk = 'p1'")},
            ],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a transactional delete of an absent key: expected the transaction applied, \
         got {status} with {body}"
    );
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": SOLO, "Key": {"pk": {"S": "p1"}}}),
    )
    .await;
    assert!(
        read.get("Item").is_none(),
        "case a transactional delete of an absent key: the statement beside it still \
         applied, got {read}"
    );

    // The same transaction with a CONDITION the stored item fails is cancelled, and
    // nothing of it applies.
    seed(&router).await;
    let (status, body) = call(
        &router,
        "ExecuteTransaction",
        json!({
            "TransactStatements": [
                {"Statement": format!(
                    "DELETE FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 's1' AND qty = 99"
                )},
                {"Statement": format!("DELETE FROM \"{SOLO}\" WHERE pk = 'p1'")},
            ],
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "TransactionCanceledException"),
        "case a transactional delete whose condition fails: expected the transaction \
         cancelled, got {status} with {body}"
    );
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": SOLO, "Key": {"pk": {"S": "p1"}}}),
    )
    .await;
    assert_eq!(
        read["Item"],
        json!({"pk": {"S": "p1"}, "qty": {"N": "4"}}),
        "case a transactional delete whose condition fails: the other statement wrote \
         nothing, got {read}"
    );
}

/// The requirement clause AWS states for a value on this member that is spelled as an
/// ARN and is not a well-formed one: the ARN's own fields AND the resource name inside
/// it, since an ARN is accepted wherever a table name is.
const ARN_FORM: &str =
    "Valid ARN format is 'arn:<awsPartition>:<vendor>:<region>:<subscriber>:resourceType/\
     resourceName', where Resource name must have length less than or equal to 255, \
     Resource name must have length greater than or equal to 3, Resource name must \
     satisfy regular expression pattern: [a-zA-Z0-9_.-]+";

/// The requirement clause an ARN of this service naming a resource of another kind
/// fails.
const RESOURCE_TYPE: &str = "Invalid resource type";

#[tokio::test]
async fn a_table_reference_neither_a_name_nor_an_arn_reports_the_constraint_it_broke() {
    let router = tables().await;
    // (case, the value sent, the requirement the clause must state)
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "an ARN with an empty vendor field",
            format!("arn:aws::{REGION}:{ACCOUNT}:table/{PAIRED}"),
            ARN_FORM,
        ),
        (
            "an ARN with an empty partition field",
            format!("arn::dynamodb:{REGION}:{ACCOUNT}:table/{PAIRED}"),
            ARN_FORM,
        ),
        (
            "an ARN with an empty region field",
            format!("arn:aws:dynamodb::{ACCOUNT}:table/{PAIRED}"),
            ARN_FORM,
        ),
        (
            "an ARN with an empty account field",
            format!("arn:aws:dynamodb:{REGION}::table/{PAIRED}"),
            ARN_FORM,
        ),
        (
            "an ARN with an empty resource field",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:"),
            ARN_FORM,
        ),
        (
            "an ARN with too few fields",
            format!("arn:aws:dynamodb:{REGION}:table/{PAIRED}"),
            ARN_FORM,
        ),
        (
            "an ARN of this service naming a backup",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:backup/{PAIRED}"),
            RESOURCE_TYPE,
        ),
        (
            // The resource NAME of a table's ARN is the table name, so a name spelled as
            // a PATH addresses something deeper than the table and breaks the form that
            // name must take — which the ARN-form clause states.
            "an ARN of this service naming a table's index",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{PAIRED}/index/by-note"),
            ARN_FORM,
        ),
        (
            "an ARN of this service with a mis-cased resource type",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:Table/{PAIRED}"),
            RESOURCE_TYPE,
        ),
        (
            "an ARN of this service with no resource type at all",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:{PAIRED}"),
            RESOURCE_TYPE,
        ),
    ];
    for (case, value, requirement) in rows {
        let (status, body) = call(
            &router,
            "GetItem",
            json!({"TableName": value, "Key": {"pk": {"S": "p1"}, "sk": {"S": "s1"}}}),
        )
        .await;
        let expected = format!(
            "1 validation error detected: Value '{value}' at 'tableName' failed to satisfy \
             constraint: {requirement}"
        );
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", expected.as_str()),
            "case {case:?}: expected the constraint clause {expected:?}, got {status} with \
             {body}"
        );
    }

    // The table's OWN ARN still addresses it, which is what makes the refusals above
    // about the VALUE rather than about an ARN in this member.
    seed(&router).await;
    let read = ok(
        &router,
        "GetItem",
        json!({
            "TableName": format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{PAIRED}"),
            "Key": {"pk": {"S": "p1"}, "sk": {"S": "s1"}},
        }),
    )
    .await;
    assert_eq!(
        read["Item"]["qty"],
        json!({"N": "4"}),
        "case the table's own ARN: it addresses the table, got {read}"
    );
}
