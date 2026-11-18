// SPDX-License-Identifier: CC0-1.0

//! What a PARTIQL write that modifies nothing leaves on a table's change feed, and how many
//! records a sequence with no-op writes in it publishes.
//!
//! RULE: a table's stream carries "a time-ordered sequence of item-level modifications"
//! (`StreamSpecification`), so a landed item write publishes a record only when it left the
//! item different from the one it replaced — and publishes exactly one when it did, however
//! many writes that changed nothing came before it. An attribute is compared as the VALUE it
//! is, so a number respelt is the same number and a set's members are a set.
//!
//! `stream_unmodified_writes` pins that rule over the CLASSIC item surfaces (`PutItem`,
//! `UpdateItem`, `BatchWriteItem`, `TransactWriteItems`, `DeleteItem`). This suite covers the
//! two things it does not:
//!
//! * the PARTIQL write surfaces — `ExecuteStatement`, `BatchExecuteStatement` and
//!   `ExecuteTransaction` — which land items through the same store and so owe the same feed
//!   the same one record per modification;
//! * the exact record COUNT of a sequence that mixes no-op writes with a real change, which a
//!   suite that follows every row with a marker write cannot state. The sequence's last write
//!   is the one that changes the item, and a record is appended while the request that made
//!   the change is still being served, so once that write has answered the shard holds
//!   everything the sequence published: a missing record is a verdict rather than a race.
//!
//! The rows span what the rule generalises over: the surface the statement runs through, the
//! clause that restates rather than changes what is stored (an assignment, arithmetic, a
//! `REMOVE`, a set function), how many no-op writes precede the change and in which order —
//! and, as the control the rule needs to be more than "nothing is ever published", the same
//! clauses making a real change.
//!
//! Everything is driven over the wire against DynamoDB and DynamoDB Streams on one engine,
//! and every verdict is read off the streams surface a change-data-capture consumer reads.

use serde_json::{json, Value};

use crate::emulator::{
    drain_stream, emulator_with_streams, event_names, ok_case, records_of, stored_item,
    streamed_table, with_sorted_sets,
};

/// The table every row below writes to, and the attribute it is keyed on. Lower case,
/// because the rows address it from PartiQL statements.
const TABLE: &str = "statements";
/// See [`TABLE`].
const KEY: &str = "pk";

/// Which PartiQL surface a row's statement is run through: the three operations that run
/// statements, each wrapping the same statement in its own request shape.
#[derive(Debug, Clone, Copy)]
enum Surface {
    /// One statement on its own.
    Statement,
    /// One statement as a slot of a batch.
    Batch,
    /// One statement as a slot of a transaction.
    Transaction,
}

impl Surface {
    /// The operation this surface is named by on the wire.
    fn operation(self) -> &'static str {
        match self {
            Self::Statement => "ExecuteStatement",
            Self::Batch => "BatchExecuteStatement",
            Self::Transaction => "ExecuteTransaction",
        }
    }

    /// The request body that runs `statement` through it.
    fn body(self, statement: &str) -> Value {
        match self {
            Self::Statement => json!({ "Statement": statement }),
            Self::Batch => json!({ "Statements": [{ "Statement": statement }] }),
            Self::Transaction => json!({ "TransactStatements": [{ "Statement": statement }] }),
        }
    }
}

/// One row: the item its key is first given, the statement run against it, and whether that
/// statement leaves the item different from the one it replaced.
struct Row {
    case: &'static str,
    /// The key this row's item is written under, which no other row uses.
    id: &'static str,
    /// The attributes the item is created with, beside its key.
    held: Value,
    surface: Surface,
    /// The statement under test, built over this row's own key.
    statement: fn(&str) -> String,
    /// Whether that statement changes the item, which is whether the stream owes a record
    /// for it.
    modifies: bool,
}

impl Row {
    /// The item this row's key holds after its creating write.
    fn item(&self) -> Value {
        let mut item = json!({ KEY: { "S": self.id } });
        for (name, value) in self.held.as_object().cloned().unwrap_or_default() {
            item[name] = value;
        }
        item
    }
}

