// SPDX-License-Identifier: CC0-1.0

//! The statements the Data API's OWN ENGINES spell, the DIAGNOSTICS each of those engines
//! words an error with, and the one result a batch of no parameter sets answers with, over
//! the real wire.
//!
//! Every call goes through the router, so routing, restJson1 deserialization, the
//! handler and serialization are all exercised; the client harness is the one
//! `sql_data_api` builds — except where a case needs a cluster whose ENGINE is a real fact
//! of the Amazon RDS control plane, which is the emulator `call_preconditions` assembles.

use serde_json::{json, Value};

use crate::call_preconditions::{Cloud, DATABASE};
use crate::sql_data_api::{error_code, list, number, parameter, DataApi};

/// RULE: the Data API runs a statement written in the dialect of either engine it
/// serves, so an IDENTITY column is declared in that engine's own spelling — MySQL's
/// `AUTO_INCREMENT` column attribute or PostgreSQL's `serial` family — and either way
/// the engine assigns the column's value when an INSERT leaves it out. The value the
/// engine generated for MySQL's `AUTO_INCREMENT` key is what `generatedFields` carries
/// (one entry per execution, so one per `parameterSets` entry of a batch), while
/// PostgreSQL's is read through a RETURNING clause, because
/// `ExecuteStatementResponse.generatedFields`'s own documentation says the data "isn't
/// supported by Aurora PostgreSQL. To get the values of generated fields, use the
/// RETURNING clause".
///
/// The rows span the spellings of the rule and the widths each dialect declares an
/// identity column at; the keys asserted are the engine's own, read back through a
/// SELECT as well as off the response.
#[tokio::test]
async fn an_identity_column_is_declared_in_either_engines_spelling() {
    // (case, the column's declaration, whether the engine's generated key is the one
    //  `generatedFields` reports)
    let cases = [
        ("MySQL's integer identity key", "INT AUTO_INCREMENT", true),
        (
            "MySQL's identity key, spelled with the full type name",
            "INTEGER AUTO_INCREMENT",
            true,
        ),
        ("MySQL's wide identity key", "BIGINT AUTO_INCREMENT", true),
        (
            "MySQL's narrow identity key",
            "SMALLINT AUTO_INCREMENT",
            true,
        ),
        ("MySQL's attribute in lower case", "int auto_increment", true),
        ("PostgreSQL's sequence column", "serial", false),
        ("PostgreSQL's wide sequence column", "bigserial", false),
        ("PostgreSQL's narrow sequence column", "smallserial", false),
    ];

    for (index, (case, declared, reports_the_key)) in cases.into_iter().enumerate() {
        let api = DataApi::new();
        let table = format!("identity_{index}");
        let (status, created) = api
            .execute(json!({
                "sql": format!(
                    "CREATE TABLE {table} (id {declared} PRIMARY KEY, name VARCHAR(32))"
                ),
            }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the declaration must be accepted, got {status} {created}"
        );

        // The first row of a fresh table takes the first value of the sequence, and
        // each later row the next one.
        for expected_key in 1..=3_i64 {
            let inserted = api
                .run(json!({
                    "sql": format!("INSERT INTO {table} (name) VALUES (:name)"),
                    "parameters": [parameter(
                        "name",
                        json!({"stringValue": format!("row {expected_key}")}),
                    )],
                }))
                .await;
            let expected_fields = if reports_the_key {
                json!([{"longValue": expected_key}])
            } else {
                json!([])
            };
            assert_eq!(
                json!(list(&inserted, "generatedFields")),
                expected_fields,
                "case {case:?} at key {expected_key}: expected generatedFields \
                 {expected_fields}, got {inserted}"
            );
            assert_eq!(
                number(&inserted, "numberOfRecordsUpdated"),
                1,
                "case {case:?} at key {expected_key}: one row was stored, got {inserted}"
            );
        }

        // A batch runs the statement once per parameter set, so each set gets its own
        // generated key: distinct, and increasing with the sets.
        let (status, batch) = api
            .batch(json!({
                "sql": format!("INSERT INTO {table} (name) VALUES (:name)"),
                "parameterSets": [
                    [parameter("name", json!({"stringValue": "row 4"}))],
                    [parameter("name", json!({"stringValue": "row 5"}))],
                ],
            }))
            .await;
        assert_eq!(status, 200, "case {case:?}: the batch failed with {batch}");
        let batched: Vec<Value> = list(&batch, "updateResults")
            .iter()
            .map(|result| json!(list(result, "generatedFields")))
            .collect();
        let expected_batch = if reports_the_key {
            vec![json!([{"longValue": 4}]), json!([{"longValue": 5}])]
        } else {
            vec![json!([]), json!([])]
        };
        assert_eq!(
            batched, expected_batch,
            "case {case:?}: expected one generatedFields per parameter set \
             ({expected_batch:?}), got {batch}"
        );

        // And the keys the engine really assigned, read back through the engine.
        let stored = api
            .run(json!({
                "sql": format!("SELECT id FROM {table} ORDER BY id"),
                "includeResultMetadata": true,
            }))
            .await;
        assert_eq!(
            stored["records"],
            json!([
                [{"longValue": 1}],
                [{"longValue": 2}],
                [{"longValue": 3}],
                [{"longValue": 4}],
                [{"longValue": 5}],
            ]),
            "case {case:?}: every row took the next value of the sequence, got {stored}"
        );
        assert_eq!(
            list(&stored, "columnMetadata")[0]["isAutoIncrement"],
            json!(true),
            "case {case:?}: the column's value is assigned by the engine, got {stored}"
        );
    }
}

/// RULE: `ColumnMetadata.isAutoIncrement` ("A value that indicates whether the column
/// increments automatically", `Boolean` with the modelled default `false`) describes one
/// result column by the DECLARATION backing it: it is true only where that declaration
/// carries an identity property in the dialect it was written in — PostgreSQL's `serial`
/// family or MySQL's `AUTO_INCREMENT` attribute, the two spellings that give a column a
/// sequence the engine draws its value from — and false for every other result column,
/// whatever its type, its position in the projection or its being its table's key, a
/// `PRIMARY KEY` being no sequence at all.
///
/// The rows differ along the declaration: each spelling of an identity column and each
/// width it is declared at, against plain keys of the integer, character and
/// floating-point types, and the flag is read both under the column's own name and under
/// an alias, because an alias renames a result column without detaching it from the
/// column it reads.
#[tokio::test]
async fn a_result_columns_auto_increment_flag_comes_from_its_declaration() {
    // (case, the key column's declaration, whether the declaration carries a sequence)
    let declarations = [
        ("PostgreSQL's sequence column", "serial", true),
        ("PostgreSQL's wide sequence column", "bigserial", true),
        ("PostgreSQL's narrow sequence column", "smallserial", true),
        ("MySQL's identity attribute", "INT AUTO_INCREMENT", true),
        (
            "MySQL's identity attribute in lower case",
            "bigint auto_increment",
            true,
        ),
        ("a plain integer key", "integer", false),
        ("a plain integer key, abbreviated", "INT", false),
        ("a plain integer key, by catalog name", "int4", false),
        ("a wide integer key", "bigint", false),
        ("a narrow integer key", "smallint", false),
        ("a character key", "varchar(16)", false),
        ("a text key", "text", false),
        ("a floating-point key", "double precision", false),
    ];

    for (index, (case, declared, carries_a_sequence)) in declarations.into_iter().enumerate() {
        let api = DataApi::new();
        let table = format!("flagged_{index}");
        let (status, created) = api
            .execute(json!({
                "sql": format!("CREATE TABLE {table} (id {declared} PRIMARY KEY, label text)"),
            }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the declaration must be accepted, got {status} {created}"
        );

        for projection in ["id", "id AS pk"] {
            let described = api
                .run(json!({
                    "sql": format!("SELECT {projection} FROM {table}"),
                    "includeResultMetadata": true,
                }))
                .await;
            let entry = list(&described, "columnMetadata")
                .first()
                .cloned()
                .unwrap_or(Value::Null);
            assert_eq!(
                entry["isAutoIncrement"],
                json!(carries_a_sequence),
                "case {case:?} projected as {projection:?}: expected isAutoIncrement \
                 {carries_a_sequence}, got {entry}"
            );
        }

        // The column beside it declares no sequence in any of these tables.
        let described = api
            .run(json!({
                "sql": format!("SELECT label FROM {table}"),
                "includeResultMetadata": true,
            }))
            .await;
        let entry = list(&described, "columnMetadata")
            .first()
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            entry["isAutoIncrement"],
            json!(false),
            "case {case:?}, the column beside the key: expected isAutoIncrement false, \
             got {entry}"
        );
    }

    // WHERE the column sits decides nothing: a sequence column that is not its table's
    // key reports the flag, and the key in front of it does not.
    let api = DataApi::new();
    api.run(json!({
        "sql": "CREATE TABLE tickets (label text PRIMARY KEY, id bigserial)",
    }))
    .await;
    let described = api
        .run(json!({
            "sql": "SELECT label, id FROM tickets",
            "includeResultMetadata": true,
        }))
        .await;
    let metadata = list(&described, "columnMetadata");
    for (position, (case, expected)) in [
        ("the text key, in front of the sequence column", false),
        ("the sequence column, behind the key", true),
    ]
    .into_iter()
    .enumerate()
    {
        let entry = metadata.get(position).cloned().unwrap_or(Value::Null);
        assert_eq!(
            entry["isAutoIncrement"],
            json!(expected),
            "case {case:?}: expected isAutoIncrement {expected}, got {entry}"
        );
    }

    // And a column no table declares — one the statement computes — carries the modelled
    // default, even where the value it computes is read FROM a sequence column.
    let api = DataApi::new();
    api.run(json!({"sql": "CREATE TABLE notes (id serial PRIMARY KEY, body text)"}))
        .await;
    api.run(json!({"sql": "INSERT INTO notes (body) VALUES ('first')"}))
        .await;
    for (case, projection) in [
        ("an expression over the sequence column", "id + 1 AS next_id"),
        ("an aggregate over it", "max(id) AS highest"),
        ("a count of the rows", "count(*) AS total"),
        ("a literal the statement spells", "7 AS seven"),
    ] {
        let described = api
            .run(json!({
                "sql": format!("SELECT {projection} FROM notes"),
                "includeResultMetadata": true,
            }))
            .await;
        let entry = list(&described, "columnMetadata")
            .first()
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            entry["isAutoIncrement"],
            json!(false),
            "case {case:?}: a computed column has no declaration to carry a sequence, \
             expected isAutoIncrement false, got {entry}"
        );
    }
}

