// SPDX-License-Identifier: CC0-1.0

//! `ListDatabases` over the databases the AWS Glue Data Catalog holds.
//!
//! The rule under test, from the operation's own documentation ("lists the databases in the
//! specified data catalog") and from `Database`'s members ("the name of the database", "an
//! optional description of the database", "a set of custom key/value pairs"): Athena
//! registers no databases of its own, so for `AwsDataCatalog` — which IS the account's Glue
//! Data Catalog — every database this listing reports is one that catalog holds and every
//! member it reports for one is that catalog's, whatever the database defines and however
//! many the catalog holds. `MaxResults` with the returned `NextToken` then pages the
//! catalog's own name order without dropping or repeating a name, and a data catalog a
//! client REGISTERS decides where the listing's metadata comes from: a `GLUE` catalog naming
//! this account's catalog id lists the same databases under a second name, one served by an
//! external metastore or by another account's catalog answers the metastore's error, and a
//! name nothing registered is a problem with the input.
//!
//! Every database here is created over GLUE's real wire (`AWSGlue.CreateDatabase`) against
//! the same shared engine both services are mounted on, and every Athena call goes over
//! Athena's own wire through the same `MultiServiceRouter` the production front door uses —
//! so what is asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The suites are tables: each row is one input along the axis the rule generalises over
//! (the member set a database defines, how many the catalog holds, the page size, the type
//! of the catalog), and the recorded case — a Glue database Athena's metadata reflects — is
//! one row among them.

use serde_json::{json, Value};

use crate::table_metadata::{athena, athena_ok, emulator, glue_ok, GLUE_CATALOG};

/// The database an account's Data Catalog HAS: every catalog holds it, so every listing
/// reports it beside the databases a client created.
const CATALOG_DEFAULT_DATABASE: &str = "default";