/// The rows: each PartiQL surface, each way a statement can restate what is stored rather
/// than change it, and the same clauses changing it.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a statement assigning the number the attribute already holds",
            id: "set-same",
            held: json!({ "n": { "N": "1" } }),
            surface: Surface::Statement,
            statement: |id| format!("UPDATE \"{TABLE}\" SET n = 1 WHERE {KEY} = '{id}'"),
            modifies: false,
        },
        Row {
            case: "a statement restating a stored number in another spelling",
            id: "set-respelt",
            held: json!({ "n": { "N": "1" } }),
            surface: Surface::Statement,
            statement: |id| format!("UPDATE \"{TABLE}\" SET n = 1.000 WHERE {KEY} = '{id}'"),
            modifies: false,
        },
        Row {
            case: "a statement adding nothing to a stored number",
            id: "add-zero",
            held: json!({ "n": { "N": "5" } }),
            surface: Surface::Statement,
            statement: |id| format!("UPDATE \"{TABLE}\" SET n = n + 0 WHERE {KEY} = '{id}'"),
            modifies: false,
        },
        Row {
            case: "a statement assigning another number",
            id: "set-other",
            held: json!({ "n": { "N": "1" } }),
            surface: Surface::Statement,
            statement: |id| format!("UPDATE \"{TABLE}\" SET n = 4 WHERE {KEY} = '{id}'"),
            modifies: true,
        },
        Row {
            case: "a statement removing an attribute the item does not carry",
            id: "remove-absent",
            held: json!({ "n": { "N": "1" } }),
            surface: Surface::Statement,
            statement: |id| format!("UPDATE \"{TABLE}\" REMOVE absent WHERE {KEY} = '{id}'"),
            modifies: false,
        },
        Row {
            case: "a statement removing an attribute the item carries",
            id: "remove-held",
            held: json!({ "n": { "N": "1" }, "gone": { "S": "x" } }),
            surface: Surface::Statement,
            statement: |id| format!("UPDATE \"{TABLE}\" REMOVE gone WHERE {KEY} = '{id}'"),
            modifies: true,
        },
        Row {
            case: "a statement adding a member the stored set already holds",
            id: "set-add-member-held",
            held: json!({ "ns": { "NS": ["1", "2"] } }),
            surface: Surface::Statement,
            statement: |id| {
                format!("UPDATE \"{TABLE}\" SET ns = set_add(ns, <<1>>) WHERE {KEY} = '{id}'")
            },
            modifies: false,
        },
        Row {
            case: "a statement deleting a member the stored set does not hold",
            id: "set-delete-absent",
            held: json!({ "ns": { "NS": ["1", "2"] } }),
            surface: Surface::Statement,
            statement: |id| {
                format!("UPDATE \"{TABLE}\" SET ns = set_delete(ns, <<9>>) WHERE {KEY} = '{id}'")
            },
            modifies: false,
        },
        Row {
            case: "a statement adding a member the stored set does not hold",
            id: "set-add-member-new",
            held: json!({ "ns": { "NS": ["1", "2"] } }),
            surface: Surface::Statement,
            statement: |id| {
                format!("UPDATE \"{TABLE}\" SET ns = set_add(ns, <<3>>) WHERE {KEY} = '{id}'")
            },
            modifies: true,
        },
        Row {
            case: "a batched statement assigning the string the attribute already holds",
            id: "batch-same",
            held: json!({ "s": { "S": "x" } }),
            surface: Surface::Batch,
            statement: |id| format!("UPDATE \"{TABLE}\" SET s = 'x' WHERE {KEY} = '{id}'"),
            modifies: false,
        },
        Row {
            case: "a batched statement assigning another string",
            id: "batch-other",
            held: json!({ "s": { "S": "x" } }),
            surface: Surface::Batch,
            statement: |id| format!("UPDATE \"{TABLE}\" SET s = 'y' WHERE {KEY} = '{id}'"),
            modifies: true,
        },
        Row {
            case: "a transacted statement assigning the string the attribute already holds",
            id: "transact-same",
            held: json!({ "s": { "S": "x" } }),
            surface: Surface::Transaction,
            statement: |id| format!("UPDATE \"{TABLE}\" SET s = 'x' WHERE {KEY} = '{id}'"),
            modifies: false,
        },
        Row {
            case: "a transacted statement assigning another string",
            id: "transact-other",
            held: json!({ "s": { "S": "x" } }),
            surface: Surface::Transaction,
            statement: |id| format!("UPDATE \"{TABLE}\" SET s = 'z' WHERE {KEY} = '{id}'"),
            modifies: true,
        },
    ]
}

