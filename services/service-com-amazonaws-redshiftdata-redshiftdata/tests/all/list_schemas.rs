// SPDX-License-Identifier: CC0-1.0

//! `ListSchemas`, driven end-to-end over the wire: every call below is a real
//! `POST /` with `X-Amz-Target: RedshiftData.ListSchemas` dispatched through the
//! router.
//!
//! The RULE: the operation "lists the schemas in a database" out of that
//! database's own catalogue of schemas — the one every Redshift database holds
//! from the moment it exists, plus the ones its statements created and minus the
//! ones they dropped — in name order, narrowed by the pattern the operation
//! documents for `SchemaPattern` ("`%` means match any substring of 0 or more
//! characters and `_` means match any one character"), and paged with an opaque
//! `NextToken`. It REPORTS: it opens no database and creates no schema, and only
//! a database the warehouse does not hold at all is a missing resource.

use serde_json::{json, Value};

use super::emulator::{error_code, list, Emulator, Warehouses};

/// The connection every request against the single-service harness is made
/// through.
fn cluster() -> Value {
    json!({"ClusterIdentifier": "my-cluster", "Database": "dev", "DbUser": "awsuser"})
}

/// The schema every database of a data warehouse holds from the moment it
/// exists: "PUBLIC schema … Every database created in Amazon Redshift has a
/// schema named PUBLIC".
const DEFAULT_SCHEMA: &str = "public";

/// The one-target sentence a catalogue reader refuses a bad target selection
/// with: `ListSchemas` models `ClusterIdentifier` and `WorkgroupName` and no
/// other selector (`smithy_query(query="operation",operation="ListSchemas")`).
const WAREHOUSE_ONE_TARGET: &str = "There should be only one of the following: ClusterIdentifier, \
                                    WorkgroupName";

/// The fixed sentence the service refuses a selection whose every named
/// warehouse is absent from the Region with.
const NO_ENDPOINT: &str = "Redshift endpoint doesn't exist in this region.";

