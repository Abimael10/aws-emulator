// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a Data API call gets when the statement, its parameters, its result or
//! its transaction is at fault — driven end to end over the real wire path, so what is
//! asserted is the status, the error code and the message a client sees.
//!
//! The rules under test, each stated above the test that pins it and each asserted over a
//! TABLE of inputs rather than one call:
//!
//! * an error the DATABASE raised while running a statement is the modelled
//!   `DatabaseErrorException`, carrying the engine's own diagnostic, on every path a
//!   statement reaches the engine by ([`an_error_the_database_raised_is_the_database_error_class`]);
//! * a `parameters` list that cannot be bound is a VALIDATION fault, one message per
//!   distinct fault, and the statement does not run
//!   ([`a_binding_fault_is_a_validation_fault_with_its_own_message`]);
//! * a `transactionId` the resource could have issued and no transaction is RUNNING under
//!   is HTTP 404 `TransactionNotFoundException` with the terminated-transaction sentence,
//!   on every operation that takes one, and a transaction a previous call ENDED is
//!   remembered as ended
//!   ([`a_transaction_that_is_not_running_is_refused_with_the_same_sentence`]);
//! * a statement whose RESULT the API cannot represent still RAN: the refusal arrives
//!   after the write, and the row it inserted stays
//!   ([`a_refused_result_keeps_the_write_the_statement_made`]);
//! * a batch is one unit of work: a statement of it that the database refuses refuses the
//!   whole batch and leaves nothing behind
//!   ([`a_batch_a_statement_of_it_fails_lands_nothing`]);
//! * a result column of a type the `Field` union has no member for is refused by THAT
//!   TYPE, in every form it can be read in, while the same cell cast to a type the union
//!   carries is served
//!   ([`a_result_of_a_type_no_field_member_carries_is_refused_by_its_type`]);
//! * a `stringValue` its `typeHint` cannot read is refused with the sentence of THAT
//!   HINT's own class, on both statement operations
//!   ([`an_unreadable_hinted_value_is_refused_in_the_sentence_of_its_hint`]).
//!
//! These rules are about the data plane, so this suite drives the Data API alone: no
//! control plane is mounted, and the cluster, secret and identity legs of the precondition
//! chain are undecided here (`tests/all/call_preconditions.rs` is the suite that mounts
//! the services those legs ask).

use serde_json::{json, Value};

use crate::sql_data_api::{a_sibling_id, error_code, list, number, parameter, DataApi};

/// The sentence every refusal of a `transactionId` that names no running transaction
/// carries, as this service states it once
/// (`state::TRANSACTION_NOT_RUNNING_MESSAGE`).
fn not_running_message() -> String {
    service_com_amazonaws_rdsdata_rdsdataservice::state::TRANSACTION_NOT_RUNNING_MESSAGE.to_string()
}

/// PostgreSQL's own sentence for a statement that reached a transaction one of its own
/// statements had already failed in, with that condition's SQLSTATE.
fn aborted_transaction_message() -> String {
    "ERROR: current transaction is aborted, commands ignored until end of transaction block; \
     SQLState: 25P02"
        .to_string()
}

/// The table every row below writes into, and the row it starts with.
async fn given_ledger(api: &DataApi) {
    api.run(json!({
        "sql": "CREATE TABLE ledger (id INT PRIMARY KEY, note TEXT NOT NULL, span INTERVAL)",
    }))
    .await;
    api.run(json!({"sql": "INSERT INTO ledger (id, note) VALUES (1, 'first')"}))
        .await;
}

/// The ids the ledger holds now, in the order the engine returns them.
async fn ledger_ids(api: &DataApi) -> Vec<i64> {
    let selected = api
        .run(json!({"sql": "SELECT id FROM ledger ORDER BY id"}))
        .await;
    list(&selected, "records")
        .iter()
        .filter_map(|row| {
            row.as_array()
                .and_then(|row| row.first())
                .and_then(|field| field.get("longValue"))
                .and_then(Value::as_i64)
        })
        .collect()
}

