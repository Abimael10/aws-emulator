// SPDX-License-Identifier: CC0-1.0

//! `GetDatabase` as a REPORT of the data catalog's current database record.
//!
//! The rule under test, from the operation's own documentation ("returns a database object
//! for the specified database and data catalog") and from `ListDatabases` ("lists the
//! databases in the specified data catalog"): a metadata read of one database answers with
//! the record the addressed catalog holds for it AT THE TIME OF THE CALL, projected exactly
//! as every other report of that same record projects it — so the object `GetDatabase`
//! answers with for a name is the very object the listing of the same catalog carries for
//! that name, a later write through the catalog's own API is visible to the next read, and
//! reading changes nothing a later caller sees.
//!
//! Every database here is created, updated and dropped over GLUE's real wire
//! (`AWSGlue.CreateDatabase` and friends) against the same shared engine both services are
//! mounted on, and every Athena call goes over Athena's own wire through the same
//! `MultiServiceRouter` the production front door uses — so what is asserted is HTTP
//! behaviour, not a direct call into a handler.
//!
//! The suites are tables: each row is one input along the axis the rule generalises over
//! (the member set a database carries, the catalog name the read addresses, the write that
//! last changed the record, and the workgroup the read is fetched under).

use serde_json::{json, Value};

use crate::table_metadata::{athena, athena_ok, emulator, glue_ok, GLUE_CATALOG};

/// The database an account's Data Catalog HAS, which is reported beside the created ones.
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

/// The `DatabaseInput` member sets the databases of these suites are created with: the axis
/// the projection generalises over — a description and properties, a description alone,
/// properties alone, and nothing beyond the name.
fn created_databases() -> Vec<Value> {
    vec![
        json!({
            "Name": "agree_full",
            "Description": "access logs of the edge fleet",
            "Parameters": { "owner": "analytics", "tier": "gold" },
        }),
        json!({ "Name": "agree_described", "Description": "nothing else set" }),
        json!({ "Name": "agree_parameters", "Parameters": { "classification": "json" } }),
        json!({ "Name": "agree_bare" }),
    ]
}

/// Every `Database` the listing of `catalog` reports, paged at `page_size` so the walk
/// crosses page boundaries, in the order the listing serves them.
async fn listed_databases(
    router: &smithy_server_runtime::MultiServiceRouter,
    catalog: &str,
    page_size: u32,
) -> Vec<Value> {
    let mut collected: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({ "CatalogName": catalog, "MaxResults": page_size });
        if let Some(cursor) = &token {
            request["NextToken"] = json!(cursor);
        }
        let answer = athena_ok(router, "ListDatabases", &request).await;
        let page = answer["DatabaseList"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            page.len() <= page_size as usize,
            "a page must hold at most {page_size} databases, got {answer}"
        );
        collected.extend(page);
        token = answer["NextToken"].as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            collected.len() < 64,
            "the listing of {catalog:?} must terminate, got {} entries",
            collected.len()
        );
    }
    collected
}

/// The object this read answers with for a name is the object the LISTING of the same
/// catalog carries for that name, member for member — the same projection of one stored
/// record, whichever metadata read addresses it.
///
/// The rows differ along two axes at once: the member set each database carries, and the
/// name the read addresses the account's catalog by (its reserved name, and a `GLUE` data
/// catalog a client registered for the same catalog id).
#[tokio::test]
async fn the_database_read_answers_what_the_listing_reports() {
    let router = emulator();
    for input in created_databases() {
        glue_ok(&router, "CreateDatabase", &json!({ "DatabaseInput": input })).await;
    }
    // A second name for the SAME catalog: a `GLUE` data catalog registered with Glue's own
    // default `catalog-id`, this account's id.
    let account = engine::emulator_account_id().to_string();
    athena_ok(
        &router,
        "CreateDataCatalog",
        &json!({
            "Name": "agreement_alias",
            "Type": "GLUE",
            "Parameters": { "catalog-id": account },
        }),
    )
    .await;

    for catalog in [GLUE_CATALOG, "agreement_alias"] {
        // Paged at two, so the listing this is compared against is assembled across pages
        // rather than served whole.
        let listed = listed_databases(&router, catalog, 2).await;
        let names: Vec<&str> = listed
            .iter()
            .filter_map(|database| database["Name"].as_str())
            .collect();
        for expected in created_databases()
            .iter()
            .filter_map(|input| input["Name"].as_str().map(str::to_string))
            .chain([CATALOG_DEFAULT_DATABASE.to_string()])
        {
            assert!(
                names.contains(&expected.as_str()),
                "case {catalog:?}: the listing must report {expected:?}, got {names:?}"
            );
        }

        for database in &listed {
            let name = database["Name"].as_str().unwrap_or_default();
            let answer = athena_ok(
                &router,
                "GetDatabase",
                &json!({ "CatalogName": catalog, "DatabaseName": name }),
            )
            .await;
            assert_eq!(
                answer["Database"], *database,
                "case {catalog:?}/{name:?}: the read must answer the listed database \
                 {database}, got {answer}"
            );
        }
    }
}