/// The `Schemas` a response reports, as plain strings.
fn reported(response: &Value) -> Vec<String> {
    list(response, "Schemas")
        .iter()
        .map(|schema| schema.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `Message` an error body carries, or `None` when it carries none.
fn message(response: &Value) -> Option<&str> {
    response["Message"].as_str()
}

/// Expected schema names as owned strings, for comparison against a response.
fn expected(names: &[&str]) -> Vec<String> {
    names.iter().map(ToString::to_string).collect()
}

/// A request about `target`, connecting through `database`.
fn connect(target: &Value, database: &str) -> Value {
    let mut request = target.clone();
    request["Database"] = json!(database);
    request
}

/// Build the catalogue every case below reads: four schemas a statement created
/// beside the one the database already held, two of them holding a table.
async fn catalogued() -> Emulator {
    let emulator = Emulator::new();
    for sql in [
        "CREATE SCHEMA sales",
        "CREATE SCHEMA sandbox",
        "CREATE SCHEMA reporting",
        "CREATE SCHEMA finance",
        "CREATE TABLE sales.orders (id INTEGER)",
        "CREATE TABLE finance.ledger (id INTEGER)",
        "CREATE TABLE people (id INTEGER)",
    ] {
        emulator.run(sql).await;
    }
    emulator
}

/// Every schema the database holds is listed, in name order, and the listing
/// follows the catalogue as statements change it.
#[tokio::test]
async fn every_schema_the_database_holds_is_listed() {
    let emulator = catalogued().await;

    let listed = emulator.ok("ListSchemas", &cluster()).await;
    assert_eq!(
        reported(&listed),
        expected(&["finance", "public", "reporting", "sales", "sandbox"]),
        "every schema of the database is listed in name order, got {listed}"
    );
    assert!(
        listed.get("NextToken").is_none(),
        "one page holds every schema, so no continuation token is issued, got {listed}"
    );

    // A report, not a mutation: reading the catalogue twice reads the same
    // catalogue.
    let again = emulator.ok("ListSchemas", &cluster()).await;
    assert_eq!(
        reported(&again),
        reported(&listed),
        "a second read answers the same catalogue, got {again}"
    );

    // The listing follows the catalogue: a dropped schema leaves it, an added
    // one joins it.
    emulator.run("DROP SCHEMA reporting").await;
    emulator.run("CREATE SCHEMA audit").await;
    let listed = emulator.ok("ListSchemas", &cluster()).await;
    assert_eq!(
        reported(&listed),
        expected(&["audit", "finance", "public", "sales", "sandbox"]),
        "a dropped schema leaves the listing and a created one joins it, got {listed}"
    );

    // A database the warehouse holds none of is the missing resource, and the
    // refusal names it.
    let (status, response) = emulator.call("ListSchemas", &connect(&cluster(), "absent")).await;
    assert_eq!(
        status, 404,
        "a database the warehouse does not hold is a missing resource, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "ResourceNotFoundException",
        "expected ResourceNotFoundException, got {response}"
    );
    assert_eq!(
        message(&response),
        Some("Database does not exist: absent"),
        "expected the missing database to be named, got {response}"
    );
}

/// One row of the pattern table: the pattern a request carries, and every schema
/// it must match.
struct PatternRow {
    label: &'static str,
    pattern: Option<&'static str>,
    matched: &'static [&'static str],
}

/// `SchemaPattern` narrows the listing, with `%` matching any substring of 0 or
/// more characters and `_` any one character; a pattern the request does not
/// carry narrows nothing.
#[tokio::test]
async fn the_schema_pattern_narrows_the_listing() {
    let emulator = catalogued().await;

    // The rows differ in which wild card the pattern uses, where it uses it, how
    // many of them it carries, and how many schemas it leaves. The two wild
    // cards are the ones `SchemaPattern` documents
    // (`smithy_query(query="doc",shape="ListSchemasRequest",member="SchemaPattern")`).
    let rows: &[PatternRow] = &[
        PatternRow {
            label: "no pattern at all",
            pattern: None,
            matched: &["finance", "public", "reporting", "sales", "sandbox"],
        },
        PatternRow {
            label: "the bare any-substring wild card",
            pattern: Some("%"),
            matched: &["finance", "public", "reporting", "sales", "sandbox"],
        },
        PatternRow {
            label: "a trailing wild card",
            pattern: Some("s%"),
            matched: &["sales", "sandbox"],
        },
        PatternRow {
            label: "a leading wild card",
            pattern: Some("%g"),
            matched: &["reporting"],
        },
        PatternRow {
            label: "a wild card at both ends",
            pattern: Some("%an%"),
            matched: &["finance", "sandbox"],
        },
        PatternRow {
            label: "exactly one name",
            pattern: Some("public"),
            matched: &[DEFAULT_SCHEMA],
        },
        PatternRow {
            label: "the single-character wild card inside a name",
            pattern: Some("sal_s"),
            matched: &["sales"],
        },
        PatternRow {
            label: "a pattern of single-character wild cards only, which fixes the length",
            pattern: Some("_____"),
            matched: &["sales"],
        },
        PatternRow {
            label: "a pattern nothing matches",
            pattern: Some("absent%"),
            matched: &[],
        },
        PatternRow {
            label: "a name one character longer than any schema's",
            pattern: Some("sales_"),
            matched: &[],
        },
    ];

    for row in rows {
        let label = row.label;
        let mut request = cluster();
        if let Some(pattern) = row.pattern {
            request["SchemaPattern"] = json!(pattern);
        }
        let listed = emulator.ok("ListSchemas", &request).await;
        assert_eq!(
            reported(&listed),
            expected(row.matched),
            "case {label}: expected {:?}, got {listed}",
            row.matched
        );
    }
}

/// The listing is paged with an opaque `NextToken`: every page carries at most
/// `MaxResults` names, the pages together carry the whole listing exactly once,
/// and a token this listing did not issue is refused.
#[tokio::test]
async fn the_listing_is_paged_with_an_opaque_token() {
    let emulator = catalogued().await;
    let whole = reported(&emulator.ok("ListSchemas", &cluster()).await);

    let mut collected: Vec<String> = Vec::new();
    let mut next_token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        let mut request = cluster();
        request["MaxResults"] = json!(2);
        if let Some(token) = &next_token {
            request["NextToken"] = json!(token);
        }
        let page = emulator.ok("ListSchemas", &request).await;
        let names = reported(&page);
        assert!(
            names.len() <= 2,
            "page {pages}: MaxResults 2 holds at most two names, got {page}"
        );
        collected.extend(names);
        pages += 1;
        match page.get("NextToken").and_then(Value::as_str) {
            Some(token) => next_token = Some(token.to_string()),
            None => break,
        }
        assert!(
            pages < 10,
            "the listing must run out of pages: {collected:?}"
        );
    }
    assert_eq!(
        collected, whole,
        "the pages together carry the whole listing, in the same order"
    );
    assert_eq!(
        pages, 3,
        "five names at two per page take three pages, got {pages}"
    );

    // A token is a position in the listing it was issued for: one minted while
    // paging the whole catalogue names no position in a narrowed one.
    let mut paging = cluster();
    paging["MaxResults"] = json!(2);
    let first = emulator.ok("ListSchemas", &paging).await;
    let token = first["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("the first of three pages issues a token: {first}"))
        .to_string();
    let mut narrowed = cluster();
    narrowed["SchemaPattern"] = json!("s%");
    narrowed["NextToken"] = json!(token.clone());
    let (status, response) = emulator.call("ListSchemas", &narrowed).await;
    assert_eq!(
        status, 400,
        "a token of another listing names no position in this one, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "expected ValidationException for a foreign token, got {response}"
    );

    // And a token this operation never issued at all is not a token.
    for bad in ["not-hexadecimal", "6162636465"] {
        let mut request = cluster();
        request["NextToken"] = json!(bad);
        let (status, response) = emulator.call("ListSchemas", &request).await;
        assert_eq!(
            status, 400,
            "case {bad:?}: expected 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {bad:?}: expected ValidationException, got {response}"
        );
    }
}

/// A database the warehouse was PROVISIONED with holds the public schema before
/// any statement has run on it — and listing its schemas opens nothing, so a
/// later caller finds the database exactly as it was. A schema a statement then
/// creates joins the listing, and the tables it holds are listed under it.
#[tokio::test]
async fn a_provisioned_database_holds_the_public_schema_before_any_statement() {
    let warehouses = Warehouses::new();
    warehouses
        .create_cluster_with_database("schema-cluster", Some("analytics"))
        .await;
    warehouses
        .create_workgroup_with_database("schema-workgroup", Some("devdb"))
        .await;

    // Each row is a warehouse of a different KIND, with the database its own
    // control plane provisioned it with.
    let rows: [(&str, Value, &str); 2] = [
        (
            "a provisioned cluster",
            json!({"ClusterIdentifier": "schema-cluster", "DbUser": "awsuser"}),
            "analytics",
        ),
        (
            "a serverless workgroup",
            json!({"WorkgroupName": "schema-workgroup", "DbUser": "awsuser"}),
            "devdb",
        ),
    ];

    for (label, target, database) in &rows {
        let request = connect(target, database);
        let listed = warehouses.data_ok("ListSchemas", &request).await;
        assert_eq!(
            reported(&listed),
            expected(&[DEFAULT_SCHEMA]),
            "case {label}: a database no statement has run on holds the public schema, got \
             {listed}"
        );

        // The pattern narrows that listing too.
        let mut narrowed = request.clone();
        narrowed["SchemaPattern"] = json!("pub%");
        let matched = warehouses.data_ok("ListSchemas", &narrowed).await;
        assert_eq!(
            reported(&matched),
            expected(&[DEFAULT_SCHEMA]),
            "case {label}: the pattern admits the public schema, got {matched}"
        );
        narrowed["SchemaPattern"] = json!("s%");
        let empty = warehouses.data_ok("ListSchemas", &narrowed).await;
        assert!(
            reported(&empty).is_empty(),
            "case {label}: a pattern nothing matches lists nothing, got {empty}"
        );

        // The read opened nothing: the database still holds no relation, and
        // the warehouse still holds exactly the database it was created with.
        let tables = warehouses.data_ok("ListTables", &request).await;
        assert!(
            list(&tables, "Tables").is_empty(),
            "case {label}: listing schemas built nothing in the database, got {tables}"
        );
        let databases = warehouses.data_ok("ListDatabases", &request).await;
        assert_eq!(
            list(&databases, "Databases")
                .iter()
                .map(|name| name.as_str().unwrap_or_default().to_string())
                .collect::<Vec<String>>(),
            expected(&[database]),
            "case {label}: listing schemas added no database, got {databases}"
        );

        // The round trip: a schema a statement creates joins the listing, and a
        // table created in it is listed under that schema.
        for sql in [
            "CREATE SCHEMA marketing",
            "CREATE TABLE marketing.leads (id INTEGER)",
        ] {
            let mut submit = request.clone();
            submit["Sql"] = json!(sql);
            submit["WaitTimeSeconds"] = json!(30);
            let submitted = warehouses.data_ok("ExecuteStatement", &submit).await;
            assert_eq!(
                submitted["Status"], "FINISHED",
                "case {label}: {sql} must finish, got {submitted}"
            );
        }
        let listed = warehouses.data_ok("ListSchemas", &request).await;
        assert_eq!(
            reported(&listed),
            expected(&["marketing", DEFAULT_SCHEMA]),
            "case {label}: the created schema joins the one the database held, got {listed}"
        );
        let tables = warehouses.data_ok("ListTables", &request).await;
        let relations: Vec<String> = list(&tables, "Tables")
            .iter()
            .map(|table| {
                format!(
                    "{}.{}",
                    table["schema"].as_str().unwrap_or_default(),
                    table["name"].as_str().unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(
            relations,
            expected(&["marketing.leads"]),
            "case {label}: the created table is listed under the created schema, got {tables}"
        );

        // A database of ANOTHER warehouse is not this one's, so it is the
        // missing resource — the refusal names the database.
        let (status, response) = warehouses
            .data("ListSchemas", &connect(target, "no-such-database"))
            .await;
        assert_eq!(
            status, 404,
            "case {label}: a database the warehouse does not hold is a missing resource, got \
             {status} {response}"
        );
        assert_eq!(
            message(&response),
            Some("Database does not exist: no-such-database"),
            "case {label}: expected the missing database to be named, got {response}"
        );
    }
}

/// A bad target selection is judged against the targets it NAMED: refused on the
/// selection as long as one of them exists, and as naming no endpoint when none
/// of them does.
#[tokio::test]
async fn a_bad_target_selection_is_refused_before_any_catalogue_is_read() {
    let warehouses = Warehouses::new();
    warehouses.create_cluster("present-cluster").await;

    // The rows differ in how many warehouses the request names and in whether
    // the ones it names exist in the Region.
    let selections: [(&str, Value, &str); 4] = [
        ("no target at all", json!({}), WAREHOUSE_ONE_TARGET),
        (
            "both warehouses, the cluster present",
            json!({"ClusterIdentifier": "present-cluster", "WorkgroupName": "absent-workgroup"}),
            WAREHOUSE_ONE_TARGET,
        ),
        (
            "both warehouses, neither present",
            json!({"ClusterIdentifier": "absent-cluster", "WorkgroupName": "absent-workgroup"}),
            NO_ENDPOINT,
        ),
        (
            "one warehouse, which is not there",
            json!({"ClusterIdentifier": "absent-cluster"}),
            NO_ENDPOINT,
        ),
    ];

    for (case, selection, sentence) in &selections {
        let mut request = json!({"Database": "dev", "DbUser": "awsuser"});
        for (member, value) in selection.as_object().expect("an object of members") {
            request[member] = value.clone();
        }
        let (status, response) = warehouses.data("ListSchemas", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {response}"
        );
        assert_eq!(
            message(&response),
            Some(*sentence),
            "case {case:?}: expected {sentence:?}, got {response}"
        );
    }
}