/// RULE: an error the DATABASE raised while running a statement is reported as the
/// modelled `DatabaseErrorException` (HTTP 400, "There was an error in processing the SQL
/// statement") carrying the diagnostic the DATABASE THIS API ANSWERS FOR words that
/// condition with — PostgreSQL's own sentence and its SQLSTATE, never the backing engine's
/// own wording and never the deprecated `BadRequestException` — on every path a statement
/// reaches the engine by, and the data the statement would have written is not written.
///
/// The rows differ along WHICH error the database raised (a key the primary key already
/// holds, a value a unique constraint over another column already holds, a column that may
/// not be NULL, a table that does not exist, a column that does not exist, a statement it
/// cannot parse) and along the path it reached the engine by (a single statement, a batch,
/// a statement inside a transaction).
#[tokio::test]
async fn an_error_the_database_raised_is_the_database_error_class() {
    let api = DataApi::new();
    given_ledger(&api).await;
    api.run(json!({"sql": "CREATE TABLE tags (id INT PRIMARY KEY, label TEXT UNIQUE)"}))
        .await;
    api.run(json!({"sql": "INSERT INTO tags (id, label) VALUES (1, 'taken')"}))
        .await;

    // (case, the statement, the diagnostic the refusal carries: the whole of it, as the
    // database this API answers for words that condition)
    let rows: &[(&str, &str, &str)] = &[
        (
            "a key the primary key already holds",
            "INSERT INTO ledger (id, note) VALUES (1, 'again')",
            "ERROR: duplicate key value violates unique constraint \"ledger_pkey\"; \
             SQLState: 23505",
        ),
        (
            "a value a unique constraint over another column already holds",
            "INSERT INTO tags (id, label) VALUES (2, 'taken')",
            "ERROR: duplicate key value violates unique constraint \"tags_label_key\"; \
             SQLState: 23505",
        ),
        (
            "a NULL in a column that may not hold one",
            "INSERT INTO ledger (id, note) VALUES (2, NULL)",
            "ERROR: null value in column \"note\" of relation \"ledger\" violates not-null \
             constraint; SQLState: 23502",
        ),
        (
            "a table that does not exist",
            "INSERT INTO absent (id) VALUES (2)",
            "ERROR: relation \"absent\" does not exist; SQLState: 42P01",
        ),
        (
            "a column that does not exist",
            "INSERT INTO ledger (id, absent) VALUES (2, 'x')",
            "ERROR: column \"absent\" of relation \"ledger\" does not exist; SQLState: 42703",
        ),
        (
            "a column that does not exist in a query",
            "SELECT absent FROM ledger",
            "ERROR: column \"absent\" does not exist; SQLState: 42703",
        ),
        (
            "a statement the engine cannot parse",
            "INSERTO INTO ledger VALUES (2)",
            "ERROR: syntax error at or near \"INSERTO\"; Position: 1; SQLState: 42601",
        ),
    ];
    for (case, sql, diagnostic) in rows {
        // The single-statement path.
        let (status, body) = api.execute(json!({"sql": sql})).await;
        assert_eq!(status, 400, "case {case}: expected HTTP 400, got {body}");
        assert_eq!(
            error_code(&body),
            "DatabaseErrorException",
            "case {case}: expected DatabaseErrorException, got {body}"
        );
        let message = body["message"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            message, *diagnostic,
            "case {case}: expected PostgreSQL's own sentence and SQLSTATE, got {message:?}"
        );

        // The batch path, where the same error is the same class.
        let (status, batched) = api
            .batch(json!({"sql": sql, "parameterSets": [[]]}))
            .await;
        assert_eq!(
            status, 400,
            "case {case} as a batch: expected HTTP 400, got {batched}"
        );
        assert_eq!(
            error_code(&batched),
            "DatabaseErrorException",
            "case {case} as a batch: expected DatabaseErrorException, got {batched}"
        );

        // The in-transaction path: the same class, and the statement that failed ABORTS
        // the transaction it ran in — the database ignores every further command of it,
        // and a commit of it discards what it wrote while still reporting the status a
        // commit reports.
        let transaction = api.begin_transaction(None).await;
        let (status, inside) = api
            .execute(json!({"sql": sql, "transactionId": transaction.clone()}))
            .await;
        assert_eq!(
            status, 400,
            "case {case} inside a transaction: expected HTTP 400, got {inside}"
        );
        assert_eq!(
            error_code(&inside),
            "DatabaseErrorException",
            "case {case} inside a transaction: expected DatabaseErrorException, got {inside}"
        );
        let (status, after) = api
            .execute(json!({
                "sql": "INSERT INTO ledger (id, note) VALUES (100, 'after the failure')",
                "transactionId": transaction.clone(),
            }))
            .await;
        assert_eq!(
            status, 400,
            "case {case}: a statement on the aborted transaction is refused, got {after}"
        );
        assert_eq!(
            error_code(&after),
            "DatabaseErrorException",
            "case {case}: expected the database's own error for an aborted transaction, got \
             {after}"
        );
        assert_eq!(
            after["message"],
            json!(aborted_transaction_message()),
            "case {case}: expected PostgreSQL's aborted-transaction sentence, got {after}"
        );
        let (status, committed) = api.commit(&transaction).await;
        assert_eq!(
            status, 200,
            "case {case}: a commit still ends an aborted transaction, got {committed}"
        );
        assert_eq!(
            committed["transactionStatus"],
            json!("Transaction Committed"),
            "case {case}: a commit reports the status of the commit it was asked for, \
             got {committed}"
        );
    }

    // Not one of the refused statements wrote anything.
    let ids = ledger_ids(&api).await;
    assert_eq!(
        ids,
        vec![1],
        "case after every refusal: expected only the row the working statement wrote, got \
         {ids:?}"
    );
}

/// RULE: a `parameters` list the statement's placeholders cannot be bound from is a fault
/// of the CALL, so it is refused with HTTP 400 `ValidationException` — one message per
/// distinct fault, each naming the parameter it is about — and the statement does not run.
///
/// The rows differ along WHICH binding fault it is: a placeholder no parameter supplies, a
/// parameter name sent twice, a name carrying the ':' that introduces a placeholder in the
/// statement, and an `arrayValue`, which the `Field` union declares but `parameters` says
/// is not supported.
#[tokio::test]
async fn a_binding_fault_is_a_validation_fault_with_its_own_message() {
    let api = DataApi::new();
    given_ledger(&api).await;

    let insert = "INSERT INTO ledger (id, note) VALUES (:id, :note)";
    // (case, the parameters sent, the message AWS words that fault with)
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "a placeholder no parameter supplies",
            json!([parameter("id", json!({"longValue": 2}))]),
            "Cannot find parameter: note",
        ),
        (
            "a parameter name sent twice",
            json!([
                parameter("id", json!({"longValue": 2})),
                parameter("id", json!({"longValue": 3})),
                parameter("note", json!({"stringValue": "second"})),
            ]),
            "Duplicate parameter id",
        ),
        (
            "a parameter name carrying the placeholder prefix",
            json!([
                parameter(":id", json!({"longValue": 2})),
                parameter("note", json!({"stringValue": "second"})),
            ]),
            "Named parameter syntax is invalid, input: :id",
        ),
        (
            "an array parameter",
            json!([
                parameter("id", json!({"longValue": 2})),
                parameter("note", json!({"arrayValue": {"stringValues": ["a", "b"]}})),
            ]),
            "Array parameters are not supported.",
        ),
    ];
    for (case, parameters, message) in rows {
        for (path, members) in [
            ("ExecuteStatement", json!({"sql": insert, "parameters": parameters})),
            (
                "BatchExecuteStatement",
                json!({"sql": insert, "parameterSets": [parameters]}),
            ),
        ] {
            let (status, body) = if path == "ExecuteStatement" {
                api.execute(members).await
            } else {
                api.batch(members).await
            };
            assert_eq!(
                status, 400,
                "case {case} on {path}: expected HTTP 400, got {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case} on {path}: expected ValidationException, got {body}"
            );
            assert_eq!(
                body["message"],
                json!(message),
                "case {case} on {path}: expected {message:?}, got {body}"
            );
        }
    }

    // A rejected binding writes nothing, and the same statement with every placeholder
    // bound once runs.
    let ids = ledger_ids(&api).await;
    assert_eq!(
        ids,
        vec![1],
        "case after every binding fault: expected no row written, got {ids:?}"
    );

    // The LENGTH of the list is the caller's to choose, so it is capped: a request
    // carrying more values than a statement can bind is refused before the list is
    // scanned, on both operations.
    let over_cap: Vec<Value> = (0..=service_com_amazonaws_rdsdata_rdsdataservice::state::MAX_PARAMETERS_PER_STATEMENT)
        .map(|index| parameter(&format!("p{index}"), json!({"longValue": 1})))
        .collect();
    for (path, members) in [
        (
            "ExecuteStatement",
            json!({"sql": insert, "parameters": over_cap.clone()}),
        ),
        (
            "BatchExecuteStatement",
            json!({"sql": insert, "parameterSets": [over_cap.clone()]}),
        ),
    ] {
        let (status, body) = if path == "ExecuteStatement" {
            api.execute(members).await
        } else {
            api.batch(members).await
        };
        assert_eq!(
            status, 400,
            "case a parameter list past the cap on {path}: expected HTTP 400, got {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case a parameter list past the cap on {path}: expected ValidationException, got \
             {body}"
        );
    }

    let inserted = api
        .run(json!({
            "sql": insert,
            "parameters": [
                parameter("id", json!({"longValue": 2})),
                parameter("note", json!({"stringValue": "second"})),
            ],
        }))
        .await;
    assert_eq!(
        number(&inserted, "numberOfRecordsUpdated"),
        1,
        "case every placeholder bound: expected the row written, got {inserted}"
    );
}