/// RULE: a statement about the cluster's DATABASES is applied to the cluster, so
/// `CREATE DATABASE <name>` makes a database the cluster HAS — one a later call selects
/// with its `database` member, whose tables are its own and invisible to every other
/// database — creating one that is already there is the database's duplicate-database
/// error unless the statement says `IF NOT EXISTS`, and `DROP DATABASE` takes it away
/// again. A call quoting a `transactionId` is served only while that transaction is
/// RUNNING, whatever its statement does.
///
/// The rows differ along the name's form (how the statement writes it) and along which
/// of the two statements is run; an unquoted name is folded to lower case, as
/// PostgreSQL folds every unquoted identifier.
#[tokio::test]
async fn a_statement_can_create_and_drop_a_database_of_the_cluster() {
    // (case, the name the statement writes, the name the `database` member selects)
    let cases = [
        ("a lower-case name", "shop", "shop"),
        ("a name with digits", "shop2", "shop2"),
        ("an unquoted name, which folds", "Shop_Three", "shop_three"),
        ("a delimited name, which does not", "\"ShopFour\"", "ShopFour"),
    ];

    for (case, written, selected) in cases {
        let api = DataApi::new();
        let created = api
            .run(json!({"sql": format!("CREATE DATABASE {written}")}))
            .await;
        assert_eq!(
            number(&created, "numberOfRecordsUpdated"),
            0,
            "case {case:?}: a statement that creates a database changes no rows, \
             got {created}"
        );

        // The database is one the cluster has, so a call may select it — and what is
        // created in it is its own.
        api.run(json!({
            "sql": "CREATE TABLE orders (id INT)",
            "database": selected,
        }))
        .await;
        api.run(json!({
            "sql": "INSERT INTO orders (id) VALUES (7)",
            "database": selected,
        }))
        .await;
        let inside = api
            .run(json!({"sql": "SELECT id FROM orders", "database": selected}))
            .await;
        assert_eq!(
            inside["records"],
            json!([[{"longValue": 7}]]),
            "case {case:?}: the row is in the database that was created, got {inside}"
        );

        // The connection's own database never saw that table.
        let (status, outside) = api.execute(json!({"sql": "SELECT id FROM orders"})).await;
        assert_eq!(
            (status, error_code(&outside)),
            (400, "DatabaseErrorException"),
            "case {case:?}: another database does not hold that table, got {status} \
             {outside}"
        );

        // Creating it again is the duplicate-database condition, in the database's own
        // words, and `IF NOT EXISTS` is what asks for it not to be.
        let (status, again) = api
            .execute(json!({"sql": format!("CREATE DATABASE {written}")}))
            .await;
        assert_eq!(
            (status, error_code(&again)),
            (400, "DatabaseErrorException"),
            "case {case:?}: a database that is already there, got {status} {again}"
        );
        assert_eq!(
            again["message"],
            json!(format!(
                "ERROR: database \"{selected}\" already exists; SQLState: 42P04"
            )),
            "case {case:?}: got {again}"
        );
        let tolerated = api
            .run(json!({"sql": format!("CREATE DATABASE IF NOT EXISTS {written}")}))
            .await;
        assert_eq!(
            number(&tolerated, "numberOfRecordsUpdated"),
            0,
            "case {case:?}: IF NOT EXISTS leaves the database as it is, got {tolerated}"
        );

        // And dropping it takes the database away, with everything it held: a call
        // naming it reaches a database that holds no such table.
        api.run(json!({"sql": format!("DROP DATABASE {written}")}))
            .await;
        let (status, gone) = api
            .execute(json!({"sql": "SELECT id FROM orders", "database": selected}))
            .await;
        assert_eq!(
            (status, error_code(&gone)),
            (400, "DatabaseErrorException"),
            "case {case:?}: the dropped database holds nothing, got {status} {gone}"
        );
        assert_eq!(
            gone["message"],
            json!("ERROR: relation \"orders\" does not exist; SQLState: 42P01"),
            "case {case:?}: got {gone}"
        );
    }

    // Dropping a database the cluster does not have is the database's own condition
    // for it, unless the statement says `IF EXISTS`.
    let api = DataApi::new();    let (status, absent) = api
        .execute(json!({"sql": "DROP DATABASE never_created"}))
        .await;
    assert_eq!(
        (status, error_code(&absent)),
        (400, "DatabaseErrorException"),
        "case \"a database that was never created\": got {status} {absent}"
    );
    assert_eq!(
        absent["message"],
        json!("ERROR: database \"never_created\" does not exist; SQLState: 3D000"),
        "case \"a database that was never created\": got {absent}"
    );
    let tolerated = api
        .run(json!({"sql": "DROP DATABASE IF EXISTS never_created"}))
        .await;
    assert_eq!(
        number(&tolerated, "numberOfRecordsUpdated"),
        0,
        "case \"IF EXISTS on a database that was never created\": got {tolerated}"
    );

    // A call quoting a transaction that is no longer RUNNING is refused whatever its
    // statement does, so a statement about the cluster's databases is refused too \u2014 and
    // the database it names is not created.
    let transaction = api.begin_transaction(None).await;
    let (status, committed) = api.commit(&transaction).await;
    assert_eq!(status, 200, "committing failed with {committed}");
    for (operation, (status, body)) in [
        (
            "ExecuteStatement",
            api.execute(json!({
                "sql": "CREATE DATABASE after_the_commit",
                "transactionId": transaction.clone(),
            }))
            .await,
        ),
        (
            "BatchExecuteStatement",
            api.batch(json!({
                "sql": "CREATE DATABASE after_the_commit",
                "transactionId": transaction.clone(),
            }))
            .await,
        ),
    ] {
        assert_eq!(
            (status, error_code(&body)),
            (404, "TransactionNotFoundException"),
            "case \"a database statement in a transaction that ended\" on {operation}: \
             got {status} {body}"
        );
    }
    let (status, absent) = api
        .execute(json!({"sql": "DROP DATABASE after_the_commit"}))
        .await;
    assert_eq!(
        (status, error_code(&absent)),
        (400, "DatabaseErrorException"),
        "case \"a database statement in a transaction that ended\": the refused call \
         created nothing, got {status} {absent}"
    );
}

