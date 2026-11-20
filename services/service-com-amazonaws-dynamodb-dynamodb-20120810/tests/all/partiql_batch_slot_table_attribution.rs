// SPDX-License-Identifier: CC0-1.0

//! Which table one slot of a `BatchExecuteStatement` is attributed to, driven end to end.
//!
//! RULE under test: a slot echoes under `TableName` the table its statement REACHED — "The
//! table name associated with a failed PartiQL batch statement"
//! (`BatchStatementResponse$TableName`) — so a statement that ran, one whose condition no
//! stored item satisfied, and one the STORED item refused all name their table, while a
//! statement refused for its own FORM, before any item was read, reached no table and
//! carries its error alone. A table NAME this endpoint resolved but holds no table for is
//! reached in that same sense, and is echoed beside the not-found code.
//!
//! A statement the stored item refused is the class this pins: what a `SET` clause DOES is
//! decided against the value the attribute already holds, so an operand of the wrong type
//! for it, and a path the stored value cannot be stepped through, are answers the table gave
//! — they are unknowable from the statement's text alone, exactly as a failed condition is.
//! The served, condition-failed and form-refused rows stand beside them as the contrast
//! within one batch; `tests/all/partiql_index_sources_and_set_operands.rs` pins those two
//! classes on their own.
//!
//! The rows span what the rule generalises over: the OPERATOR the clause joins its operands
//! with, the stored TYPE that refused it, which operand came from the item, the kind of
//! statement (`UPDATE`, `INSERT`, `SELECT`), and the outcome (served, refused by the stored
//! item, refused for the statement's form, refused for a table that is not there).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, ok};
use crate::table_items::create_active_table;

/// The table every statement below addresses.
const TABLE: &str = "BatchSlotTables";

/// A table name nothing created, for the row whose statement reaches no table of this
/// endpoint but still resolves to one name.
const ABSENT_TABLE: &str = "BatchSlotTablesAbsent";

/// AWS's fixed sentence for an update operand the STORED attribute cannot be combined with.
const INCORRECT_OPERAND_TYPE: &str = "An operand in the update expression has an incorrect \
                                      data type";

/// AWS's fixed sentence for an update path the stored value cannot be stepped through.
const INVALID_DOCUMENT_PATH: &str = "The document path provided in the update expression is \
                                     invalid for update";

/// The sentence a batch read that does not address one item by its whole primary key earns.
const KEY_EQUALITY: &str = "Select statements within BatchExecuteStatement must specify the \
                            primary key in the where clause.";

/// One row: the statement, the `Error.Code` its slot must carry (`None` where the statement
/// is served), the `Error.Message` it must carry where that sentence is the rule (`None`
/// where only a sentence being there is), and the `TableName` the slot must report (`None`
/// where the statement reached no table).
struct Slot {
    case: &'static str,
    statement: String,
    code: Option<&'static str>,
    message: Option<&'static str>,
    table: Option<&'static str>,
}

/// An emulator holding the table, with one item per key a write row addresses: a string, a
/// boolean, a map and a number, so a clause can be refused by the type each one holds.
async fn seeded(keys: &[&str]) -> MultiServiceRouter {
    let router = crate::emulator::emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    for key in keys {
        ok(
            &router,
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {
                    "pk": {"S": *key},
                    "text": {"S": "words"},
                    "flag": {"BOOL": true},
                    "doc": {"M": {"leaf": {"S": "deep"}}},
                    "tally": {"N": "1"},
                },
            }),
        )
        .await;
    }
    router
}