/// The `Name`s of a `DatabaseList`, in the order the service returned them.
fn reported_names(page: &Value) -> Vec<String> {
    page["DatabaseList"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a DatabaseList, got {page}"))
        .iter()
        .map(|database| {
            database["Name"]
                .as_str()
                .unwrap_or_else(|| panic!("expected a database Name, got {database}"))
                .to_string()
        })
        .collect()
}

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

/// One listed database, found by name.
fn reported(page: &Value, name: &str) -> Value {
    page["DatabaseList"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a DatabaseList, got {page}"))
        .iter()
        .find(|database| database["Name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("expected the listing to report {name}, got {page}"))
        .clone()
}

/// Every member a listing reports for a database is the one the CATALOG holds for it, and a
/// member the catalog holds nothing for is absent rather than reported empty.
///
/// The rows differ along the member set a `DatabaseInput` defines: a description and
/// properties, a description alone, properties alone, and neither.
#[tokio::test]
async fn every_member_reported_is_the_one_the_catalog_holds() {
    let router = emulator();

    // (case, the DatabaseInput created, the Database the listing must report for it)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "a database carrying a description and properties",
            json!({
                "Name": "cloudfront_logs",
                "Description": "CloudFront distribution logs",
                "Parameters": { "owner": "analytics", "tier": "gold" },
            }),
            json!({
                "Name": "cloudfront_logs",
                "Description": "CloudFront distribution logs",
                "Parameters": { "owner": "analytics", "tier": "gold" },
            }),
        ),
        (
            "a database carrying only a description",
            json!({ "Name": "described_only", "Description": "nothing else set" }),
            json!({ "Name": "described_only", "Description": "nothing else set" }),
        ),
        (
            "a database carrying only properties",
            json!({ "Name": "parameters_only", "Parameters": { "classification": "csv" } }),
            json!({ "Name": "parameters_only", "Parameters": { "classification": "csv" } }),
        ),
        (
            "a database defining nothing beyond its name",
            json!({ "Name": "bare_database" }),
            json!({ "Name": "bare_database" }),
        ),
    ];

    for (case, input, _) in &rows {
        glue_ok(&router, "CreateDatabase", &json!({ "DatabaseInput": input })).await;
        let _ = case;
    }

    let page = athena_ok(
        &router,
        "ListDatabases",
        &json!({ "CatalogName": GLUE_CATALOG }),
    )
    .await;

    for (case, _, expected) in &rows {
        let name = expected["Name"].as_str().expect("the case names a database");
        let actual = reported(&page, name);
        assert_eq!(
            &actual, expected,
            "case {case:?}: expected the catalog's own {expected}, got {actual}"
        );
    }

    // The database every Data Catalog HAS is reported beside them, and the whole listing is
    // the catalog's name order.
    let names = reported_names(&page);
    let mut expected: Vec<String> = rows
        .iter()
        .map(|(_, _, database)| database["Name"].as_str().unwrap_or_default().to_string())
        .collect();
    expected.push(CATALOG_DEFAULT_DATABASE.to_string());
    expected.sort();
    assert_eq!(
        names, expected,
        "the listing must report every database the catalog holds in name order, got {names:?}"
    );
    assert!(
        page.get("NextToken").is_none() || page["NextToken"].is_null(),
        "a listing that reported every database names no next page, got {page}"
    );
}

/// `MaxResults` with the returned `NextToken` pages the catalog's own order, dropping and
/// repeating no name — whatever the page size and however it divides the stored count.
///
/// The rows differ along the page size: one that divides the count, one that does not, one
/// of exactly the count, one larger than it, and the largest the model takes.
#[tokio::test]
async fn the_page_size_and_token_page_the_catalogs_own_order() {
    let router = emulator();
    // Five databases of this suite's own, plus the one every catalog has.
    for name in ["alpha_db", "beta_db", "delta_db", "epsilon_db", "gamma_db"] {
        glue_ok(
            &router,
            "CreateDatabase",
            &json!({ "DatabaseInput": { "Name": name } }),
        )
        .await;
    }
    let whole = reported_names(
        &athena_ok(
            &router,
            "ListDatabases",
            &json!({ "CatalogName": GLUE_CATALOG }),
        )
        .await,
    );
    assert_eq!(
        whole.len(),
        6,
        "setup: the catalog must hold the five created databases and its own, got {whole:?}"
    );

    // The page size `MaxDatabasesCount` ranges over is 1..50; the rows span that range's
    // lower bound, sizes either side of the stored count, and its upper bound.
    for page_size in [1, 2, 3, 6, 7, 50] {
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut request = json!({ "CatalogName": GLUE_CATALOG, "MaxResults": page_size });
            if let Some(cursor) = &token {
                request["NextToken"] = json!(cursor);
            }
            let page = athena_ok(&router, "ListDatabases", &request).await;
            let names = reported_names(&page);
            assert!(
                names.len() <= page_size,
                "case page size {page_size}: a page holds at most that many, got {names:?}"
            );
            seen.extend(names);
            pages += 1;
            assert!(
                pages <= whole.len() + 1,
                "case page size {page_size}: paging must terminate, saw {seen:?}"
            );
            match page["NextToken"].as_str() {
                Some(cursor) => token = Some(cursor.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, whole,
            "case page size {page_size}: paging must report the catalog's order exactly once \
             each, got {seen:?}"
        );
    }
}

/// One REGISTERED data catalog under test: what registering it takes, and what the listing
/// then answers for it.
struct Registered {
    /// What the row is.
    case: &'static str,
    /// The `CreateDataCatalog` request that registers it, or `None` for a catalog name
    /// nothing registers.
    create: Option<Value>,
    /// The database names the listing must then report, or `None` when it must be refused.
    expected: Option<Vec<String>>,
    /// The awsJson error code a refusal must carry.
    code: &'static str,
    /// The fragment a refusal's message must name, which is the obstacle it reports.
    fragment: &'static str,
}

/// A data catalog a client REGISTERS decides where the listing's metadata comes from: a
/// `GLUE` catalog naming this account's catalog id lists the same databases under a second
/// name, a catalog served by an external metastore or by another account's catalog answers
/// the metastore's error naming the obstacle it hit, and a name nothing registered is a bad
/// request naming the catalog.
#[tokio::test]
async fn a_registered_catalog_lists_the_catalog_its_parameters_name() {
    let router = emulator();
    glue_ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "registered_listing_db" } }),
    )
    .await;
    // The `catalog-id` a GLUE data catalog is registered with: Glue's own default, the
    // account id, which is the catalog this emulator holds.
    let account = engine::emulator_account_id().to_string();
    let held = vec![
        CATALOG_DEFAULT_DATABASE.to_string(),
        "registered_listing_db".to_string(),
    ];

    // The rows span where a registered catalog's metadata comes from.
    let rows: Vec<Registered> = vec![
        Registered {
            case: "a GLUE catalog naming this account's catalog id",
            create: Some(json!({
                "Name": "second_name_for_listing",
                "Type": "GLUE",
                "Parameters": { "catalog-id": account }
            })),
            expected: Some(held.clone()),
            code: "",
            fragment: "",
        },
        Registered {
            case: "a GLUE catalog naming another account's catalog id",
            create: Some(json!({
                "Name": "other_account_listing",
                "Type": "GLUE",
                "Parameters": { "catalog-id": "210987654321" }
            })),
            expected: None,
            code: "MetadataException",
            fragment: "210987654321",
        },
        Registered {
            case: "a HIVE catalog, whose metadata an external metastore serves",
            create: Some(json!({
                "Name": "hive_listing_metastore",
                "Type": "HIVE",
                "Parameters": {
                    "metadata-function": "arn:aws:lambda:us-east-1:000000000000:function:hms"
                }
            })),
            expected: None,
            code: "MetadataException",
            fragment: "hive_listing_metastore",
        },
        Registered {
            case: "a LAMBDA catalog, whose metadata a connector function serves",
            create: Some(json!({
                "Name": "lambda_listing_connector",
                "Type": "LAMBDA",
                "Parameters": {
                    "function": "arn:aws:lambda:us-east-1:000000000000:function:connector"
                }
            })),
            expected: None,
            code: "MetadataException",
            fragment: "lambda_listing_connector",
        },
        Registered {
            case: "a catalog name nothing registered",
            create: None,
            expected: None,
            code: "InvalidRequestException",
            fragment: "nothing_registered_this",
        },
    ];

    for Registered {
        case,
        create,
        expected,
        code,
        fragment,
    } in rows
    {
        let catalog = match &create {
            Some(create) => {
                athena_ok(&router, "CreateDataCatalog", create).await;
                create["Name"].as_str().expect("the case names a catalog").to_string()
            }
            None => "nothing_registered_this".to_string(),
        };
        let request = json!({ "CatalogName": catalog });
        match expected {
            Some(expected) => {
                let page = athena_ok(&router, "ListDatabases", &request).await;
                assert_eq!(
                    reported_names(&page),
                    expected,
                    "case {case:?}: expected {expected:?}, got {:?}",
                    reported_names(&page)
                );
            }
            None => {
                let (status, body) = athena(&router, "ListDatabases", &request).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: the listing must be refused, got {status} with {body}"
                );
                assert!(
                    error_code(&body).contains(code),
                    "case {case:?}: expected {code}, got {}",
                    error_code(&body)
                );
                assert!(
                    error_message(&body).contains(fragment),
                    "case {case:?}: the message must name {fragment:?}, got {:?}",
                    error_message(&body)
                );
            }
        }
    }
}