/// RULE: `BatchExecuteStatement`'s `parameterSets` is optional, and a call that carries
/// none still RUNS the statement: it binds nothing, so there is exactly one execution
/// of it and the response carries exactly one `UpdateResult` — whose `generatedFields`
/// is empty for a statement that generates nothing — and no other member, because
/// `numberOfRecordsUpdated`, `records` and `formattedRecords` belong to
/// `ExecuteStatement`.
///
/// The rows differ along how the call carries no sets (the member omitted, and an empty
/// list) and along the statement's effect, so what is asserted is one execution of
/// whatever was sent and not one shape of statement.
#[tokio::test]
async fn a_batch_of_no_parameter_sets_runs_the_statement_once() {
    // (case, the statement, the rows the table holds afterwards)
    let statements = [
        (
            "an insert of its own values",
            "INSERT INTO stock (id, label) VALUES (3, 'three')",
            json!([
                [{"longValue": 1}, {"stringValue": "one"}],
                [{"longValue": 2}, {"stringValue": "two"}],
                [{"longValue": 3}, {"stringValue": "three"}],
            ]),
        ),
        (
            "an update",
            "UPDATE stock SET label = 'same'",
            json!([
                [{"longValue": 1}, {"stringValue": "same"}],
                [{"longValue": 2}, {"stringValue": "same"}],
            ]),
        ),
        (
            "a delete",
            "DELETE FROM stock WHERE id = 1",
            json!([[{"longValue": 2}, {"stringValue": "two"}]]),
        ),
    ];

    for (case, sql, remaining) in statements {
        for sets in ["omitted", "an empty list"] {
            let api = DataApi::new();
            api.run(json!({"sql": "CREATE TABLE stock (id INT, label VARCHAR(8))"}))
                .await;
            api.run(json!({
                "sql": "INSERT INTO stock (id, label) VALUES (1, 'one'), (2, 'two')",
            }))
            .await;

            let mut members = json!({"sql": sql});
            if sets == "an empty list" {
                members["parameterSets"] = json!([]);
            }
            let (status, body) = api.batch(members).await;
            assert_eq!(
                status, 200,
                "case {case:?} with parameterSets {sets}: got {status} {body}"
            );
            assert_eq!(
                body,
                json!({"updateResults": [{"generatedFields": []}]}),
                "case {case:?} with parameterSets {sets}: expected exactly one empty \
                 UpdateResult and no other member, got {body}"
            );

            let after = api
                .run(json!({"sql": "SELECT id, label FROM stock ORDER BY id"}))
                .await;
            assert_eq!(
                after["records"], remaining,
                "case {case:?} with parameterSets {sets}: the statement ran once, \
                 expected {remaining}, got {after}"
            );
        }
    }
}

