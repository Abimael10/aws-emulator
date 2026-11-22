// SPDX-License-Identifier: CC0-1.0

//! `GetDatabase` over the databases the AWS Glue Data Catalog holds.
//!
//! The rule under test, from the operation's own documentation ("returns a database object
//! for the specified database and data catalog") and from `Database`'s members ("the name of
//! the database", "an optional description of the database", "a set of custom key/value
//! pairs"): Athena registers no databases of its own, so for `AwsDataCatalog` — which IS the
//! account's Glue Data Catalog — the database this read answers with is the one that catalog
//! holds and every member it reports for it is that catalog's; the name resolves the way the
//! catalog resolves one, without regard to case; a database the catalog does not hold is the
//! metastore's not-found answer; and the data catalog a client REGISTERS decides where the
//! metadata comes from, exactly as it does for the listing of the same databases.
//!
//! Every database here is created over GLUE's real wire (`AWSGlue.CreateDatabase`) against
//! the same shared engine both services are mounted on, and every Athena call goes over
//! Athena's own wire through the same `MultiServiceRouter` the production front door uses.
//!
//! The suites are tables: each row is one input along the axis the rule generalises over
//! (the member set a database defines, the casing of the name asked for, the type of the
//! catalog, and the names the catalog holds nothing for).

use serde_json::{json, Value};

use crate::table_metadata::{athena, athena_ok, emulator, glue_ok, GLUE_CATALOG};

/// The database an account's Data Catalog HAS: every catalog carries it, so it is readable
/// without anything having created it.
const CATALOG_DEFAULT_DATABASE: &str = "default";

/// The awsJson error code of a failure body (`__type`), which may carry a namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `Message` member of a failure body, which carries the refusal's wording.
fn error_message(body: &Value) -> String {
    body.get("Message")
        .and_then(Value::as_str)
        .unwrap_or("<no Message>")
        .to_string()
}

/// Every member this read reports for a database is the one the CATALOG holds for it, and a
/// member the catalog holds nothing for is absent rather than reported empty.
///
/// The rows differ along the member set a `DatabaseInput` defines: a description and
/// properties, a description alone, properties alone, and neither.
#[tokio::test]
async fn the_database_read_reports_what_the_catalog_holds() {
    let router = emulator();

    // (case, the DatabaseInput created, the Database this read must answer with)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "a database carrying a description and properties",
            json!({
                "Name": "read_logs",
                "Description": "CloudFront distribution logs",
                "Parameters": { "owner": "analytics", "tier": "gold" },
            }),
            json!({
                "Name": "read_logs",
                "Description": "CloudFront distribution logs",
                "Parameters": { "owner": "analytics", "tier": "gold" },
            }),
        ),
        (
            "a database carrying only a description",
            json!({ "Name": "read_described", "Description": "nothing else set" }),
            json!({ "Name": "read_described", "Description": "nothing else set" }),
        ),
        (
            "a database carrying only properties",
            json!({ "Name": "read_parameters", "Parameters": { "classification": "csv" } }),
            json!({ "Name": "read_parameters", "Parameters": { "classification": "csv" } }),
        ),
        (
            "a database defining nothing beyond its name",
            json!({ "Name": "read_bare" }),
            json!({ "Name": "read_bare" }),
        ),
    ];

    for (_, input, _) in &rows {
        glue_ok(&router, "CreateDatabase", &json!({ "DatabaseInput": input })).await;
    }

    for (case, input, expected) in &rows {
        let name = input["Name"].as_str().unwrap_or_default();
        let answer = athena_ok(
            &router,
            "GetDatabase",
            &json!({ "CatalogName": GLUE_CATALOG, "DatabaseName": name }),
        )
        .await;
        assert_eq!(
            answer["Database"], *expected,
            "case {case:?}: expected the database {expected}, got {answer}"
        );
    }

    // The database every catalog carries is readable too, without anything having created
    // it.
    let held = athena_ok(
        &router,
        "GetDatabase",
        &json!({ "CatalogName": GLUE_CATALOG, "DatabaseName": CATALOG_DEFAULT_DATABASE }),
    )
    .await;
    assert_eq!(
        held["Database"]["Name"].as_str(),
        Some(CATALOG_DEFAULT_DATABASE),
        "the database every catalog carries must be readable, got {held}"
    );
}