/// Run a batch of `statements` and answer its slots, asserting the 200 a batch always
/// answers: "A HTTP 200 response does not mean that all statements in the
/// BatchExecuteStatement succeeded" (the operation's doc).
async fn batch(router: &MultiServiceRouter, case: &str, statements: &[&Slot]) -> Vec<Value> {
    let sent: Vec<Value> = statements
        .iter()
        .map(|slot| json!({"Statement": slot.statement}))
        .collect();
    let (status, answered) = call(
        router,
        "BatchExecuteStatement",
        json!({"Statements": sent}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: a batch answers 200 whatever its slots say, got {status} with {answered}"
    );
    let slots = answered["Responses"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        slots.len(),
        statements.len(),
        "case {case}: one slot per statement, got {answered}"
    );
    slots
}

/// Assert one slot against its row.
fn check(slot: &Value, row: &Slot) {
    let case = row.case;
    assert_eq!(
        slot["Error"].get("Code").and_then(Value::as_str),
        row.code,
        "case {case:?}: expected Error.Code {:?}, got {slot}",
        row.code
    );
    let stated = slot["Error"].get("Message").and_then(Value::as_str);
    match row.message {
        Some(message) => assert_eq!(
            stated,
            Some(message),
            "case {case:?}: expected Error.Message {message:?}, got {slot}"
        ),
        // A statement refused for where its text stopped reading states a POSITIONED
        // sentence, computed from that text rather than transcribed here; a served
        // statement states none at all.
        None => assert_eq!(
            stated.is_some(),
            row.code.is_some(),
            "case {case:?}: a refused slot states a sentence and a served one none, got {slot}"
        ),
    }
    assert_eq!(
        slot.get("TableName").and_then(Value::as_str),
        row.table,
        "case {case:?}: expected TableName {:?}, got {slot}",
        row.table
    );
}

/// The WRITE rows: a batch "must consist of either read statements or write statements", so
/// the writes are sent on their own — and each one addresses its OWN item, because a batch
/// applies at most one write per item.
fn write_rows() -> Vec<Slot> {
    vec![
        Slot {
            case: "a sum whose left operand is a stored string",
            statement: format!("UPDATE \"{TABLE}\" SET tally = text + 1 WHERE pk = 'w1'"),
            code: Some("ValidationError"),
            message: Some(INCORRECT_OPERAND_TYPE),
            table: Some(TABLE),
        },
        Slot {
            case: "a difference whose left operand is a stored string",
            statement: format!("UPDATE \"{TABLE}\" SET tally = text - 1 WHERE pk = 'w2'"),
            code: Some("ValidationError"),
            message: Some(INCORRECT_OPERAND_TYPE),
            table: Some(TABLE),
        },
        Slot {
            case: "a sum whose RIGHT operand is a stored string",
            statement: format!("UPDATE \"{TABLE}\" SET tally = tally + text WHERE pk = 'w3'"),
            code: Some("ValidationError"),
            message: Some(INCORRECT_OPERAND_TYPE),
            table: Some(TABLE),
        },
        Slot {
            case: "a sum over a stored boolean",
            statement: format!("UPDATE \"{TABLE}\" SET tally = flag + 1 WHERE pk = 'w4'"),
            code: Some("ValidationError"),
            message: Some(INCORRECT_OPERAND_TYPE),
            table: Some(TABLE),
        },
        Slot {
            case: "a sum over a stored map",
            statement: format!("UPDATE \"{TABLE}\" SET tally = doc + 1 WHERE pk = 'w5'"),
            code: Some("ValidationError"),
            message: Some(INCORRECT_OPERAND_TYPE),
            table: Some(TABLE),
        },
        Slot {
            case: "a path stepped through a stored scalar",
            statement: format!("UPDATE \"{TABLE}\" SET text.deeper = 1 WHERE pk = 'w6'"),
            code: Some("ValidationError"),
            message: Some(INVALID_DOCUMENT_PATH),
            table: Some(TABLE),
        },
        Slot {
            case: "an update the stored item does satisfy",
            statement: format!("UPDATE \"{TABLE}\" SET tally = tally + 1 WHERE pk = 'w7'"),
            code: None,
            message: None,
            table: Some(TABLE),
        },
        Slot {
            case: "an update whose WHERE clause no stored item satisfies",
            statement: format!("UPDATE \"{TABLE}\" SET tally = 1 WHERE pk = 'ghost'"),
            code: Some("ConditionalCheckFailed"),
            message: Some("The conditional request failed"),
            table: Some(TABLE),
        },
        Slot {
            case: "an update naming a table nothing created",
            statement: format!("UPDATE \"{ABSENT_TABLE}\" SET tally = 1 WHERE pk = 'w8'"),
            code: Some("ResourceNotFound"),
            message: Some("Requested resource not found"),
            table: Some(ABSENT_TABLE),
        },
        Slot {
            case: "an INSERT refused for the clause it states",
            statement: format!(
                "INSERT INTO \"{TABLE}\" VALUE {{'pk': 'w9'}} RETURNING ALL OLD *"
            ),
            code: Some("ValidationError"),
            message: Some("RETURNING is not supported in INSERT"),
            table: None,
        },
    ]
}

/// The READ rows, sent as their own batch for the same reason.
fn read_rows() -> Vec<Slot> {
    vec![
        Slot {
            case: "a read that addresses one stored item",
            statement: format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'w7'"),
            code: None,
            message: None,
            table: Some(TABLE),
        },
        Slot {
            case: "a read that pins no key at all",
            statement: format!("SELECT * FROM \"{TABLE}\""),
            code: Some("ValidationError"),
            message: Some(KEY_EQUALITY),
            table: None,
        },
        Slot {
            case: "a statement that does not parse",
            statement: format!("SELEKT * FROM \"{TABLE}\" WHERE pk = 'w7'"),
            code: Some("ValidationError"),
            message: None,
            table: None,
        },
        Slot {
            case: "a read naming a table nothing created",
            statement: format!("SELECT * FROM \"{ABSENT_TABLE}\" WHERE pk = 'w7'"),
            code: Some("ResourceNotFound"),
            message: Some("Requested resource not found"),
            table: Some(ABSENT_TABLE),
        },
    ]
}

#[tokio::test]
async fn a_write_slot_names_the_table_whenever_its_statement_reached_one() {
    let keys = ["w1", "w2", "w3", "w4", "w5", "w6", "w7"];
    let router = seeded(&keys).await;
    let rows = write_rows();
    let slots = batch(&router, "the write batch", &rows.iter().collect::<Vec<&Slot>>()).await;

    for (slot, row) in slots.iter().zip(&rows) {
        check(slot, row);
    }
}

#[tokio::test]
async fn a_read_slot_names_the_table_whenever_its_statement_reached_one() {
    let router = seeded(&["w7"]).await;
    let rows = read_rows();
    let slots = batch(&router, "the read batch", &rows.iter().collect::<Vec<&Slot>>()).await;

    for (slot, row) in slots.iter().zip(&rows) {
        check(slot, row);
    }
}

#[tokio::test]
async fn a_slot_the_stored_item_refused_wrote_nothing() {
    let keys = ["w1", "w2", "w3", "w4", "w5", "w6", "w7"];
    let router = seeded(&keys).await;
    let rows = write_rows();
    batch(&router, "the write batch", &rows.iter().collect::<Vec<&Slot>>()).await;

    // A slot that reported an error left its item as it stood; the one that ran is the only
    // key whose stored value moved.
    for key in ["w1", "w2", "w3", "w4", "w5", "w6"] {
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": key}}}),
        )
        .await;
        assert_eq!(
            read["Item"]["tally"],
            json!({"N": "1"}),
            "case {key}: a refused slot writes nothing, got {read}"
        );
        assert_eq!(
            read["Item"]["text"],
            json!({"S": "words"}),
            "case {key}: a refused slot writes nothing, got {read}"
        );
    }
    let ran = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "w7"}}}),
    )
    .await;
    assert_eq!(
        ran["Item"]["tally"],
        json!({"N": "2"}),
        "case w7: the slot that ran applied its sum, got {ran}"
    );
    let ghost = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "ghost"}}}),
    )
    .await;
    assert_eq!(
        ghost.get("Item"),
        None,
        "case ghost: a condition no item satisfied stores nothing, got {ghost}"
    );
}