/// RULE: a statement publishes a stream record only when it left the item different from the
/// one it replaced — whichever of the three PartiQL surfaces ran it.
#[tokio::test]
async fn only_a_statement_that_modifies_the_item_publishes_a_record() {
    let suite = "statements that modify nothing";
    let router = emulator_with_streams();
    let stream_arn = streamed_table(&router, suite, TABLE, KEY).await;

    for row in rows() {
        let case = row.case;
        // The item the row's statement is run against, which is one INSERT on the stream.
        ok_case(
            &router,
            case,
            "PutItem",
            json!({ "TableName": TABLE, "Item": row.item() }),
        )
        .await;
        let before = stored_item(&router, case, TABLE, KEY, row.id).await;

        // The statement under test.
        let statement = (row.statement)(row.id);
        ok_case(
            &router,
            case,
            row.surface.operation(),
            row.surface.body(&statement),
        )
        .await;
        let after = stored_item(&router, case, TABLE, KEY, row.id).await;
        assert_eq!(
            after == before,
            !row.modifies,
            "case {case:?} ({statement}): the statement must leave the item {}, held {before} \
             and now holds {after}",
            if row.modifies { "different" } else { "as it was" }
        );

        // And one write that DOES change the item, so a record the emulator only delayed
        // would still leave the row's expectation unmet.
        let mut changed = row.item();
        changed["marker"] = json!({ "S": "last" });
        ok_case(
            &router,
            case,
            "PutItem",
            json!({ "TableName": TABLE, "Item": changed }),
        )
        .await;
    }

    let drained = drain_stream(&router, suite, &stream_arn).await;
    for row in rows() {
        let case = row.case;
        let mine = records_of(&drained, KEY, row.id);
        let names = event_names(&mine);
        let mut expected = vec!["INSERT".to_string()];
        if row.modifies {
            expected.push("MODIFY".to_string());
        }
        expected.push("MODIFY".to_string());
        assert_eq!(
            names, expected,
            "case {case:?}: the item's stream records must be {expected:?}, got {names:?} from \
             {mine:?}"
        );
        // A record of a modification carries the images either side of it, so a record
        // published for a statement that changed nothing would be one whose two images are
        // the same document.
        for record in &mine {
            let change = &record["dynamodb"];
            assert_ne!(
                with_sorted_sets(&change["OldImage"]),
                with_sorted_sets(&change["NewImage"]),
                "case {case:?}: every record must describe a modification, got {record}"
            );
        }
    }
}

/// One sequence row: the writes made between the insert of the item and the write that
/// changes it. Each is named by what it restates rather than changes.
#[derive(Debug, Clone, Copy)]
enum NoOp {
    /// `PutItem` storing byte for byte the item already held.
    IdenticalPut,
    /// `UpdateItem` assigning an attribute the value it already holds.
    RestatingSet,
    /// A PartiQL `UPDATE` assigning the same value, through `ExecuteStatement`.
    RestatingStatement,
}

/// One row of the sequence table: the no-op writes made between the insert and the change.
struct Sequence {
    case: &'static str,
    id: &'static str,
    no_ops: &'static [NoOp],
}

/// The rows differ in how many no-op writes precede the change, which kinds they are and in
/// which order — including none at all, which is the control the count is read against.
const SEQUENCES: &[Sequence] = &[
    Sequence {
        case: "no write that changes nothing at all",
        id: "seq-none",
        no_ops: &[],
    },
    Sequence {
        case: "one byte-identical put",
        id: "seq-put",
        no_ops: &[NoOp::IdenticalPut],
    },
    Sequence {
        case: "one update assigning the value already held",
        id: "seq-set",
        no_ops: &[NoOp::RestatingSet],
    },
    Sequence {
        case: "a byte-identical put and then a restating update",
        id: "seq-put-set",
        no_ops: &[NoOp::IdenticalPut, NoOp::RestatingSet],
    },
    Sequence {
        case: "a restating update and then a byte-identical put",
        id: "seq-set-put",
        no_ops: &[NoOp::RestatingSet, NoOp::IdenticalPut],
    },
    Sequence {
        case: "every kind of restating write, each made twice",
        id: "seq-all-twice",
        no_ops: &[
            NoOp::IdenticalPut,
            NoOp::RestatingSet,
            NoOp::RestatingStatement,
            NoOp::IdenticalPut,
            NoOp::RestatingSet,
            NoOp::RestatingStatement,
        ],
    },
];