/// The name is resolved the way the CATALOG resolves one — folded, so any casing the client
/// sends reaches the same database — and a name the catalog holds nothing for is the
/// metastore's not-found answer rather than an empty success.
#[tokio::test]
async fn the_name_resolves_the_way_the_catalog_resolves_one() {
    let router = emulator();
    glue_ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "MixedCaseDb", "Description": "folded" } }),
    )
    .await;

    // (case, the name asked for, the name the read must answer with)
    let found: Vec<(&str, &str)> = vec![
        ("the casing the client created it with", "MixedCaseDb"),
        ("all lower case", "mixedcasedb"),
        ("all upper case", "MIXEDCASEDB"),
    ];
    for (case, asked) in &found {
        let answer = athena_ok(
            &router,
            "GetDatabase",
            &json!({ "CatalogName": GLUE_CATALOG, "DatabaseName": asked }),
        )
        .await;
        assert_eq!(
            answer["Database"]["Description"].as_str(),
            Some("folded"),
            "case {case:?}: {asked} must reach the stored database, got {answer}"
        );
    }

    // The rows differ along why no database answers: a name nothing created, one a near
    // miss of a stored name, and one the catalog dropped again.
    glue_ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "dropped_db" } }),
    )
    .await;
    glue_ok(&router, "DeleteDatabase", &json!({ "Name": "dropped_db" })).await;
    let absent: Vec<(&str, &str)> = vec![
        ("a name nothing created", "no_such_database"),
        ("a near miss of a stored name", "mixedcasedbx"),
        ("a database the catalog dropped again", "dropped_db"),
    ];
    for (case, asked) in &absent {
        let (status, body) = athena(
            &router,
            "GetDatabase",
            &json!({ "CatalogName": GLUE_CATALOG, "DatabaseName": asked }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the read must be refused, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("MetadataException"),
            "case {case:?}: expected MetadataException, got {}",
            error_code(&body)
        );
        assert!(
            error_message(&body).contains(*asked),
            "case {case:?}: the message must name {asked:?}, got {:?}",
            error_message(&body)
        );
    }
}

/// A data catalog a client REGISTERS decides where this read's metadata comes from: a `GLUE`
/// catalog naming this account's catalog id reads the same database under a second name, a
/// catalog served by an external metastore or by another account's catalog answers the
/// metastore's error naming the obstacle it hit, and a name nothing registered is a bad
/// request naming the catalog.
#[tokio::test]
async fn a_registered_catalog_reads_the_catalog_its_parameters_name() {
    let router = emulator();
    glue_ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "registered_read_db", "Description": "held" } }),
    )
    .await;
    // The `catalog-id` a GLUE data catalog is registered with: Glue's own default, the
    // account id, which is the catalog this emulator holds.
    let account = engine::emulator_account_id().to_string();

    // (case, the DataCatalog to register, whether the read answers, the error code, a
    // fragment its message must name)
    let rows: Vec<(&str, Option<Value>, bool, &str, String)> = vec![
        (
            "a GLUE catalog naming this account's catalog id",
            Some(json!({
                "Name": "second_name_for_read",
                "Type": "GLUE",
                "Parameters": { "catalog-id": account.clone() }
            })),
            true,
            "",
            String::new(),
        ),
        (
            "a GLUE catalog naming another account's catalog id",
            Some(json!({
                "Name": "other_account_read",
                "Type": "GLUE",
                "Parameters": { "catalog-id": "210987654321" }
            })),
            false,
            "MetadataException",
            "210987654321".to_string(),
        ),
        (
            "a HIVE catalog, whose metadata an external metastore serves",
            Some(json!({
                "Name": "hive_read_metastore",
                "Type": "HIVE",
                "Parameters": {
                    "metadata-function": "arn:aws:lambda:us-east-1:000000000000:function:hms"
                }
            })),
            false,
            "MetadataException",
            "hive_read_metastore".to_string(),
        ),
        (
            "a LAMBDA catalog, whose metadata a connector function serves",
            Some(json!({
                "Name": "lambda_read_connector",
                "Type": "LAMBDA",
                "Parameters": {
                    "function": "arn:aws:lambda:us-east-1:000000000000:function:connector"
                }
            })),
            false,
            "MetadataException",
            "lambda_read_connector".to_string(),
        ),
        (
            "a catalog name nothing registered",
            None,
            false,
            "InvalidRequestException",
            "nothing_registered_read".to_string(),
        ),
    ];

    for (case, create, answers, code, fragment) in &rows {
        let catalog = match create {
            Some(create) => {
                athena_ok(&router, "CreateDataCatalog", create).await;
                create["Name"].as_str().unwrap_or_default().to_string()
            }
            None => "nothing_registered_read".to_string(),
        };
        let request = json!({
            "CatalogName": catalog,
            "DatabaseName": "registered_read_db",
        });
        if *answers {
            let answer = athena_ok(&router, "GetDatabase", &request).await;
            assert_eq!(
                answer["Database"]["Description"].as_str(),
                Some("held"),
                "case {case:?}: expected the database this account's catalog holds, got \
                 {answer}"
            );
            continue;
        }
        let (status, body) = athena(&router, "GetDatabase", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the read must be refused, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains(code),
            "case {case:?}: expected {code}, got {}",
            error_code(&body)
        );
        assert!(
            error_message(&body).contains(fragment.as_str()),
            "case {case:?}: the message must name {fragment:?}, got {:?}",
            error_message(&body)
        );
    }
}