/// RULE: a `transactionId` the resource the call names COULD have issued and no transaction
/// of that resource is RUNNING under is refused with HTTP 404
/// `TransactionNotFoundException` and the fixed sentence AWS words that class with — on
/// every operation that takes one, and however the transaction stopped running: a commit
/// ended it, a rollback ended it, a statement of its own that failed ended it, or no
/// transaction was ever opened under that id. An id of this service's own form that
/// belongs to ANOTHER resource is refused APART from those, as the invalid argument it
/// is. Refusing the call changes nothing either way: the committed row stays readable
/// and the rolled-back write stays absent.
/// (A value that could NOT be an id of this service at all is a fault of the call's own
/// shape instead — `sql_data_api` pins that class.)
#[tokio::test]
async fn a_transaction_that_is_not_running_is_refused_with_the_same_sentence() {
    let api = DataApi::new();
    given_ledger(&api).await;

    // A transaction that COMMITTED: its first commit reports the status, and the id is
    // then no longer running.
    let committed = api.begin_transaction(None).await;
    api.run(json!({
        "sql": "INSERT INTO ledger (id, note) VALUES (2, 'committed')",
        "transactionId": committed.clone(),
    }))
    .await;
    let (status, answered) = api.commit(&committed).await;
    assert_eq!(status, 200, "case the first commit: got {answered}");
    assert_eq!(
        answered["transactionStatus"],
        json!("Transaction Committed"),
        "case the first commit: expected the committed status, got {answered}"
    );

    // A transaction that ROLLED BACK, whose write never lands.
    let rolled_back = api.begin_transaction(None).await;
    api.run(json!({
        "sql": "INSERT INTO ledger (id, note) VALUES (3, 'discarded')",
        "transactionId": rolled_back.clone(),
    }))
    .await;
    let (status, answered) = api.rollback(&rolled_back).await;
    assert_eq!(status, 200, "case the first rollback: got {answered}");
    assert_eq!(
        answered["transactionStatus"],
        json!("Rollback complete"),
        "case the first rollback: expected the rollback status, got {answered}"
    );

    // A transaction a statement of its own ABORTED, and that a rollback then ENDED: an
    // aborted transaction is still there until a commit or a rollback ends it.
    let terminated = api.begin_transaction(None).await;
    let (status, failed) = api
        .execute(json!({
            "sql": "INSERT INTO ledger (id, note) VALUES (1, 'duplicate')",
            "transactionId": terminated.clone(),
        }))
        .await;
    assert_eq!(status, 400, "case the failing statement: got {failed}");
    let (status, answered) = api.rollback(&terminated).await;
    assert_eq!(
        status, 200,
        "case the rollback of an aborted transaction: got {answered}"
    );

    // An id this cluster COULD have issued and never did: the resource binding a minted id
    // carries is this cluster's, and the rest of it names no transaction that was ever
    // opened.
    let never_issued = a_sibling_id(&committed);

    // Every id that is not running, on every operation that takes one.
    let ids = [
        ("a committed transaction", committed.as_str()),
        ("a rolled-back transaction", rolled_back.as_str()),
        ("a transaction its own statement aborted and a rollback ended", terminated.as_str()),
        ("an id this cluster could have issued and never did", never_issued.as_str()),
    ];
    for (case, transaction_id) in ids {
        let calls = vec![
            (
                "ExecuteStatement",
                api.execute(json!({
                    "sql": "INSERT INTO ledger (id, note) VALUES (9, 'late')",
                    "transactionId": transaction_id,
                }))
                .await,
            ),
            (
                "BatchExecuteStatement",
                api.batch(json!({
                    "sql": "INSERT INTO ledger (id, note) VALUES (9, 'late')",
                    "parameterSets": [[]],
                    "transactionId": transaction_id,
                }))
                .await,
            ),
            (
                "CommitTransaction",
                api.commit(transaction_id).await,
            ),
            ("RollbackTransaction", api.rollback(transaction_id).await),
        ];
        for (operation, (status, body)) in calls {
            assert_eq!(
                status, 404,
                "case {case} on {operation}: expected HTTP 404, got {status} {body}"
            );
            assert_eq!(
                error_code(&body),
                "TransactionNotFoundException",
                "case {case} on {operation}: expected TransactionNotFoundException, got {body}"
            );
            assert_eq!(
                body["message"],
                json!(not_running_message()),
                "case {case} on {operation}: expected the terminated-transaction sentence, got \
                 {body}"
            );
        }
    }

    // And an id of THIS SERVICE's own form that belongs to ANOTHER resource: refused
    // APART from the ids above, on the resource the id itself carries, because the
    // transaction it names may well still be running on the cluster that issued it.
    let foreign = api
        .begin_transaction_on("arn:aws:rds:us-east-1:000000000000:cluster:other")
        .await;
    let calls = vec![
        (
            "ExecuteStatement",
            api.execute(json!({
                "sql": "INSERT INTO ledger (id, note) VALUES (9, 'late')",
                "transactionId": foreign.clone(),
            }))
            .await,
        ),
        (
            "BatchExecuteStatement",
            api.batch(json!({
                "sql": "INSERT INTO ledger (id, note) VALUES (9, 'late')",
                "parameterSets": [[]],
                "transactionId": foreign.clone(),
            }))
            .await,
        ),
        ("CommitTransaction", api.commit(&foreign).await),
        ("RollbackTransaction", api.rollback(&foreign).await),
    ];
    for (operation, (status, body)) in calls {
        assert_eq!(
            (status, error_code(&body), body["message"].clone()),
            (400, "ValidationException", json!("Invalid transaction ID")),
            "case an id of another resource on {operation}: expected the invalid-argument \
             refusal, got {status} {body}"
        );
    }

    // None of those refusals changed the data: the committed row is there, and neither
    // the rolled-back write nor any of the refused statements is.
    let ids = ledger_ids(&api).await;
    assert_eq!(
        ids,
        vec![1, 2],
        "case after every refusal: expected the committed rows only, got {ids:?}"
    );
}