/// One statement issued inside a transaction, and what the engines whose data definition
/// is not transactional do with the work the transaction had done before it.
struct Definition {
    case: &'static str,
    /// The statement under test, issued inside the transaction.
    sql: &'static str,
    /// Whether it is a statement that COMMITS the transaction on the MySQL family: every
    /// statement that defines or modifies a stored object does, and a statement about a
    /// TEMPORARY object — the exception MySQL's own list of implicitly committing
    /// statements names — does not, nor does a statement that only changes rows.
    commits_on_mysql: bool,
    /// A statement that is served only where the definition itself took effect, read
    /// after the transaction has been rolled back; `None` for a definition no later
    /// statement can ask about.
    proof: Option<&'static str>,
}

/// RULE: a statement that DEFINES OR MODIFIES a stored object cannot be part of a
/// transaction on the MySQL-family engines — it commits the transaction it is issued in,
/// so everything the transaction had written before it is durable and a later
/// `RollbackTransaction` has none of that work left to discard, while the transaction goes
/// on and whatever it writes AFTER the definition is still rolled back. PostgreSQL's data
/// definition IS transactional, so on an Aurora PostgreSQL cluster the same sequence
/// rolls back whole — which engine the addressed cluster runs is the Amazon RDS control
/// plane's own fact.
///
/// The rows differ along the statement issued inside the transaction (a table created, a
/// column added, a table dropped, an index created), and along the two kinds that do NOT
/// commit it: a TEMPORARY object, which MySQL's own list of implicitly committing
/// statements excepts, and a statement that only changes rows.
#[tokio::test]
async fn a_data_definition_inside_a_transaction_commits_it_on_the_mysql_family() {
    let definitions = &[
        Definition {
            case: "a table created",
            sql: "CREATE TABLE created_inside (id INT)",
            commits_on_mysql: true,
            proof: Some("SELECT id FROM created_inside"),
        },
        Definition {
            case: "a column added",
            sql: "ALTER TABLE ledger ADD COLUMN note VARCHAR(16)",
            commits_on_mysql: true,
            proof: Some("SELECT note FROM ledger"),
        },
        Definition {
            case: "a table dropped",
            sql: "DROP TABLE spare",
            commits_on_mysql: true,
            proof: None,
        },
        Definition {
            case: "an index created",
            sql: "CREATE INDEX ledger_label ON ledger (label)",
            commits_on_mysql: true,
            proof: None,
        },
        Definition {
            case: "a TEMPORARY table created",
            sql: "CREATE TEMPORARY TABLE scratch (id INT)",
            commits_on_mysql: false,
            proof: None,
        },
        Definition {
            case: "a statement that only changes rows",
            sql: "INSERT INTO ledger (id, label) VALUES (3, 'dml')",
            commits_on_mysql: false,
            proof: None,
        },
    ];

    for (index, definition) in definitions.iter().enumerate() {
        for engine in ["aurora-mysql", "aurora-postgresql"] {
            let committed = definition.commits_on_mysql && engine == "aurora-mysql";
            let cloud = Cloud::new();
            let (resource_arn, secret_arn) = cloud
                .create_cluster_running(&format!("emu-ddl-{index}-{engine}"), engine, true)
                .await;
            let call = |members: Value| {
                let mut members = members;
                members["resourceArn"] = json!(resource_arn);
                members["secretArn"] = json!(secret_arn);
                members["database"] = json!(DATABASE);
                members
            };
            for ddl in [
                "CREATE TABLE ledger (id INT, label VARCHAR(16))",
                "CREATE TABLE spare (id INT)",
            ] {
                cloud.run(call(json!({"sql": ddl}))).await;
            }

            let (status, begun) = cloud
                .call("/BeginTransaction", call(json!({})))
                .await;
            assert_eq!(
                status, 200,
                "case {:?} on {engine}: BeginTransaction failed with {begun}",
                definition.case
            );
            let transaction_id = begun["transactionId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?} on {engine}: BeginTransaction returns an id: {begun}",
                        definition.case
                    )
                })
                .to_string();

            // The work the transaction does BEFORE the statement under test, the statement
            // itself, and work it does AFTER it — which a rollback discards on either
            // engine, because the transaction goes on.
            for sql in [
                "INSERT INTO ledger (id, label) VALUES (1, 'before')",
                definition.sql,
                "INSERT INTO ledger (id, label) VALUES (2, 'after')",
            ] {
                let served = cloud
                    .run(call(json!({"sql": sql, "transactionId": transaction_id})))
                    .await;
                assert!(
                    served.get("message").is_none(),
                    "case {:?} on {engine}: {sql} must be served, got {served}",
                    definition.case
                );
            }

            // What a reader OUTSIDE the transaction sees once the definition has run: the
            // work the definition committed, and nothing else — the row written after it
            // is still the transaction's own.
            let expected = if committed {
                json!([[{"stringValue": "before"}]])
            } else {
                json!([])
            };
            let outside = cloud
                .run(call(json!({"sql": "SELECT label FROM ledger ORDER BY id"})))
                .await;
            assert_eq!(
                outside["records"],
                expected,
                "case {:?} on {engine}: expected {expected} committed by the statement, \
                 got {outside}",
                definition.case
            );

            let (status, rolled_back) = cloud
                .call(
                    "/RollbackTransaction",
                    call(json!({"transactionId": transaction_id})),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {:?} on {engine}: RollbackTransaction failed with {rolled_back}",
                definition.case
            );

            // And what survives the rollback is exactly what the definition had committed.
            let after = cloud
                .run(call(json!({"sql": "SELECT label FROM ledger ORDER BY id"})))
                .await;
            assert_eq!(
                after["records"],
                expected,
                "case {:?} on {engine}: expected {expected} to survive the rollback, \
                 got {after}",
                definition.case
            );

            // The definition itself is durable too where the engine commits it, and goes
            // away with the transaction where it does not.
            if let Some(proof) = definition.proof {
                let (status, body) = cloud.execute(call(json!({"sql": proof}))).await;
                let expected_status = if committed { 200 } else { 400 };
                assert_eq!(
                    status, expected_status,
                    "case {:?} on {engine}: expected HTTP {expected_status} for {proof}, \
                     got {status} {body}",
                    definition.case
                );
            }
        }
    }
}

