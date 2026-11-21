// SPDX-License-Identifier: CC0-1.0

//! The CATALOGUE of a data warehouse the Data API did not create, and the
//! precedence its target validation runs in — both driven over the wire against
//! the Data API mounted beside the two Redshift control planes that own the
//! warehouses.
//!
//! Two rules, each asserted over a TABLE of inputs rather than one recorded
//! request:
//!
//! 1. a data warehouse holds the database it was PROVISIONED with — a serverless
//!    namespace's `dbName`, a provisioned cluster's `DBName` — from the moment it
//!    exists, so the Data API lists that database and resolves a connection
//!    through it before any statement has ever run, while a database no warehouse
//!    of that name holds is still a missing resource;
//! 2. how a request SELECTS its compute target is judged against the targets it
//!    NAMED: a request that names none of the targets an operation accepts, or
//!    more than one of them where at least one really exists, is refused with
//!    that operation's own one-target sentence — while one whose every named
//!    warehouse is absent from the Region is refused as naming no endpoint at
//!    all, and nothing is recorded for either.

use serde_json::{json, Value};

use super::emulator::{error_code, list, Warehouses};

/// The one-target sentence a SUBMIT operation refuses a bad target selection
/// with: it accepts a session and a Glue-managed target beside the two
/// warehouses, so its refusal enumerates all four.
const SUBMIT_ONE_TARGET: &str = "There should be only one of the following: ClusterIdentifier, \
                                 WorkgroupName, Glue resource, SessionId";

/// The one-target sentence a CATALOGUE reader refuses a bad target selection
/// with: `ListDatabases` and `DescribeTable` model the two warehouse selectors
/// and no other
/// (`smithy_query(query="operation",operation="ListDatabases,DescribeTable")`).
const WAREHOUSE_ONE_TARGET: &str = "There should be only one of the following: ClusterIdentifier, \
                                    WorkgroupName";

/// The fixed sentence the service refuses a selection whose every named
/// warehouse is absent from the Region with: there is no endpoint for it
/// whichever of them it meant.
const NO_ENDPOINT: &str = "Redshift endpoint doesn't exist in this region.";

/// A `SessionId` in the form the modelled `UUID` pattern accepts, which names no
/// session this service holds.
const SESSION_ID: &str = "3dc6a4b5-5e2b-4b5a-9f1d-6b6d2b6c7a8e";

/// The schema this emulator's engine holds every table of a database in, which
/// is the one a column's metadata names.
const DEFAULT_SCHEMA: &str = "public";

/// The `Message` an error body carries, or `None` when it carries none.
fn message(response: &Value) -> Option<&str> {
    response["Message"].as_str()
}

/// The `Databases` a response reports, as plain strings.
fn reported(response: &Value) -> Vec<String> {
    list(response, "Databases")
        .iter()
        .map(|database| database.as_str().unwrap_or_default().to_string())
        .collect()
}

/// A request about `target`, connecting through `database`.
fn connect(target: &Value, database: &str) -> Value {
    let mut request = target.clone();
    request["Database"] = json!(database);
    request
}

/// One row of the catalogue table: a data warehouse, the members every Data API
/// request about it carries, and the database its OWN control plane provisioned
/// it with.
struct Warehouse {
    label: &'static str,
    target: Value,
    provisioned: String,
}

/// The database a serverless namespace was created with, read back from the
/// control plane that OWNS the namespace — so the expectation below is the
/// owner's answer and never a name written into this test.
async fn namespace_database(warehouses: &Warehouses, namespace: &str) -> String {
    let (status, body) = warehouses
        .json(
            "redshift-serverless",
            "RedshiftServerless",
            "GetNamespace",
            &json!({"namespaceName": namespace}),
        )
        .await;
    assert_eq!(
        status, 200,
        "GetNamespace {namespace}: expected 200, got {status} {body}"
    );
    body["namespace"]["dbName"]
        .as_str()
        .unwrap_or_else(|| panic!("a namespace carries the database it was created with: {body}"))
        .to_string()
}