/// The record the read answers with is the catalog's CURRENT one: a write through the
/// catalog's own API is visible to the next read, including one that takes a member away,
/// and a database the catalog no longer holds stops answering.
///
/// The rows differ along the write that last changed the record.
#[tokio::test]
async fn the_database_read_reports_the_record_the_catalog_holds_now() {
    let router = emulator();
    glue_ok(
        &router,
        "CreateDatabase",
        &json!({
            "DatabaseInput": {
                "Name": "evolving_db",
                "Description": "as created",
                "Parameters": { "stage": "raw" },
            }
        }),
    )
    .await;

    // (case, the DatabaseInput the update submits, the Database the next read must answer)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "the definition the create recorded",
            Value::Null,
            json!({
                "Name": "evolving_db",
                "Description": "as created",
                "Parameters": { "stage": "raw" },
            }),
        ),
        (
            "a description the update changed",
            json!({
                "Name": "evolving_db",
                "Description": "curated now",
                "Parameters": { "stage": "raw" },
            }),
            json!({
                "Name": "evolving_db",
                "Description": "curated now",
                "Parameters": { "stage": "raw" },
            }),
        ),
        (
            "a property set the update replaced",
            json!({
                "Name": "evolving_db",
                "Description": "curated now",
                "Parameters": { "stage": "curated", "retention": "30" },
            }),
            json!({
                "Name": "evolving_db",
                "Description": "curated now",
                "Parameters": { "stage": "curated", "retention": "30" },
            }),
        ),
        (
            "a definition the update narrowed to the name alone",
            json!({ "Name": "evolving_db" }),
            json!({ "Name": "evolving_db" }),
        ),
        (
            "a description the update put back",
            json!({ "Name": "evolving_db", "Description": "described again" }),
            json!({ "Name": "evolving_db", "Description": "described again" }),
        ),
    ];

    for (case, update, expected) in &rows {
        if let Some(input) = update.as_object() {
            glue_ok(
                &router,
                "UpdateDatabase",
                &json!({ "Name": "evolving_db", "DatabaseInput": input }),
            )
            .await;
        }
        let answer = athena_ok(
            &router,
            "GetDatabase",
            &json!({ "CatalogName": GLUE_CATALOG, "DatabaseName": "evolving_db" }),
        )
        .await;
        assert_eq!(
            answer["Database"], *expected,
            "case {case:?}: the read must answer {expected}, got {answer}"
        );
    }

    glue_ok(&router, "DeleteDatabase", &json!({ "Name": "evolving_db" })).await;
    let (status, body) = athena(
        &router,
        "GetDatabase",
        &json!({ "CatalogName": GLUE_CATALOG, "DatabaseName": "evolving_db" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a database the catalog dropped must stop answering, got {status} with {body}"
    );
    assert!(
        error_code(&body).contains("MetadataException"),
        "a database the catalog dropped is the metastore's answer, got {}",
        error_code(&body)
    );
}

/// The read REPORTS: neither a hit nor a miss changes what a later caller sees, and the
/// workgroup the metadata is fetched under does not change the record reported.
///
/// The rows differ along the workgroup member the read carries: absent, and the workgroup
/// every account starts with.
#[tokio::test]
async fn the_database_read_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    for input in created_databases() {
        glue_ok(&router, "CreateDatabase", &json!({ "DatabaseInput": input })).await;
    }
    let before = listed_databases(&router, GLUE_CATALOG, 50).await;

    // (case, the WorkGroup the read carries)
    let rows: Vec<(&str, Option<&str>)> = vec![
        ("no workgroup", None),
        ("the workgroup every account starts with", Some("primary")),
    ];
    let mut answers: Vec<Value> = Vec::new();
    for (case, work_group) in &rows {
        // Twice per row: a second identical read must answer identically, so nothing of
        // the first read is consumed.
        for attempt in 1..=2 {
            let mut request = json!({
                "CatalogName": GLUE_CATALOG,
                "DatabaseName": "agree_full",
            });
            if let Some(work_group) = work_group {
                request["WorkGroup"] = json!(work_group);
            }
            let answer = athena_ok(&router, "GetDatabase", &request).await;
            assert_eq!(
                answer["Database"]["Name"].as_str(),
                Some("agree_full"),
                "case {case:?} read {attempt}: the read must answer the database, got {answer}"
            );
            answers.push(answer["Database"].clone());
        }
    }
    let first = answers.first().cloned().unwrap_or(Value::Null);
    for (index, answer) in answers.iter().enumerate() {
        assert_eq!(
            *answer, first,
            "read {index}: every read of one record must answer the same object {first}, got \
             {answer}"
        );
    }

    // A miss leaves the catalog exactly as it was: nothing is created for the name asked
    // for, so the listing does not grow and the name stays absent.
    let (status, body) = athena(
        &router,
        "GetDatabase",
        &json!({ "CatalogName": GLUE_CATALOG, "DatabaseName": "never_created_db" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a name the catalog holds nothing for must be refused, got {status} with {body}"
    );
    assert!(
        error_message(&body).contains("never_created_db"),
        "the refusal must name the database asked for, got {:?}",
        error_message(&body)
    );

    let after = listed_databases(&router, GLUE_CATALOG, 50).await;
    assert_eq!(
        after, before,
        "the databases a later caller sees must be unchanged by reading, expected {before:?}, \
         got {after:?}"
    );
}