/// RULE: a statement whose RESULT the API cannot represent still RAN. The result is shaped
/// after the statement has been applied, so the call is refused with
/// `UnsupportedResultException` AND the row the statement inserted stays — where a
/// statement refused for its own shape (one the engine cannot parse, a parameter that
/// cannot be bound) never runs at all and writes nothing.
///
/// The rows differ along WHICH result the API cannot represent: a multidimensional array
/// and a data type the `Field` union has no member for, each named by a different part of
/// the statement (a constructor, a cast).
#[tokio::test]
async fn a_refused_result_keeps_the_write_the_statement_made() {
    let api = DataApi::new();
    given_ledger(&api).await;

    // (case, the statement, the id it inserts, the message the refusal carries)
    let rows: &[(&str, &str, i64, &str)] = &[
        (
            "a multidimensional array returned by an insert",
            "INSERT INTO ledger (id, note) VALUES (2, 'returned') \
             RETURNING ARRAY[[1, 2], [3, 4]] AS grid",
            2,
            "The result contains a multidimensional array.",
        ),
        (
            "an unsupported data type returned by an insert",
            "INSERT INTO ledger (id, note) VALUES (3, 'returned') \
             RETURNING '1 day'::interval AS iv",
            3,
            "The result contains the unsupported data type INTERVAL.",
        ),
        (
            "a braced literal of arrays returned by an insert",
            "INSERT INTO ledger (id, note) VALUES (4, 'returned') \
             RETURNING '{{1,2},{3,4}}'::int4[] AS grid",
            4,
            "The result contains a multidimensional array.",
        ),
        (
            "an unsupported data type of a column the table declares",
            "INSERT INTO ledger (id, note, span) VALUES (5, 'returned', '1 day') \
             RETURNING span",
            5,
            "The result contains the unsupported data type INTERVAL.",
        ),
    ];
    let mut landed = vec![1];
    for (case, sql, id, message) in rows {
        let (status, body) = api.execute(json!({"sql": sql})).await;
        assert_eq!(status, 400, "case {case}: expected HTTP 400, got {body}");
        assert_eq!(
            error_code(&body),
            "UnsupportedResultException",
            "case {case}: expected UnsupportedResultException, got {body}"
        );
        assert_eq!(
            body["message"],
            json!(message),
            "case {case}: expected {message:?}, got {body}"
        );
        landed.push(*id);
        let ids = ledger_ids(&api).await;
        assert_eq!(
            ids, landed,
            "case {case}: the refusal shaped the result AFTER the row was written, so the \
             row must be present: expected {landed:?}, got {ids:?}"
        );
    }

    // A value the DATABASE holds and one response cannot CARRY: the same rule, on the
    // other result the model names ("The size was too large"). The statement runs, the row
    // stays, and the result is refused — which is why the engine's own value limit is
    // larger than what a response may carry.
    let over_the_cap =
        service_com_amazonaws_rdsdata_rdsdataservice::state::MAX_RESPONSE_BYTES + 1_024;
    let (status, refused) = api
        .execute(json!({
            "sql": format!(
                "INSERT INTO ledger (id, note) VALUES (6, repeat('x', {over_the_cap})) \
                 RETURNING note"
            ),
        }))
        .await;
    assert_eq!(
        (status, error_code(&refused)),
        (400, "UnsupportedResultException"),
        "case a result larger than one response may carry: got {status} {refused}"
    );
    assert_eq!(
        refused["message"],
        json!(format!(
            "The result exceeds the size limit {} MB.",
            service_com_amazonaws_rdsdata_rdsdataservice::state::MAX_RESPONSE_BYTES
                / (1024 * 1024)
        )),
        "case a result larger than one response may carry: expected the size refusal's \
         own sentence, got {refused}"
    );
    landed.push(6);
    let ids = ledger_ids(&api).await;
    assert_eq!(
        ids, landed,
        "case a result larger than one response may carry: the row the statement wrote \
         must be present: expected {landed:?}, got {ids:?}"
    );
    // And the value the row holds is the whole value the statement computed, read back
    // through its length rather than its bytes — which no response can carry.
    let measured = api
        .run(json!({"sql": "SELECT length(note) AS size FROM ledger WHERE id = 6"}))
        .await;
    assert_eq!(
        measured["records"],
        json!([[{"longValue": over_the_cap as i64}]]),
        "case a result larger than one response may carry: expected the stored value's \
         whole length, got {measured}"
    );

    // A QUERY of the rows those refusals left behind is served, so what was refused is
    // the RESULT of each write and not the rows it wrote.
    let read_back = api
        .run(json!({"sql": "SELECT count(*) AS rows FROM ledger"}))
        .await;
    assert_eq!(
        read_back["records"],
        json!([[{"longValue": landed.len() as i64}]]),
        "case reading the rows back: expected every landed row, got {read_back}"
    );

    // By contrast, a statement refused for its own shape never ran, so it wrote nothing.
    let (status, refused) = api
        .execute(json!({"sql": "INSERTO INTO ledger (id) VALUES (8)"}))
        .await;
    assert_eq!(status, 400, "case a statement the engine cannot parse: got {refused}");
    let ids = ledger_ids(&api).await;
    assert_eq!(
        ids, landed,
        "case a statement the engine cannot parse: expected nothing written, got {ids:?}"
    );
}

/// What AWS tells a caller whose result carries a floating value that is not a number: the
/// fixed sentence the modelled `UnsupportedResultException` carries for that condition.
const NON_FINITE_MESSAGE: &str = "NaN and Infinity values are not supported";

/// What AWS tells a caller that carries the `schema` member, whose model documents it as a
/// member the service does not support: the fixed sentence of that refusal.
const SCHEMA_UNSUPPORTED_MESSAGE: &str = "The schema parameter isn't supported.";

