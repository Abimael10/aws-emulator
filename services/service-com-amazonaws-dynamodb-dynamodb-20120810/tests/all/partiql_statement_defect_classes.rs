// SPDX-License-Identifier: CC0-1.0

//! Which DEFECT a malformed or inadmissible PartiQL statement is reported as, per class.
//!
//! Two rules are pinned here, each over a table of statements rather than one recorded
//! request:
//!
//! * a statement the dialect cannot parse is refused with the parser's own phrase for the
//!   POSITION the parse stopped at, so two statements that stop at different positions are
//!   two different diagnoses: a `SELECT` list with no `FROM`, an `UPDATE` stating neither
//!   action, and an `INSERT` stopping where its VALUE belongs are three classes, not one;
//! * a read addressed at an INDEX is admitted by no transaction, and the refusal names the
//!   `TransactStatements` slot that carried it. The same statement is a per-slot error of
//!   `BatchExecuteStatement` — a different surface with its own sentence — and is SERVED
//!   by the single-statement surface, which reads an index like any other query.
//!
//! Everything is driven over the wire, so nothing is asserted that a client could not
//! observe.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every statement below names, and the index on it the transaction rows read.
const TABLE: &str = "StatementClasses";
const INDEX: &str = "by-owner";

/// The fixed sentence a parse defect is stated with, over the parser's phrase for the
/// position the parse stopped at.
fn parse_failure(phrase: &str) -> String {
    format!("Statement wasn't well formed, can't be processed: {phrase}")
}

/// An emulator holding one active table with a global secondary index, and one item.
async fn seeded() -> smithy_server_runtime::MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "owner", "AttributeType": "S"},
            ],
            "GlobalSecondaryIndexes": [{
                "IndexName": INDEX,
                "KeySchema": [{"AttributeName": "owner", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "the statement-classes table", TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": TABLE,
            "Item": {"pk": {"S": "a"}, "owner": {"S": "o1"}, "n": {"N": "1"}},
        }),
    )
    .await;
    router
}

#[tokio::test]
async fn each_malformed_statement_class_states_its_own_diagnosis() {
    let router = seeded().await;

    // Each row: the case, the statement, and the whole message it must earn. The rows
    // differ in WHERE the statement stops — after a `SELECT` list, after an `UPDATE`'s
    // source, after an `INSERT`'s target, after its `VALUE` keyword — and in the verb
    // that opened it; a statement that parses and then asks for something the surface
    // does not serve states its own rule instead, which the last rows show.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a SELECT list with no FROM",
            format!("SELECT * \"{TABLE}\""),
            parse_failure("Expected FROM after SELECT list"),
        ),
        (
            "a SELECT list with no FROM and a named projection",
            format!("SELECT pk, n \"{TABLE}\""),
            parse_failure("Expected FROM after SELECT list"),
        ),
        (
            "an UPDATE stating neither SET nor REMOVE",
            format!("UPDATE \"{TABLE}\" WHERE pk = 'a'"),
            parse_failure("Expected data manipulation"),
        ),
        (
            "an UPDATE stating nothing after its source",
            format!("UPDATE \"{TABLE}\""),
            parse_failure("Expected data manipulation"),
        ),
        (
            "text that states no manipulation of data at all",
            format!("\"{TABLE}\""),
            parse_failure("Expected data manipulation"),
        ),
        (
            "an INSERT stopping after its target",
            format!("INSERT INTO \"{TABLE}\""),
            parse_failure("Unexpected term"),
        ),
        (
            "an INSERT stopping after its VALUE keyword",
            format!("INSERT INTO \"{TABLE}\" VALUE"),
            parse_failure("Unexpected term"),
        ),
        (
            "an INSERT stopping after VALUES",
            format!("INSERT INTO \"{TABLE}\" VALUES"),
            parse_failure("Unexpected term"),
        ),
        (
            "a DELETE stating no WHERE clause",
            format!("DELETE FROM \"{TABLE}\""),
            "Where clause does not contain a mandatory equality on all key attributes".to_string(),
        ),
    ];

    for (case, statement, expected) in rows {
        let (status, answered) = call(
            &router,
            "ExecuteStatement",
            json!({"Statement": statement}),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} ({statement}): expected {expected:?}, got {status} with {answered}"
        );
    }

    // The well-formed statements of the same classes are served, so the rows above are
    // the missing clause being reported rather than the verb being refused.
    for (case, statement) in [
        ("a complete INSERT", format!(
            "INSERT INTO \"{TABLE}\" VALUE {{'pk': 'b', 'owner': 'o2'}}"
        )),
        ("a complete UPDATE", format!(
            "UPDATE \"{TABLE}\" SET n = 2 WHERE pk = 'a'"
        )),
        ("a complete SELECT", format!(
            "SELECT * FROM \"{TABLE}\" WHERE pk = 'a'"
        )),
    ] {
        let (status, answered) =
            call(&router, "ExecuteStatement", json!({"Statement": statement})).await;
        assert_eq!(
            status, 200,
            "case {case:?} ({statement}): expected HTTP 200, got {status} with {answered}"
        );
    }
}