/// Provision the warehouses the catalogue rules are asserted over. The rows
/// differ in the KIND of warehouse (a serverless workgroup, a provisioned
/// cluster), in the FORM the request names it in (its name, its ARN), and in the
/// database it was created with (one the create named, one the owning service
/// named for it).
async fn provisioned_warehouses() -> (Warehouses, Vec<Warehouse>) {
    let warehouses = Warehouses::new();

    warehouses
        .create_workgroup_with_database("catalogue-workgroup", Some("devdb"))
        .await;
    let arn = warehouses
        .create_workgroup_with_database("reporting-workgroup", Some("reporting"))
        .await;
    warehouses
        .create_workgroup_with_database("defaulted-workgroup", None)
        .await;
    let defaulted = namespace_database(&warehouses, "defaulted-workgroup-ns").await;
    warehouses
        .create_cluster_with_database("catalogue-cluster", Some("analytics"))
        .await;
    warehouses
        .create_cluster_with_database("second-cluster", Some("dev"))
        .await;

    let rows = vec![
        Warehouse {
            label: "a workgroup whose namespace named its first database",
            target: json!({"WorkgroupName": "catalogue-workgroup", "DbUser": "awsuser"}),
            provisioned: "devdb".to_string(),
        },
        Warehouse {
            label: "the same kind of workgroup named by ARN",
            target: json!({"WorkgroupName": arn, "DbUser": "awsuser"}),
            provisioned: "reporting".to_string(),
        },
        Warehouse {
            label: "a workgroup whose namespace left its first database to the service",
            target: json!({"WorkgroupName": "defaulted-workgroup", "DbUser": "awsuser"}),
            provisioned: defaulted,
        },
        Warehouse {
            label: "a provisioned cluster created with a database of its own",
            target: json!({"ClusterIdentifier": "catalogue-cluster", "DbUser": "awsuser"}),
            provisioned: "analytics".to_string(),
        },
        Warehouse {
            label: "a second provisioned cluster, created with another database",
            target: json!({"ClusterIdentifier": "second-cluster", "DbUser": "awsuser"}),
            provisioned: "dev".to_string(),
        },
    ];
    (warehouses, rows)
}

/// A warehouse's own database is listed, and can be connected through, before
/// any statement has run on it — and it is listed for THAT warehouse only.
#[tokio::test]
async fn a_warehouse_lists_the_database_it_was_provisioned_with() {
    let (warehouses, rows) = provisioned_warehouses().await;

    for row in &rows {
        let label = row.label;
        let response = warehouses
            .data_ok("ListDatabases", &connect(&row.target, &row.provisioned))
            .await;
        assert_eq!(
            reported(&response),
            vec![row.provisioned.clone()],
            "case {label}: a warehouse no statement has run on holds exactly the database it \
             was created with ({:?}), got {response}",
            row.provisioned
        );
        assert!(
            response.get("NextToken").is_none(),
            "case {label}: one page holds that database, got {response}"
        );

        // The databases of ANOTHER warehouse are not this one's: connecting
        // through one of them names a database this warehouse does not hold.
        for other in &rows {
            if other.provisioned == row.provisioned {
                continue;
            }
            let (status, response) = warehouses
                .data("ListDatabases", &connect(&row.target, &other.provisioned))
                .await;
            assert_eq!(
                status, 404,
                "case {label}: {:?} belongs to another warehouse, expected 404, got {status} \
                 {response}",
                other.provisioned
            );
            assert_eq!(
                error_code(&response),
                "ResourceNotFoundException",
                "case {label}: expected ResourceNotFoundException for {:?}, got {response}",
                other.provisioned
            );
        }
    }
}

/// A database a `CREATE DATABASE` statement adds JOINS the one the warehouse was
/// provisioned with: the catalogue reports both, in name order and each exactly
/// once. A statement naming a database the warehouse holds NONE of creates
/// nothing — it is accepted and then fails with the backend's own fatal line,
/// because the connection it needs cannot be made.
#[tokio::test]
async fn a_created_database_joins_the_provisioned_one() {
    let (warehouses, rows) = provisioned_warehouses().await;

    for row in &rows {
        let label = row.label;
        // A database nothing created is not a database of the warehouse, so the
        // statement fails asynchronously instead of opening one.
        let mut absent = connect(&row.target, "scratch");
        absent["Sql"] = json!("CREATE TABLE marker (id INTEGER)");
        absent["WaitTimeSeconds"] = json!(30);
        let submitted = warehouses.data_ok("ExecuteStatement", &absent).await;
        let described = warehouses
            .data_ok("DescribeStatement", &json!({"Id": submitted["Id"]}))
            .await;
        assert_eq!(
            described["Status"], "FAILED",
            "case {label}: a statement naming a database the warehouse holds none of must fail, \
             got {described}"
        );
        assert_eq!(
            described["Error"],
            json!("FATAL: database \"scratch\" does not exist"),
            "case {label}: expected the backend's fatal line for the absent database, got \
             {described}"
        );
        let response = warehouses
            .data_ok("ListDatabases", &connect(&row.target, &row.provisioned))
            .await;
        assert_eq!(
            reported(&response),
            vec![row.provisioned.clone()],
            "case {label}: a statement that failed to connect added no database, got {response}"
        );

        // `CREATE DATABASE` is what adds one, and the statements that follow run
        // on it.
        let mut create = connect(&row.target, &row.provisioned);
        create["Sql"] = json!("CREATE DATABASE scratch");
        create["WaitTimeSeconds"] = json!(30);
        let created = warehouses.data_ok("ExecuteStatement", &create).await;
        assert_eq!(
            created["Status"], "FINISHED",
            "case {label}: CREATE DATABASE must finish, got {created}"
        );

        for database in [row.provisioned.as_str(), "scratch"] {
            let mut request = connect(&row.target, database);
            request["Sql"] = json!("CREATE TABLE marker (id INTEGER)");
            request["WaitTimeSeconds"] = json!(30);
            let response = warehouses.data_ok("ExecuteStatement", &request).await;
            assert_eq!(
                response["Status"], "FINISHED",
                "case {label}: a statement on {database:?} must finish, got {response}"
            );
        }

        let mut expected = vec![row.provisioned.clone(), "scratch".to_string()];
        expected.sort();
        let response = warehouses
            .data_ok("ListDatabases", &connect(&row.target, "scratch"))
            .await;
        assert_eq!(
            reported(&response),
            expected,
            "case {label}: expected {expected:?} once each, in name order, got {response}"
        );
    }
}