/// RULE: a floating value that is NOT A NUMBER — the `NaN`, `Infinity` and `-Infinity`
/// values PostgreSQL documents for every one of its floating types — is kept as the value
/// it is: the statement RUNS and the row it wrote STAYS, `records` cannot carry the value
/// (no member of the `Field` union holds one, so the call is refused with
/// `UnsupportedResultException` and AWS's own sentence for it) and `formatRecordsAs=JSON`
/// renders it as the string the database displays it as.
///
/// The rows differ along which value it is and how the statement spells it (each
/// documented name, its abbreviation, in either case), and every row is read back three
/// ways: by the statement that wrote it, by a later statement, and in the JSON form. The
/// values that come from somewhere else — a parameter the request bound, an element of an
/// array, a number the engine itself computed (`sql_data_api`) — follow.
#[tokio::test]
async fn a_float_that_is_not_a_number_is_kept_and_refused_in_records() {
    let api = DataApi::new();
    api.run(json!({
        "sql": "CREATE TABLE readings (id INT PRIMARY KEY, dbl DOUBLE PRECISION)",
    }))
    .await;

    // (case, the value as the statement writes it, the text the database displays it as)
    let rows: &[(&str, &str, &str)] = &[
        ("not a number", "'NaN'::float8", "NaN"),
        ("not a number, in lower case", "'nan'::double precision", "NaN"),
        ("the positive infinity", "'Infinity'::float8", "Infinity"),
        ("the positive infinity, abbreviated", "'inf'::float8", "Infinity"),
        ("the negative infinity", "'-Infinity'::float8", "-Infinity"),
        ("the negative infinity, abbreviated", "'-INF'::real", "-Infinity"),
    ];
    for (index, (case, written, displayed)) in rows.iter().enumerate() {
        let id = index as i64 + 1;
        let (status, refused) = api
            .execute(json!({
                "sql": format!(
                    "INSERT INTO readings (id, dbl) VALUES ({id}, {written}) RETURNING dbl"
                ),
            }))
            .await;
        assert_eq!(
            (status, error_code(&refused), refused["message"].clone()),
            (
                400,
                "UnsupportedResultException",
                json!(NON_FINITE_MESSAGE)
            ),
            "case {case:?}: expected the unsupported-result refusal, got {status} {refused}"
        );

        // The statement RAN: the row it wrote is there, read through a column the
        // refusal is not about.
        let stored = api
            .run(json!({"sql": format!("SELECT id FROM readings WHERE id = {id}")}))
            .await;
        assert_eq!(
            stored["records"],
            json!([[{"longValue": id}]]),
            "case {case:?}: the refusal shaped the result AFTER the row was written, so the \
             row must be present, got {stored}"
        );

        // And the value the column holds is the one that was written: a LATER read of it
        // earns the same refusal, and the JSON form of that read renders it as the text
        // the database displays it as.
        let read_back = format!("SELECT dbl FROM readings WHERE id = {id}");
        let (status, again) = api.execute(json!({"sql": read_back.clone()})).await;
        assert_eq!(
            (status, error_code(&again), again["message"].clone()),
            (
                400,
                "UnsupportedResultException",
                json!(NON_FINITE_MESSAGE)
            ),
            "case {case:?}: reading the stored value back is the same refusal, got {status} \
             {again}"
        );
        let formatted = api
            .run(json!({"sql": read_back, "formatRecordsAs": "JSON"}))
            .await;
        assert_eq!(
            formatted["formattedRecords"],
            json!(format!("[{{\"dbl\":\"{displayed}\"}}]")),
            "case {case:?}: expected the JSON string {displayed:?}, got {formatted}"
        );
    }

    // A value the REQUEST carried, cast by the statement to a floating type: the same
    // value, so the same answer.
    let (status, refused) = api
        .execute(json!({
            "sql": "INSERT INTO readings (id, dbl) VALUES (:id, CAST(:value AS float8)) \
                    RETURNING dbl",
            "parameters": [
                parameter("id", json!({"longValue": 20})),
                parameter("value", json!({"stringValue": "NaN"})),
            ],
        }))
        .await;
    assert_eq!(
        (status, error_code(&refused), refused["message"].clone()),
        (
            400,
            "UnsupportedResultException",
            json!(NON_FINITE_MESSAGE)
        ),
        "case \"a parameter the request bound\": got {status} {refused}"
    );
    let formatted = api
        .run(json!({
            "sql": "SELECT dbl FROM readings WHERE id = 20",
            "formatRecordsAs": "JSON",
        }))
        .await;
    assert_eq!(
        formatted["formattedRecords"],
        json!("[{\"dbl\":\"NaN\"}]"),
        "case \"a parameter the request bound\": got {formatted}"
    );

    // An ELEMENT of an array is the same value in a list: `arrayValue`'s `doubleValues`
    // holds a double per element and no such value either, and the JSON form renders the
    // element as that same string.
    let (status, refused) = api
        .execute(json!({"sql": "SELECT '{1,NaN}'::float8[] AS vals"}))
        .await;
    assert_eq!(
        (status, error_code(&refused), refused["message"].clone()),
        (
            400,
            "UnsupportedResultException",
            json!(NON_FINITE_MESSAGE)
        ),
        "case \"an element of an array\": got {status} {refused}"
    );
    let formatted = api
        .run(json!({
            "sql": "SELECT '{1,NaN}'::float8[] AS vals",
            "formatRecordsAs": "JSON",
        }))
        .await;
    let rendered: Value = serde_json::from_str(
        formatted["formattedRecords"]
            .as_str()
            .unwrap_or_else(|| panic!("formattedRecords must be a string: {formatted}")),
    )
    .expect("formattedRecords parses as JSON");
    assert_eq!(
        rendered,
        json!([{"vals": [1.0, "NaN"]}]),
        "case \"an element of an array\": got {formatted}"
    );

    // A FINITE value of the same column is unchanged by any of it: it comes back as the
    // number it is, in both forms.
    api.run(json!({
        "sql": "INSERT INTO readings (id, dbl) VALUES (30, '2.5'::float8)",
    }))
    .await;
    let selected = api
        .run(json!({"sql": "SELECT dbl FROM readings WHERE id = 30"}))
        .await;
    assert_eq!(
        selected["records"],
        json!([[{"doubleValue": 2.5}]]),
        "case \"a finite value of the same column\": got {selected}"
    );
    let formatted = api
        .run(json!({
            "sql": "SELECT dbl FROM readings WHERE id = 30",
            "formatRecordsAs": "JSON",
        }))
        .await;
    assert_eq!(
        formatted["formattedRecords"],
        json!("[{\"dbl\":2.5}]"),
        "case \"a finite value of the same column\": got {formatted}"
    );
}