#[tokio::test]
async fn an_index_read_inside_a_transaction_is_refused_at_the_slot_that_carried_it() {
    let router = seeded().await;

    // The rows differ in WHICH slot carries the index read, and so in the position the
    // message names: the statements beside it are well formed, and the one addressed at
    // the index is the one reported.
    let index_read = format!("SELECT * FROM \"{TABLE}\".\"{INDEX}\" WHERE owner = 'o1'");
    let table_read = format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'a'");
    for (case, statements, slot) in [
        ("the only statement", vec![index_read.clone()], 0usize),
        (
            "the second of two statements",
            vec![table_read.clone(), index_read.clone()],
            1usize,
        ),
        (
            "the first of three statements",
            vec![index_read.clone(), table_read.clone(), table_read.clone()],
            0usize,
        ),
    ] {
        let transact: Vec<Value> = statements
            .iter()
            .map(|statement| json!({"Statement": statement}))
            .collect();
        let expected = format!(
            "Validation failed in TransactStatements[{slot}]: Reads on indices are not supported \
             within transactions."
        );
        let (status, answered) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": transact}),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} with {answered}"
        );
        // A top-level validation refused the call as a whole: the transaction was never
        // attempted, so there is nothing to report a cancellation of.
        assert_eq!(
            answered.get("CancellationReasons"),
            None,
            "case {case:?}: expected no cancellation, got {answered}"
        );
    }

    // The SAME statement on the other two surfaces: a per-slot error of the batch, with
    // that surface's own sentence and code, and a served read on the single-statement
    // surface, which reads an index like any other query.
    let (status, batched) = call(
        &router,
        "BatchExecuteStatement",
        json!({"Statements": [{"Statement": index_read}]}),
    )
    .await;
    assert_eq!(
        (
            status,
            batched["Responses"][0]["Error"]["Code"].clone(),
            batched["Responses"][0]["Error"]["Message"].clone()
        ),
        (
            200,
            json!("ValidationError"),
            json!(
                "Select statements within BatchExecuteStatement must specify the primary key in \
                 the where clause."
            )
        ),
        "case the batch surface: expected its own per-slot error, got {status} with {batched}"
    );

    let served = ok(
        &router,
        "ExecuteStatement",
        json!({"Statement": format!("SELECT * FROM \"{TABLE}\".\"{INDEX}\" WHERE owner = 'o1'")}),
    )
    .await;
    assert_eq!(
        served["Items"][0]["pk"].clone(),
        json!({"S": "a"}),
        "case the single-statement surface: an index read is served, got {served}"
    );
}