/// The catalogue of a warehouse's own database: empty until a statement builds
/// something in it, readable through `DescribeTable` either way, and a database
/// the warehouse does not hold at all is the missing resource — unlike a table
/// it does not hold, which describes no column.
#[tokio::test]
async fn describe_table_reads_the_provisioned_databases_catalogue() {
    let (warehouses, rows) = provisioned_warehouses().await;

    for row in &rows {
        let label = row.label;
        // No statement has run: the database exists, so this is a database with
        // no table in it rather than a database that does not exist.
        let empty = warehouses
            .data_ok("DescribeTable", &connect(&row.target, &row.provisioned))
            .await;
        assert!(
            list(&empty, "ColumnList").is_empty(),
            "case {label}: a database no statement has built anything in describes no column, \
             got {empty}"
        );

        // A table named in that empty database describes no column — a name the
        // catalog matches nothing for is an empty answer, not a refusal.
        let mut request = connect(&row.target, &row.provisioned);
        request["Table"] = json!("absent_table");
        let described = warehouses.data_ok("DescribeTable", &request).await;
        assert!(
            list(&described, "ColumnList").is_empty(),
            "case {label}: a table the database does not hold describes no column, got \
             {described}"
        );
        assert_eq!(
            described["TableName"],
            json!(format!("{}..absent_table", row.provisioned)),
            "case {label}: the empty answer still names the path it was asked about, with an \
             empty component for the schema the request did not name, got {described}"
        );

        // A database the warehouse holds no database of that name for is the
        // missing resource too, and the refusal names the database.
        let (status, response) = warehouses
            .data("DescribeTable", &connect(&row.target, "no-such-database"))
            .await;
        assert_eq!(
            status, 404,
            "case {label}: a database the warehouse does not hold is a missing resource, got \
             {status} {response}"
        );
        assert_eq!(
            message(&response),
            Some("Database does not exist: no-such-database"),
            "case {label}: expected the missing DATABASE to be named, got {response}"
        );

        // And the round trip: a table created in the warehouse's own database is
        // described with the columns the engine really declares for it.
        let mut create = connect(&row.target, &row.provisioned);
        create["Sql"] = json!("CREATE TABLE orders (id INTEGER, placed_at VARCHAR(32))");
        create["WaitTimeSeconds"] = json!(30);
        let created = warehouses.data_ok("ExecuteStatement", &create).await;
        assert_eq!(
            created["Status"], "FINISHED",
            "case {label}: the CREATE TABLE must finish, got {created}"
        );

        let mut described = connect(&row.target, &row.provisioned);
        described["Table"] = json!("orders");
        let described = warehouses.data_ok("DescribeTable", &described).await;
        let columns: Vec<String> = list(&described, "ColumnList")
            .iter()
            .map(|column| column["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            columns,
            vec!["id".to_string(), "placed_at".to_string()],
            "case {label}: expected the created table's columns, got {described}"
        );
        for column in list(&described, "ColumnList") {
            assert_eq!(
                (
                    column["schemaName"].clone(),
                    column["tableName"].clone()
                ),
                (json!(DEFAULT_SCHEMA), json!("orders")),
                "case {label}: every described column names the table that includes it, got \
                 {column}"
            );
        }
    }
}

/// A bad target selection is judged against the targets it NAMED: it is refused
/// on the SELECTION as long as one of them exists, and as naming no endpoint when
/// none of them does — and no statement is recorded for a refused request.
#[tokio::test]
async fn a_bad_target_selection_is_judged_against_the_targets_it_named() {
    let warehouses = Warehouses::new();
    warehouses.create_cluster("present-cluster").await;
    warehouses.create_workgroup("present-workgroup").await;

    // Each row is a target SELECTION and whether the refusal reports the
    // selection or the endpoint. They differ in how many targets they select, in
    // which ones, and — the point of this table — in whether the warehouses they
    // name exist in the Region at all.
    let selections: [(&str, Value, bool); 7] = [
        // Nothing was named, so there is nothing to resolve: the caller is told
        // which selectors it must name one of.
        ("no target at all", json!({}), true),
        (
            "both warehouses, both present",
            json!({"ClusterIdentifier": "present-cluster", "WorkgroupName": "present-workgroup"}),
            true,
        ),
        (
            "both warehouses, the cluster absent",
            json!({"ClusterIdentifier": "absent-cluster", "WorkgroupName": "present-workgroup"}),
            true,
        ),
        (
            "both warehouses, the workgroup absent",
            json!({"ClusterIdentifier": "present-cluster", "WorkgroupName": "absent-workgroup"}),
            true,
        ),
        // Neither of the warehouses it named is there, so which of them it meant
        // decides nothing: the request names no endpoint.
        (
            "both warehouses, neither present",
            json!({"ClusterIdentifier": "absent-cluster", "WorkgroupName": "absent-workgroup"}),
            false,
        ),
        (
            "an absent cluster beside a session that is not live",
            json!({"ClusterIdentifier": "absent-cluster", "SessionId": SESSION_ID}),
            false,
        ),
        (
            "a present workgroup beside a session",
            json!({"WorkgroupName": "present-workgroup", "SessionId": SESSION_ID}),
            true,
        ),
    ];

    for (case, selection, on_selection) in &selections {
        // Each operation that takes these members, with the sentence IT states:
        // a submit enumerates every target it accepts, a catalogue reader only
        // the two it models.
        for (operation, base, selectors) in [
            (
                "ExecuteStatement",
                json!({"Database": "dev", "DbUser": "awsuser", "Sql": "SELECT 1"}),
                SUBMIT_ONE_TARGET,
            ),
            (
                "BatchExecuteStatement",
                json!({"Database": "dev", "DbUser": "awsuser", "Sqls": ["SELECT 1"]}),
                SUBMIT_ONE_TARGET,
            ),
            (
                "ListDatabases",
                json!({"Database": "dev", "DbUser": "awsuser"}),
                WAREHOUSE_ONE_TARGET,
            ),
            (
                "DescribeTable",
                json!({"Database": "dev", "DbUser": "awsuser"}),
                WAREHOUSE_ONE_TARGET,
            ),
        ] {
            // A catalogue reader models no `SessionId`, so a selection naming one
            // cannot be sent to it at all.
            if selection.get("SessionId").is_some() && selectors == WAREHOUSE_ONE_TARGET {
                continue;
            }
            let expected = if *on_selection { selectors } else { NO_ENDPOINT };
            let mut request = base;
            for (member, value) in selection.as_object().expect("an object of members") {
                request[member] = value.clone();
            }
            let (status, response) = warehouses.data(operation, &request).await;
            assert_eq!(
                status, 400,
                "case {case:?} on {operation}: expected 400, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case:?} on {operation}: expected ValidationException, got {response}"
            );
            assert_eq!(
                message(&response),
                Some(expected),
                "case {case:?} on {operation}: expected {expected:?}, got {response}"
            );
        }
    }

    // Nothing above was admitted: a request refused on its selection, or for
    // naming no endpoint, leaves no statement behind.
    let page = warehouses.data_ok("ListStatements", &json!({})).await;
    assert!(
        list(&page, "Statements").is_empty(),
        "a refused request must record no statement, got {page}"
    );

    // And the control: exactly one target, on a warehouse the Region holds, is
    // admitted — so the table above refuses the SELECTION and not the request.
    let accepted = warehouses
        .data_ok(
            "ExecuteStatement",
            &json!({
                "WorkgroupName": "present-workgroup",
                "Database": "dev",
                "DbUser": "awsuser",
                "Sql": "SELECT 1",
            }),
        )
        .await;
    assert!(
        accepted["Id"].is_string(),
        "one target selects one warehouse and is admitted, got {accepted}"
    );
}