/// The attributes the sequence rows' item is inserted with, beside its key.
fn seeded(id: &str) -> Value {
    json!({ KEY: { "S": id }, "n": { "N": "1" } })
}

/// The operation and body one no-op write is made with, over `id`.
fn no_op_request(kind: NoOp, id: &str) -> (&'static str, Value) {
    match kind {
        NoOp::IdenticalPut => (
            "PutItem",
            json!({ "TableName": TABLE, "Item": seeded(id) }),
        ),
        NoOp::RestatingSet => (
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": { KEY: { "S": id } },
                "UpdateExpression": "SET #n = :same",
                "ExpressionAttributeNames": { "#n": "n" },
                "ExpressionAttributeValues": { ":same": { "N": "1" } },
            }),
        ),
        NoOp::RestatingStatement => (
            "ExecuteStatement",
            json!({
                "Statement": format!("UPDATE \"{TABLE}\" SET n = 1 WHERE {KEY} = '{id}'"),
            }),
        ),
    }
}

/// RULE, as a COUNT: the insert, any number of writes that change nothing, and then one write
/// that does, leave the shard holding exactly two records — `INSERT` then `MODIFY` — whose
/// images are the item either side of the one real change.
#[tokio::test]
async fn a_sequence_of_writes_that_change_nothing_leaves_only_the_insert_and_the_change() {
    for sequence in SEQUENCES {
        let case = sequence.case;
        // One emulator per row, so the shard holds this row's records and nothing else and
        // the COUNT below is the whole feed.
        let router = emulator_with_streams();
        let stream_arn = streamed_table(&router, case, TABLE, KEY).await;
        let id = sequence.id;

        ok_case(
            &router,
            case,
            "PutItem",
            json!({ "TableName": TABLE, "Item": seeded(id) }),
        )
        .await;
        let inserted = stored_item(&router, case, TABLE, KEY, id).await;

        for kind in sequence.no_ops {
            let (operation, body) = no_op_request(*kind, id);
            ok_case(&router, case, operation, body).await;
            let held = stored_item(&router, case, TABLE, KEY, id).await;
            assert_eq!(
                held, inserted,
                "case {case:?}: the {kind:?} write must leave the item as it was, expected \
                 {inserted}, got {held}"
            );
        }

        // The one write that changes the item. Its record is appended while the request is
        // still being served, so once this has answered the shard holds every record the
        // sequence published.
        ok_case(
            &router,
            case,
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": { KEY: { "S": id } },
                "UpdateExpression": "SET #n = :two",
                "ExpressionAttributeNames": { "#n": "n" },
                "ExpressionAttributeValues": { ":two": { "N": "2" } },
            }),
        )
        .await;
        let changed = stored_item(&router, case, TABLE, KEY, id).await;

        let drained = drain_stream(&router, case, &stream_arn).await;
        let mine = records_of(&drained, KEY, id);
        let names = event_names(&mine);
        assert_eq!(
            names,
            vec!["INSERT".to_string(), "MODIFY".to_string()],
            "case {case:?}: {} write(s) that change nothing publish none, so the shard must \
             hold [\"INSERT\", \"MODIFY\"], got {names:?} from {mine:?}",
            sequence.no_ops.len()
        );
        let modification = &mine[1]["dynamodb"];
        assert_eq!(
            with_sorted_sets(&modification["OldImage"]),
            inserted,
            "case {case:?}: the MODIFY record's OldImage must be the item the change replaced, \
             got {modification}"
        );
        assert_eq!(
            with_sorted_sets(&modification["NewImage"]),
            changed,
            "case {case:?}: the MODIFY record's NewImage must be the item the change stored, \
             got {modification}"
        );
    }
}