/// One condition the database raises, with the statement that raises it and the sentence
/// each engine words it with.
struct Diagnostic {
    case: &'static str,
    sql: &'static str,
    /// What an Aurora MySQL cluster answers: MySQL's own sentence, its error number and
    /// its SQLSTATE, from its "Server Error Message Reference".
    on_mysql: &'static str,
    /// And what an Aurora PostgreSQL one answers: PostgreSQL's own sentence and the
    /// SQLSTATE from its "Error Codes" appendix.
    on_postgresql: &'static str,
}

/// RULE: an error the DATABASE raised while running a statement reaches the caller in the
/// words of the ENGINE THE ADDRESSED CLUSTER RUNS — that engine's own sentence for the
/// condition, its own vendor error number where it has one, and its own SQLSTATE, which is
/// what a client branches, retries and logs on — and the names and values each sentence
/// carries (the constraint, the index, the relation, the column, the colliding value) are
/// the ones the schema and the request really hold. So one and the same statement, refused
/// by one and the same condition, is worded one way on an Aurora MySQL cluster and another
/// on an Aurora PostgreSQL one, while the modelled CLASS of the refusal
/// (`DatabaseErrorException`) is the same on both.
///
/// The rows differ along the condition the database raised (a value the primary key already
/// holds, a value a unique key over another column holds, a NULL in a column that admits
/// none, a relation that is not there, a column nothing declares) and along the engine the
/// cluster runs. The duplicate-key rows differ further along where the colliding value
/// comes from — the statement's own literal, a parameter of the request, and a statement
/// that settles no single value for the key at all, which each engine words its own way.
#[tokio::test]
async fn an_error_the_database_raised_is_worded_as_the_clusters_engine_words_it() {
    // (the engine the cluster runs, the identifier it is created under)
    let engines = [
        ("aurora-mysql", "emu-dialect-mysql"),
        ("aurora-postgresql", "emu-dialect-postgresql"),
    ];
    let conditions: &[Diagnostic] = &[
        Diagnostic {
            case: "a value the primary key already holds, written by the statement",
            sql: "INSERT INTO ledger (id, label, note) VALUES (1, 'other', 'again')",
            on_mysql: "Duplicate entry '1' for key 'ledger.PRIMARY'; Error code: 1062; \
                       SQLState: 23000",
            on_postgresql: "ERROR: duplicate key value violates unique constraint \
                            \"ledger_pkey\"; SQLState: 23505",
        },
        Diagnostic {
            case: "a value a unique key over another column already holds",
            sql: "INSERT INTO ledger (id, label, note) VALUES (2, 'taken', 'again')",
            on_mysql: "Duplicate entry 'taken' for key 'ledger.label'; Error code: 1062; \
                       SQLState: 23000",
            on_postgresql: "ERROR: duplicate key value violates unique constraint \
                            \"ledger_label_key\"; SQLState: 23505",
        },
        Diagnostic {
            case: "a duplicate key the statement settles no single value for",
            sql: "INSERT INTO ledger (id, label, note) SELECT id, label, note FROM ledger",
            on_mysql: "Can't write; duplicate key in table 'ledger'; Error code: 1022; \
                       SQLState: 23000",
            on_postgresql: "ERROR: duplicate key value violates unique constraint \
                            \"ledger_label_key\"; SQLState: 23505",
        },
        Diagnostic {
            case: "a NULL in a column that admits none",
            sql: "INSERT INTO ledger (id, label, note) VALUES (3, 'three', NULL)",
            on_mysql: "Column 'note' cannot be null; Error code: 1048; SQLState: 23000",
            on_postgresql: "ERROR: null value in column \"note\" of relation \"ledger\" \
                            violates not-null constraint; SQLState: 23502",
        },
        Diagnostic {
            case: "a relation the database does not hold",
            sql: "INSERT INTO absent (id) VALUES (4)",
            on_mysql: "Table 'appdb.absent' doesn't exist; Error code: 1146; SQLState: 42S02",
            on_postgresql: "ERROR: relation \"absent\" does not exist; SQLState: 42P01",
        },
        Diagnostic {
            case: "a column nothing declares, in a query",
            sql: "SELECT missing FROM ledger",
            on_mysql: "Unknown column 'missing' in 'field list'; Error code: 1054; \
                       SQLState: 42S22",
            on_postgresql: "ERROR: column \"missing\" does not exist; SQLState: 42703",
        },
        Diagnostic {
            case: "a column nothing declares, compared in the condition",
            sql: "SELECT id FROM ledger WHERE missing = 1",
            on_mysql: "Unknown column 'missing' in 'where clause'; Error code: 1054; \
                       SQLState: 42S22",
            on_postgresql: "ERROR: column \"missing\" does not exist; SQLState: 42703",
        },
        Diagnostic {
            case: "a column nothing declares, named in the ordering",
            sql: "SELECT id FROM ledger ORDER BY missing",
            on_mysql: "Unknown column 'missing' in 'order clause'; Error code: 1054; \
                       SQLState: 42S22",
            on_postgresql: "ERROR: column \"missing\" does not exist; SQLState: 42703",
        },
    ];

    for (engine, identifier) in engines {
        let cloud = Cloud::new();
        let (resource_arn, secret_arn) = cloud
            .create_cluster_running(identifier, engine, true)
            .await;
        let call = |members: Value| {
            let mut members = members;
            members["resourceArn"] = json!(resource_arn);
            members["secretArn"] = json!(secret_arn);
            members["database"] = json!(DATABASE);
            members
        };
        // The schema every row below is refused against, created through the Data API
        // itself: a key, a unique column beside it and a column that admits no NULL.
        for ddl in [
            "CREATE TABLE ledger (id INT PRIMARY KEY, label VARCHAR(16) UNIQUE, \
             note VARCHAR(16) NOT NULL)",
            "INSERT INTO ledger (id, label, note) VALUES (1, 'taken', 'first')",
        ] {
            let served = cloud.run(call(json!({"sql": ddl}))).await;
            assert!(
                served.get("message").is_none(),
                "setup on {engine}: {ddl} must be served, got {served}"
            );
        }

        for condition in conditions {
            let expected = if engine == "aurora-mysql" {
                condition.on_mysql
            } else {
                condition.on_postgresql
            };
            let (status, body) = cloud.execute(call(json!({"sql": condition.sql}))).await;
            assert_eq!(
                (status, error_code(&body), body["message"].clone()),
                (400, "DatabaseErrorException", json!(expected)),
                "case {:?} on {engine}: expected that engine's own diagnostic, got {status} \
                 {body}",
                condition.case
            );
        }

        // The VALUE a duplicate-key diagnostic names is the one the call really carried,
        // whether the statement wrote it or a parameter bound it — on either path a
        // statement reaches the engine by.
        let bound = json!([
            parameter("id", json!({"longValue": 1})),
            parameter("label", json!({"stringValue": "bound"})),
            parameter("note", json!({"stringValue": "bound"})),
        ]);
        let parameterised =
            "INSERT INTO ledger (id, label, note) VALUES (:id, :label, :note)";
        let expected_bound = if engine == "aurora-mysql" {
            "Duplicate entry '1' for key 'ledger.PRIMARY'; Error code: 1062; SQLState: 23000"
        } else {
            "ERROR: duplicate key value violates unique constraint \"ledger_pkey\"; \
             SQLState: 23505"
        };
        let calls = vec![
            (
                "ExecuteStatement",
                cloud
                    .execute(call(json!({"sql": parameterised, "parameters": bound})))
                    .await,
            ),
            (
                "BatchExecuteStatement",
                cloud
                    .batch(call(json!({
                        "sql": parameterised,
                        "parameterSets": [[
                            parameter("id", json!({"longValue": 1})),
                            parameter("label", json!({"stringValue": "batched"})),
                            parameter("note", json!({"stringValue": "batched"})),
                        ]],
                    })))
                    .await,
            ),
        ];
        for (operation, (status, body)) in calls {
            assert_eq!(
                (status, error_code(&body), body["message"].clone()),
                (400, "DatabaseErrorException", json!(expected_bound)),
                "case \"a key a parameter bound\" on {engine} through {operation}: got \
                 {status} {body}"
            );
        }

        // None of those refusals wrote anything: the row the setup stored is the only one.
        let rows = cloud
            .run(call(json!({"sql": "SELECT count(*) AS rows FROM ledger"})))
            .await;
        assert_eq!(
            rows["records"],
            json!([[{"longValue": 1}]]),
            "case \"after every refusal\" on {engine}: a refused statement writes nothing, \
             got {rows}"
        );
    }
}