/// A database created AFTER a first listing is reported by the next one, and one the catalog
/// dropped stops being reported: the listing answers out of Glue's store rather than a
/// snapshot of its own.
#[tokio::test]
async fn the_listing_follows_the_catalog() {
    let router = emulator();
    let names = |page: &Value| reported_names(page);

    glue_ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "moving_listing_db" } }),
    )
    .await;
    let before = athena_ok(
        &router,
        "ListDatabases",
        &json!({ "CatalogName": GLUE_CATALOG }),
    )
    .await;
    assert_eq!(
        names(&before),
        [CATALOG_DEFAULT_DATABASE, "moving_listing_db"],
        "the first listing must report the stored databases, got {:?}",
        names(&before)
    );

    glue_ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "zebra_listing_db" } }),
    )
    .await;
    let after = athena_ok(
        &router,
        "ListDatabases",
        &json!({ "CatalogName": GLUE_CATALOG }),
    )
    .await;
    assert_eq!(
        names(&after),
        [
            CATALOG_DEFAULT_DATABASE,
            "moving_listing_db",
            "zebra_listing_db"
        ],
        "a database created after the first listing must be reported, got {:?}",
        names(&after)
    );

    glue_ok(
        &router,
        "DeleteDatabase",
        &json!({ "Name": "moving_listing_db" }),
    )
    .await;
    let dropped = athena_ok(
        &router,
        "ListDatabases",
        &json!({ "CatalogName": GLUE_CATALOG }),
    )
    .await;
    assert_eq!(
        names(&dropped),
        [CATALOG_DEFAULT_DATABASE, "zebra_listing_db"],
        "a database the catalog dropped must stop being reported, got {:?}",
        names(&dropped)
    );
}