/// RULE: the `schema` member is one the service does not support — each operation that
/// models it documents that — so a request carrying it is refused with HTTP 400
/// `ValidationException` and the fixed sentence AWS words that refusal with, on every
/// operation that models the member, and nothing the request asked for runs.
///
/// The rows differ along the schema the request names (the name of the default schema,
/// another name, an empty value), along the operation carrying it, and along what the
/// statement would have done (a read, a write) — because it is the MEMBER that is refused
/// and not what it names.
#[tokio::test]
async fn a_call_carrying_the_schema_member_is_refused() {
    let api = DataApi::new();
    given_ledger(&api).await;

    for (case, schema) in [
        ("the default schema", "public"),
        ("a schema of another name", "reporting"),
        ("a schema named as the database is", "ledger"),
        ("an empty schema", ""),
    ] {
        let calls = vec![
            (
                "ExecuteStatement over a read",
                api.execute(json!({"sql": "SELECT 1 AS one", "schema": schema}))
                    .await,
            ),
            (
                "ExecuteStatement over a write",
                api.execute(json!({
                    "sql": "INSERT INTO ledger (id, note) VALUES (7, 'schema')",
                    "schema": schema,
                }))
                .await,
            ),
            (
                "BatchExecuteStatement",
                api.batch(json!({
                    "sql": "INSERT INTO ledger (id, note) VALUES (:id, :note)",
                    "parameterSets": [[
                        parameter("id", json!({"longValue": 8})),
                        parameter("note", json!({"stringValue": "schema"})),
                    ]],
                    "schema": schema,
                }))
                .await,
            ),
        ];
        for (operation, (status, body)) in calls {
            assert_eq!(
                (status, error_code(&body), body["message"].clone()),
                (
                    400,
                    "ValidationException",
                    json!(SCHEMA_UNSUPPORTED_MESSAGE)
                ),
                "case {case:?} on {operation}: expected the unsupported-member refusal, got \
                 {status} {body}"
            );
        }
    }

    // None of those calls ran: the rows the writes would have left are not there.
    let ids = ledger_ids(&api).await;
    assert_eq!(
        ids,
        vec![1],
        "case \"after every refusal\": a refused call runs nothing, got {ids:?}"
    );

    // The same calls without the member are served, which is what makes the refusals
    // above the MEMBER being refused and not the statements being wrong.
    let served = api
        .run(json!({"sql": "INSERT INTO ledger (id, note) VALUES (7, 'no schema')"}))
        .await;
    assert_eq!(
        number(&served, "numberOfRecordsUpdated"),
        1,
        "case \"the same statement without the member\": got {served}"
    );
}

/// The ceiling the model declares for a statement: `ExecuteStatement$sql` targets
/// `SqlStatement`, whose `@length` max is 65536
/// (`smithy_query(query="operation", operation="ExecuteStatement")`).
const SQL_CHARACTER_CEILING: usize = 65_536;

/// RULE: a statement longer than the ceiling the model declares for `sql` is refused as a
/// validation fault whose sentence names the NUMBER OF CHARACTERS a statement may carry —
/// on every operation that takes a statement — and nothing of it runs, while a statement
/// AT the ceiling is served.
///
/// The rows differ along how long the statement is: at the ceiling, one character past it,
/// and far past it; each is sent on both operations that carry a statement.
#[tokio::test]
async fn a_statement_longer_than_the_ceiling_is_refused_by_its_length() {
    let api = DataApi::new();
    given_ledger(&api).await;

    // A statement of exactly `length` characters: a read, padded out by a comment, so
    // what differs between the rows is the LENGTH and nothing else.
    let padded = |length: usize| -> String {
        const PREFIX: &str = "SELECT 1 AS one /*";
        const SUFFIX: &str = "*/";
        let fill = length.saturating_sub(PREFIX.len() + SUFFIX.len());
        format!("{PREFIX}{}{SUFFIX}", "x".repeat(fill))
    };
    let at_the_ceiling = padded(SQL_CHARACTER_CEILING);
    assert_eq!(
        at_the_ceiling.chars().count(),
        SQL_CHARACTER_CEILING,
        "case \"the statement at the ceiling\": the padded statement must be exactly at it"
    );
    let served = api.run(json!({"sql": at_the_ceiling})).await;
    assert_eq!(
        served["records"],
        json!([[{"longValue": 1}]]),
        "case \"a statement at the ceiling\": it must be served, got {served}"
    );

    let expected = format!(
        "Number of characters in SQLStatement is longer than {SQL_CHARACTER_CEILING}"
    );
    for (case, length) in [
        ("one character past the ceiling", SQL_CHARACTER_CEILING + 1),
        ("far past the ceiling", SQL_CHARACTER_CEILING * 2),
    ] {
        let too_long = padded(length);
        let calls = vec![
            (
                "ExecuteStatement",
                api.execute(json!({"sql": too_long.clone()})).await,
            ),
            (
                "BatchExecuteStatement",
                api.batch(json!({"sql": too_long.clone(), "parameterSets": [[]]}))
                    .await,
            ),
        ];
        for (operation, (status, body)) in calls {
            assert_eq!(
                (status, error_code(&body), body["message"].clone()),
                (400, "ValidationException", json!(expected)),
                "case {case:?} on {operation}: expected the character-ceiling refusal, got \
                 {status} {body}"
            );
        }
    }

    // And nothing those refusals carried ran: the ledger is as the setup left it.
    let ids = ledger_ids(&api).await;
    assert_eq!(
        ids,
        vec![1],
        "case \"after the refusals\": a refused statement runs nothing, got {ids:?}"
    );
}

/// RULE: one `BatchExecuteStatement` is ONE unit of work. A statement of it the database
/// refuses refuses the whole batch — the modelled `DatabaseErrorException` — and nothing
/// the batch had already written stays, so a caller retrying it cannot land a row twice.
///
/// The rows differ along WHERE in the batch the refused set sits (first, in the middle,
/// last) and along how many sets the batch carries.
#[tokio::test]
async fn a_batch_a_statement_of_it_fails_lands_nothing() {
    let api = DataApi::new();
    given_ledger(&api).await;

    let insert = "INSERT INTO ledger (id, note) VALUES (:id, :note)";
    let set = |id: i64| {
        json!([
            parameter("id", json!({"longValue": id})),
            parameter("note", json!({"stringValue": format!("row {id}")})),
        ])
    };
    // The key 1 is already taken, so a set carrying it is the one the database refuses.
    // (case, the sets the batch carries)
    let rows: Vec<(&str, Value)> = vec![
        ("the first set of three", json!([set(1), set(20), set(21)])),
        ("the middle set of three", json!([set(20), set(1), set(21)])),
        ("the last set of three", json!([set(20), set(21), set(1)])),
        ("the only set", json!([set(1)])),
        (
            "one set among ten",
            json!([
                set(30), set(31), set(32), set(33), set(1), set(34), set(35), set(36), set(37),
                set(38),
            ]),
        ),
    ];
    for (case, sets) in rows {
        let (status, body) = api
            .batch(json!({"sql": insert, "parameterSets": sets}))
            .await;
        assert_eq!(status, 400, "case {case}: expected HTTP 400, got {body}");
        assert_eq!(
            error_code(&body),
            "DatabaseErrorException",
            "case {case}: expected DatabaseErrorException, got {body}"
        );
        let ids = ledger_ids(&api).await;
        assert_eq!(
            ids,
            vec![1],
            "case {case}: a refused batch leaves nothing behind, got {ids:?}"
        );
    }

    // A batch every statement of which runs lands every row, which is what makes the
    // refusals above the batch being refused and not the statements failing one by one.
    let (status, served) = api
        .batch(json!({"sql": insert, "parameterSets": [set(40), set(41)]}))
        .await;
    assert_eq!(status, 200, "case a batch that runs: got {served}");
    assert_eq!(
        list(&served, "updateResults").len(),
        2,
        "case a batch that runs: expected one result per set, got {served}"
    );
    let ids = ledger_ids(&api).await;
    assert_eq!(
        ids,
        vec![1, 40, 41],
        "case a batch that runs: expected every row landed, got {ids:?}"
    );
}