/// The column definitions every table-option case declares, so the only difference between
/// a case's two tables is the options the definition carries.
const OPTION_CASE_COLUMNS: &str = "(id INT PRIMARY KEY, label VARCHAR(64) NOT NULL, hits INT)";

/// RULE: a TABLE OPTION of a `CREATE TABLE` names a property of how the engine STORES the
/// table — which storage engine holds it, the character set and collation its text columns
/// default to, its row format, how its pages are packed, its comment — and not what the
/// table holds, so a definition carrying table options is executed and defines exactly the
/// table the same definition without them defines. `DatabaseErrorException` is reserved for
/// SQL the engine itself rejects, which valid data definition is not. The one option of that
/// list a client can OBSERVE is `AUTO_INCREMENT = <value>`, "the initial AUTO_INCREMENT value
/// for the table", which the test below this one pins.
///
/// The rows span what the rule generalises over: each option class the MySQL Reference
/// Manual's `CREATE TABLE` entry lists under `table_option` (the storage engine, the
/// character set, the collation, the row format, the key packing, the persistent statistics,
/// the comment, the starting counter), the spellings a client writes them in (with `=` and
/// without, upper and lower case, several in one definition), and a definition carrying none
/// at all as the control every row is compared against.
#[tokio::test]
async fn a_definition_carrying_mysql_table_options_defines_the_same_table() {
    // (case, the options the definition carries)
    let cases = [
        ("the storage engine", "ENGINE = InnoDB"),
        ("the storage engine, written without =", "ENGINE InnoDB"),
        ("the storage engine, in lower case", "engine = innodb"),
        ("another storage engine", "ENGINE = MyISAM"),
        (
            "the default character set and collation",
            "DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_0900_ai_ci",
        ),
        ("the row format", "ROW_FORMAT = DYNAMIC"),
        ("the key packing", "PACK_KEYS = 1 KEY_BLOCK_SIZE = 8"),
        ("the persistent statistics", "STATS_PERSISTENT = 1"),
        ("a comment", "COMMENT = 'rows a client seeded'"),
        (
            "the counter's own start, on a table whose key is not a counter",
            "AUTO_INCREMENT = 1",
        ),
        (
            "the whole idiom a dump emits",
            "ENGINE = InnoDB AUTO_INCREMENT = 1 DEFAULT CHARSET = utf8mb4 \
             COLLATE = utf8mb4_0900_ai_ci ROW_FORMAT = DYNAMIC",
        ),
    ];

    for (index, (case, options)) in cases.into_iter().enumerate() {
        let api = DataApi::new();
        let declared = format!("options_{index}");
        let control = format!("plain_{index}");

        let (status, created) = api
            .execute(json!({
                "sql": format!("CREATE TABLE {declared} {OPTION_CASE_COLUMNS} {options}"),
            }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the definition must be executed, got {status} {created}"
        );
        assert_eq!(
            number(&created, "numberOfRecordsUpdated"),
            0,
            "case {case:?}: a definition updates no records, got {created}"
        );
        api.run(json!({
            "sql": format!("CREATE TABLE {control} {OPTION_CASE_COLUMNS}"),
        }))
        .await;

        // The same row, stored and read back through both tables: what the table holds, and
        // how the result set describes it, cannot depend on the options.
        for table in [&declared, &control] {
            api.run(json!({
                "sql": format!(
                    "INSERT INTO {table} (id, label, hits) VALUES (:id, :label, 7)"
                ),
                "parameters": [
                    parameter("id", json!({"longValue": 1})),
                    parameter("label", json!({"stringValue": "seeded"})),
                ],
            }))
            .await;
        }
        let read_back = async |table: &str| {
            api.run(json!({
                "sql": format!("SELECT id, label, hits FROM {table} ORDER BY id"),
                "includeResultMetadata": true,
            }))
            .await
        };
        let from_declared = read_back(&declared).await;
        let from_control = read_back(&control).await;
        assert_eq!(
            from_declared["records"], from_control["records"],
            "case {case:?}: expected the rows of the table declared without options \
             ({}), got {from_declared}",
            from_control["records"]
        );
        assert_eq!(
            described_columns(&from_declared),
            described_columns(&from_control),
            "case {case:?}: expected the columns of the table declared without options, \
             got {from_declared}"
        );

        // And the constraints the definition declared are in force in both: the NOT NULL
        // column refuses a row that leaves it out.
        for table in [&declared, &control] {
            let (status, refused) = api
                .execute(json!({
                    "sql": format!("INSERT INTO {table} (id, hits) VALUES (2, 1)"),
                }))
                .await;
            assert_eq!(
                (status, error_code(&refused)),
                (400, "DatabaseErrorException"),
                "case {case:?}: the NOT NULL column of {table} must refuse the row, got \
                 {status} {refused}"
            );
        }
    }
}

/// RULE: `AUTO_INCREMENT = <value>` is "the initial AUTO_INCREMENT value for the table", so
/// the first key the table's counter hands out is that value and each later row takes the
/// next one — whatever else the definition carries, and whichever dialect declared the
/// counter.
///
/// The rows span what the rule generalises over: starting values at the counter's own start,
/// just above it and far above it, the option alone and inside the idiom a dump emits, both
/// spellings of a counter column (MySQL's `AUTO_INCREMENT` attribute, which is the one
/// `generatedFields` reports, and PostgreSQL's `serial`), and the case a definition writes the
/// table's name in — the name the engine then keeps the counter under.
#[tokio::test]
async fn a_definition_can_start_the_tables_counter_at_a_value_of_its_own() {
    // (case, the table's name, the counter column's declaration, the options, the first key
    //  the table hands out)
    let cases = [
        (
            "the counter's own start",
            "counted_plain",
            "INT AUTO_INCREMENT",
            "AUTO_INCREMENT = 1",
            1,
        ),
        (
            "one above the counter's own start",
            "counted_next",
            "INT AUTO_INCREMENT",
            "AUTO_INCREMENT = 2",
            2,
        ),
        (
            "far above it",
            "counted_far",
            "INT AUTO_INCREMENT",
            "AUTO_INCREMENT = 1000",
            1000,
        ),
        (
            "inside the idiom a dump emits",
            "counted_dump",
            "BIGINT AUTO_INCREMENT",
            "ENGINE = InnoDB AUTO_INCREMENT = 42 DEFAULT CHARSET = utf8mb4",
            42,
        ),
        (
            "a counter declared in PostgreSQL's spelling",
            "counted_serial",
            "serial",
            "AUTO_INCREMENT = 7",
            7,
        ),
        (
            "a table whose name is not in lower case",
            "CountedOrders",
            "INT AUTO_INCREMENT",
            "AUTO_INCREMENT = 500",
            500,
        ),
        (
            "a table whose name the definition delimited",
            "\"Counted Orders\"",
            "INT AUTO_INCREMENT",
            "AUTO_INCREMENT = 9",
            9,
        ),
        (
            "no starting value at all, so the counter's own start",
            "counted_none",
            "INT AUTO_INCREMENT",
            "ENGINE = InnoDB",
            1,
        ),
    ];

    for (case, table, counter, options, first_key) in cases {
        let api = DataApi::new();
        let (status, created) = api
            .execute(json!({
                "sql": format!(
                    "CREATE TABLE {table} (id {counter} PRIMARY KEY, label VARCHAR(16)) {options}"
                ),
            }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the definition must be executed, got {status} {created}"
        );

        // Two rows that leave the counter column out: the first takes the value the
        // definition started the counter at, the second the one after it.
        for expected in [first_key, first_key + 1] {
            api.run(json!({
                "sql": format!("INSERT INTO {table} (label) VALUES (:label)"),
                "parameters": [parameter("label", json!({"stringValue": "row"}))],
            }))
            .await;
            let stored = api
                .run(json!({"sql": format!("SELECT max(id) FROM {table}")}))
                .await;
            assert_eq!(
                stored["records"],
                json!([[{"longValue": expected}]]),
                "case {case:?}: expected the counter to hand out {expected}, got {stored}"
            );
        }
    }
}

/// Each column of a result set as `(name, typeName, nullable)`, which is what
/// `columnMetadata` describes a table's columns by.
fn described_columns(body: &Value) -> Vec<(String, String, Value)> {
    list(body, "columnMetadata")
        .iter()
        .map(|column| {
            (
                column["name"].as_str().unwrap_or_default().to_string(),
                column["typeName"].as_str().unwrap_or_default().to_string(),
                column["nullable"].clone(),
            )
        })
        .collect()
}