/// A third rule of the same family: a BARE word standing where a name belongs is read as
/// the word it spells, so one that spells a reserved keyword of the statement language is
/// diagnosed as that keyword and one that spells no keyword as the term it is.
///
/// Which is what makes `"double quotes"` the way a name colliding with a keyword is
/// written: the two diagnoses differ only in the word, and the reserved set is the
/// LANGUAGE's — its clause words and verbs, the type names a value can be cast to, the
/// date parts, the trim specifications and the names of its function special forms —
/// rather than the subset this dialect's own clauses happen to read.
#[tokio::test]
async fn a_bare_word_where_a_name_belongs_is_diagnosed_by_the_word_it_spells() {
    let router = seeded().await;

    // One word per group the language's keyword set is made of. None of them is a word
    // this dialect reads at the positions below, so what refuses each is that it is a
    // keyword at all.
    let reserved: [(&str, &str); 14] = [
        ("a clause word", "select"),
        ("a verb", "update"),
        ("an operator spelled as a word", "between"),
        ("a type name", "varchar"),
        ("the second word of a type name", "precision"),
        ("the second word of another type name", "varying"),
        ("a collection type", "tuple"),
        ("a type name spelled in full", "integer8"),
        ("a date part", "year"),
        ("the smallest date part", "second"),
        ("a compound date part", "timezone_hour"),
        ("a trim specification", "leading"),
        ("the other trim specification", "trailing"),
        ("the name of a function special form", "size"),
    ];

    // One statement per position a statement writes a NAME in: a projection item, a path
    // in a predicate, the attribute a `SET` assigns, the attribute a tuple names, and the
    // index component of a source path.
    type NamePosition = (&'static str, fn(&str) -> String);
    let positions: [NamePosition; 5] = [
        ("the projection list of a SELECT", |word| {
            format!("SELECT {word} FROM \"{TABLE}\"")
        }),
        ("a path in a WHERE clause", |word| {
            format!("SELECT * FROM \"{TABLE}\" WHERE {word} = 1")
        }),
        ("the attribute an UPDATE assigns", |word| {
            format!("UPDATE \"{TABLE}\" SET {word} = 1 WHERE pk = 'a'")
        }),
        ("an attribute of the tuple an INSERT carries", |word| {
            format!("INSERT INTO \"{TABLE}\" VALUE {{{word}: 1}}")
        }),
        ("the index component of a source path", |word| {
            format!("SELECT * FROM \"{TABLE}\".{word} WHERE pk = 'a'")
        }),
    ];

    for (group, word) in reserved {
        for (position, write) in positions {
            let statement = write(word);
            let case = format!("{group} ({word}) as {position}");
            let (status, answered) =
                call(&router, "ExecuteStatement", json!({"Statement": &statement})).await;
            let expected = parse_failure("Unexpected keyword");
            assert_eq!(
                (status, error_code(&answered).as_str(), message(&answered)),
                (400, "ValidationException", expected.clone()),
                "case {case:?} ({statement}): expected {expected:?}, got {status} with {answered}"
            );
        }
    }

    // The other half of the classification, in the one position where a bare word is no
    // name whatever it spells: a word that is NOT a keyword of the language is reported as
    // the term it is, so the two diagnoses differ only in the word.
    for (case, word) in [
        ("an attribute name that spells no keyword", "status"),
        ("one the item itself carries", "owner"),
        ("one with a digit in it", "level2"),
    ] {
        let statement = format!("INSERT INTO \"{TABLE}\" VALUE {{{word}: 1}}");
        let (status, answered) =
            call(&router, "ExecuteStatement", json!({"Statement": &statement})).await;
        let expected = parse_failure("Unexpected term");
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} ({statement}): expected {expected:?}, got {status} with {answered}"
        );
    }

    // And the escape hatch the diagnosis points at: each of those reserved words names an
    // attribute once it is QUOTED — a tuple names its attributes with a string literal,
    // every other position with a double-quoted name.
    let inserted = format!(
        "INSERT INTO \"{TABLE}\" VALUE {{'pk': 'quoted', 'year': 2001, 'second': 2, \
         'leading': 'edge'}}"
    );
    let (status, answered) =
        call(&router, "ExecuteStatement", json!({"Statement": &inserted})).await;
    assert_eq!(
        status, 200,
        "case a tuple naming reserved words as string literals ({inserted}): expected HTTP 200, \
         got {status} with {answered}"
    );
    let projected = ok(
        &router,
        "ExecuteStatement",
        json!({"Statement": format!(
            "SELECT \"year\", \"leading\" FROM \"{TABLE}\" WHERE pk = 'quoted'"
        )}),
    )
    .await;
    assert_eq!(
        projected["Items"],
        json!([{"year": {"N": "2001"}, "leading": {"S": "edge"}}]),
        "case a projection of quoted reserved names: expected both attributes, got {projected}"
    );
    let updated = ok(
        &router,
        "ExecuteStatement",
        json!({"Statement": format!(
            "UPDATE \"{TABLE}\" SET \"second\" = 3 WHERE pk = 'quoted' RETURNING ALL NEW *"
        )}),
    )
    .await;
    assert_eq!(
        updated["Items"][0]["second"],
        json!({"N": "3"}),
        "case an assignment to a quoted reserved name: expected it to be written, got {updated}"
    );

    // A keyword the grammar spells a function special form with is still read as that
    // call where the grammar admits one, so the classification above is about the word
    // standing where a NAME belongs and not about the word itself.
    let called = ok(
        &router,
        "ExecuteStatement",
        json!({"Statement": format!(
            "SELECT * FROM \"{TABLE}\" WHERE pk = 'a' AND size(owner) = 2"
        )}),
    )
    .await;
    assert_eq!(
        called["Items"][0]["pk"],
        json!({"S": "a"}),
        "case the same keyword called as a function: expected the item, got {called}"
    );
}