/// What AWS tells a caller whose result carries a value of a type the `Field` union has no
/// member for: the fixed sentence of the modelled `UnsupportedResultException` for that
/// condition, closing with the offending type's own name in upper case.
fn unsupported_type_message(named: &str) -> String {
    format!("The result contains the unsupported data type {named}.")
}

/// RULE: a result column whose PostgreSQL type has no `Field` member to carry its values is
/// refused with `UnsupportedResultException` naming THAT TYPE — in every form the column
/// can be read in, `formatRecordsAs=JSON` included — while the same cell read through a
/// cast to a type the union DOES carry is served: the refusal is a property of the type and
/// not of the row. A value written as text is no value of such a type either, so a
/// parameter carrying one is refused by the database's assignment rule instead.
///
/// The rows are the catalog's types whose values no member of the union holds: a currency
/// amount, whose fractional digits and symbol come from the database's monetary locale, and
/// a span of time. Each is read as a stored column, beside a column that IS carried, as a
/// cast the statement writes, and in the JSON form.
#[tokio::test]
async fn a_result_of_a_type_no_field_member_carries_is_refused_by_its_type() {
    let api = DataApi::new();

    // (case, the declared type, the value the statement writes, the name the refusal names
    //  the type by, the cast that rescues the cell, what that cast answers)
    let types: &[(&str, &str, &str, &str, &str, Value)] = &[
        (
            "a currency amount",
            "money",
            "'12.34'::money",
            "MONEY",
            "numeric",
            json!([[{"stringValue": "12.34"}]]),
        ),
        (
            "a span of time",
            "interval",
            "interval '1 day'",
            "INTERVAL",
            "text",
            json!([[{"stringValue": "1 day"}]]),
        ),
    ];
    for (index, (case, declared, written, named, rescued_as, rescued)) in
        types.iter().enumerate()
    {
        let table = format!("unreturnable_{index}");
        api.run(json!({
            "sql": format!("CREATE TABLE {table} (id INT PRIMARY KEY, value {declared})"),
        }))
        .await;
        api.run(json!({
            "sql": format!("INSERT INTO {table} (id, value) VALUES (1, {written})"),
        }))
        .await;

        let expected = unsupported_type_message(named);
        let forms = [
            (
                "the stored column on its own",
                json!({"sql": format!("SELECT value FROM {table}")}),
            ),
            (
                "the stored column beside one the union carries",
                json!({
                    "sql": format!("SELECT id, value FROM {table}"),
                    "includeResultMetadata": true,
                }),
            ),
            (
                "the stored column under formatRecordsAs=JSON",
                json!({
                    "sql": format!("SELECT value FROM {table}"),
                    "formatRecordsAs": "JSON",
                }),
            ),
            (
                "a cast the statement writes",
                json!({"sql": format!("SELECT {written} AS value")}),
            ),
            (
                "that cast under formatRecordsAs=JSON",
                json!({
                    "sql": format!("SELECT {written} AS value"),
                    "formatRecordsAs": "JSON",
                }),
            ),
        ];
        for (form, members) in forms {
            let (status, body) = api.execute(members).await;
            assert_eq!(
                (status, error_code(&body), body["message"].clone()),
                (400, "UnsupportedResultException", json!(expected)),
                "case {case} as {form}: expected the unsupported-type refusal, got {status} \
                 {body}"
            );
        }

        // The same cell, cast to a type the union carries, is served — which is what makes
        // the refusals above the TYPE's and not the row's.
        let served = api
            .run(json!({
                "sql": format!("SELECT value::{rescued_as} AS value FROM {table}"),
            }))
            .await;
        assert_eq!(
            &served["records"], rescued,
            "case {case} cast to {rescued_as}: expected {rescued}, got {served}"
        );

        // And a value written as TEXT is no value of such a type: the database refuses the
        // assignment rather than storing it, so the column cannot be filled around the
        // refusal above.
        let (status, body) = api
            .execute(json!({
                "sql": format!("INSERT INTO {table} (id, value) VALUES (2, :value)"),
                "parameters": [parameter("value", json!({"stringValue": "12.34"}))],
            }))
            .await;
        assert_eq!(
            (status, error_code(&body)),
            (400, "DatabaseErrorException"),
            "case {case} assigned a value written as text: expected the database's refusal, \
             got {status} {body}"
        );
        assert_eq!(
            body["message"],
            json!(format!(
                "ERROR: column \"value\" is of type {declared} but expression is of type text; \
                 Hint: You will need to rewrite or cast the expression.; Position: {}; \
                 SQLState: 42804",
                format!("INSERT INTO {table} (id, value) VALUES (2, ").len() + 1
            )),
            "case {case} assigned a value written as text: expected the assignment \
             diagnostic, got {body}"
        );
    }

    // The OTHER unsupported-result class keeps its own sentence: a floating value that is
    // not a number is refused for the VALUE it is, not for its type, so the two sentences
    // stay apart.
    let (status, body) = api
        .execute(json!({"sql": "SELECT 'inf'::float8 AS inf"}))
        .await;
    assert_eq!(
        (status, error_code(&body), body["message"].clone()),
        (400, "UnsupportedResultException", json!(NON_FINITE_MESSAGE)),
        "case \"a non-finite float\": expected the non-finite sentence, got {status} {body}"
    );
    assert_ne!(
        body["message"],
        json!(unsupported_type_message("DOUBLE PRECISION")),
        "case \"a non-finite float\": the two unsupported-result sentences must stay apart, \
         got {body}"
    );
}