/// Two more refusals of the same family that turn on a CLAUSE rather than on a token: the
/// projection a `DELETE` may ask for, and a component of a source path spelled as the
/// empty name.
///
/// A `DELETE` leaves no item to report, so the only projection it can ask for is the one
/// reporting the WHOLE item it replaced, and the refusal echoes the clause the caller
/// wrote. A
/// `"quoted"` component of a source path names a table or an index of it, and the empty
/// name names neither, so such a path is refused as the malformed path it is rather than
/// resolved and reported as a lookup failure.
#[tokio::test]
async fn a_delete_returning_clause_and_an_empty_path_component_state_their_own_rule() {
    let router = seeded().await;

    // Every form the grammar spells but this verb may not ask for: the two that report the
    // item as it stands AFTER the write, and the one that names the attributes a write
    // MODIFIED, which is a projection of an update rather than of a removal.
    for (case, clause) in [
        ("the whole item after the write", "ALL NEW *"),
        ("the changed attributes after the write", "MODIFIED NEW *"),
        ("the changed attributes as they stood", "MODIFIED OLD *"),
    ] {
        let statement = format!("DELETE FROM \"{TABLE}\" WHERE pk = 'a' RETURNING {clause}");
        let expected = format!(
            "Invalid returning clause: RETURNING {clause}. Only RETURNING ALL OLD * is allowed \
             in DELETE statements."
        );
        let (status, answered) =
            call(&router, "ExecuteStatement", json!({"Statement": &statement})).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} ({statement}): expected {expected:?}, got {status} with {answered}"
        );
    }

    // The one form the sentence above names is served, so those refusals are about the
    // projection the clause asks for and not about the clause being stated.
    for (case, clause) in [("the whole item as it stood", "ALL OLD *")] {
        let key = format!("deleted-{}", clause.replace(' ', "-"));
        ok(
            &router,
            "ExecuteStatement",
            json!({"Statement": format!(
                "INSERT INTO \"{TABLE}\" VALUE {{'pk': '{key}', 'n': 4}}"
            )}),
        )
        .await;
        let deleted = ok(
            &router,
            "ExecuteStatement",
            json!({"Statement": format!(
                "DELETE FROM \"{TABLE}\" WHERE pk = '{key}' RETURNING {clause}"
            )}),
        )
        .await;
        assert_eq!(
            deleted["Items"][0]["n"],
            json!({"N": "4"}),
            "case {case:?}: expected the item as it stood, got {deleted}"
        );
    }

    // Each component of a source path, and each verb that names one: the empty name is
    // refused as the path component it is not, whichever component carries it.
    for (case, statement) in [
        (
            "the index component of a SELECT's source",
            format!("SELECT * FROM \"{TABLE}\".\"\" WHERE pk = 'a'"),
        ),
        (
            "the table component of a SELECT's source",
            "SELECT * FROM \"\" WHERE pk = 'a'".to_string(),
        ),
        (
            "the table an UPDATE names",
            "UPDATE \"\" SET n = 2 WHERE pk = 'a'".to_string(),
        ),
        (
            "the table an INSERT names",
            "INSERT INTO \"\" VALUE {'pk': 'b'}".to_string(),
        ),
        (
            "the index component of a DELETE's source",
            format!("DELETE FROM \"{TABLE}\".\"\" WHERE pk = 'a'"),
        ),
    ] {
        let (status, answered) =
            call(&router, "ExecuteStatement", json!({"Statement": &statement})).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (
                400,
                "ValidationException",
                "Path component cannot be an empty string".to_string()
            ),
            "case {case:?} ({statement}): expected the empty-component sentence, got {status} \
             with {answered}"
        );
    }
}