/// RULE: a `stringValue` its `typeHint` cannot read as a value of the type that hint names
/// is refused HTTP 400 `DatabaseErrorException` with the sentence of THAT HINT's OWN class
/// — one sentence per hint and not one sentence generalised over the six — and the same
/// sentence whichever statement operation carried the parameter.
///
/// The three classes, each its own wording:
///
/// * `UUID` is read by the API itself, and its sentence QUOTES the string it could not
///   read back, in double quotes, carrying neither the `ERROR:` prefix nor the `SQLState:`
///   part an error the database raised is rendered with — which is what tells this
///   refusal, made on the parameter before the statement runs, from an engine-raised one;
/// * `JSON` carries the document type's own input-syntax error, which is what its PARSER
///   said was wrong with the string;
/// * `DATE`, `TIME`, `TIMESTAMP` and `DECIMAL` carry the fixed parse-error sentence of the
///   type, which names the type and the fault its reader reports and carries nothing of
///   the request.
///
/// The hints and their accepted forms come from the model
/// (`smithy_query(query="shape", shape="SqlParameter")`, member `typeHint`; `TypeHint`
/// lists exactly those six values). The rows differ along the hint, and within the `UUID`
/// hint along WHAT is wrong with the string — nothing of the form at all, too few groups,
/// non-hexadecimal digits, a value with something after it, and no string at all — so what
/// is pinned is the template and not one value.
#[tokio::test]
async fn an_unreadable_hinted_value_is_refused_in_the_sentence_of_its_hint() {
    let api = DataApi::new();
    api.run(json!({"sql": "CREATE TABLE hinted_refusals (value TEXT)"}))
        .await;
    // The hinted value reaches the character column through a CAST, because the hint names
    // the type the string is SENT AS; what this test is about is which sentence refuses a
    // string the hint cannot read, which is decided before any column is reached.
    const INSERT_HINTED: &str =
        "INSERT INTO hinted_refusals (value) VALUES (CAST(:value AS TEXT))";

    // (the hint, the string it cannot read, the sentence the refusal carries, whether that
    // sentence quotes the string back)
    let rows: Vec<(&str, &str, String, bool)> = vec![
        (
            "UUID",
            "nope-not-a-uuid",
            "Cannot parse UUID parameter: \"nope-not-a-uuid\"".to_string(),
            true,
        ),
        (
            "UUID",
            "4bcd3f0e-7a25-4f8c-9c3a",
            "Cannot parse UUID parameter: \"4bcd3f0e-7a25-4f8c-9c3a\"".to_string(),
            true,
        ),
        (
            "UUID",
            "zzzzzzzz-7a25-4f8c-9c3a-1b2c3d4e5f60",
            "Cannot parse UUID parameter: \"zzzzzzzz-7a25-4f8c-9c3a-1b2c3d4e5f60\"".to_string(),
            true,
        ),
        (
            "UUID",
            "4bcd3f0e-7a25-4f8c-9c3a-1b2c3d4e5f60 and more",
            "Cannot parse UUID parameter: \"4bcd3f0e-7a25-4f8c-9c3a-1b2c3d4e5f60 and more\""
                .to_string(),
            true,
        ),
        (
            "UUID",
            "",
            "Cannot parse UUID parameter: \"\"".to_string(),
            true,
        ),
        (
            "JSON",
            "{bad json}",
            "invalid input syntax for type json: key must be a string at line 1 column 2"
                .to_string(),
            false,
        ),
        (
            "DATE",
            "nope-not-a-date",
            "Parse Error for Date: input contains invalid characters".to_string(),
            false,
        ),
        (
            "TIME",
            "nope-not-a-time",
            "Parse Error for Time: input contains invalid characters".to_string(),
            false,
        ),
        (
            "TIMESTAMP",
            "nope-not-a-timestamp",
            "Parse Error for Timestamp: input contains invalid characters".to_string(),
            false,
        ),
        (
            "DECIMAL",
            "nope-not-a-number",
            "Parse Error for Decimal: input contains invalid characters".to_string(),
            false,
        ),
    ];
    for (hint, value, message, quotes_the_value) in rows {
        let sent = json!({"name": "value", "value": {"stringValue": value}, "typeHint": hint});
        for (path, members) in [
            (
                "ExecuteStatement",
                json!({"sql": INSERT_HINTED, "parameters": [sent.clone()]}),
            ),
            (
                "BatchExecuteStatement",
                json!({"sql": INSERT_HINTED, "parameterSets": [[sent.clone()]]}),
            ),
        ] {
            let (status, body) = if path == "ExecuteStatement" {
                api.execute(members).await
            } else {
                api.batch(members).await
            };
            assert_eq!(
                (status, error_code(&body), body["message"].clone()),
                (400, "DatabaseErrorException", json!(message)),
                "case {hint} refusing {value:?} on {path}: expected HTTP 400 \
                 DatabaseErrorException and {message:?}, got {status} {body}"
            );
            let answered = body["message"].as_str().unwrap_or_default().to_string();
            assert!(
                !answered.starts_with("ERROR:") && !answered.contains("SQLState:"),
                "case {hint} refusing {value:?} on {path}: the API refused the parameter, so \
                 the sentence carries neither the prefix nor the SQLSTATE of an \
                 engine-raised diagnostic, got {answered:?}"
            );
            assert_eq!(
                answered.contains(value),
                quotes_the_value,
                "case {hint} refusing {value:?} on {path}: expected the string quoted back \
                 to be {quotes_the_value}, got {answered:?}"
            );
        }
    }

    // None of those refusals ran its statement.
    let stored = api
        .run(json!({"sql": "SELECT value FROM hinted_refusals"}))
        .await;
    assert!(
        list(&stored, "records").is_empty(),
        "case \"after every refusal\": expected no row written, got {stored}"
    );

    // And a string the hint CAN read is served through the same path, on both operations:
    // what the refusals above are about is the string, not the hint being present.
    for (path, accepted) in [
        ("ExecuteStatement", "4bcd3f0e-7a25-4f8c-9c3a-1b2c3d4e5f60"),
        ("BatchExecuteStatement", "0e1d2c3b-4a59-4687-8765-43210fedcba9"),
    ] {
        let sent = json!({"name": "value", "value": {"stringValue": accepted}, "typeHint": "UUID"});
        let (status, body) = if path == "ExecuteStatement" {
            api.execute(json!({"sql": INSERT_HINTED, "parameters": [sent]}))
                .await
        } else {
            api.batch(json!({"sql": INSERT_HINTED, "parameterSets": [[sent]]}))
                .await
        };
        assert_eq!(
            status, 200,
            "case a readable UUID on {path}: expected the statement to run, got {status} {body}"
        );
    }
    let stored = api
        .run(json!({"sql": "SELECT value FROM hinted_refusals ORDER BY value"}))
        .await;
    assert_eq!(
        stored["records"],
        json!([
            [{"stringValue": "0e1d2c3b-4a59-4687-8765-43210fedcba9"}],
            [{"stringValue": "4bcd3f0e-7a25-4f8c-9c3a-1b2c3d4e5f60"}],
        ]),
        "case \"the readable UUIDs\": expected both rows stored as they were sent, got {stored}"
    );
}
