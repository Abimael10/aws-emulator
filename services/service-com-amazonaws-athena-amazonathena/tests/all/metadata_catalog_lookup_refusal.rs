// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a catalog-METADATA read answers when the `CatalogName` it
//! carries names no data catalog this account holds.
//!
//! The rule: every metadata read resolves that name through the same step before it reads
//! anything — `ListDatabases` ("lists the databases in the specified data catalog"),
//! `ListTableMetadata` ("lists the metadata for the tables in the specified data catalog
//! database") and `GetTableMetadata` ("returns table metadata for the specified catalog,
//! database, and table") — so a name nothing registered is refused by that step, with the
//! operation's own `InvalidRequestException` (HTTP 400) under this service's
//! `INVALID_INPUT` class and ONE sentence: Athena's metadata plane states that it cannot
//! find the catalog, naming the `CatalogName` the request carried and nothing else. The
//! sentence is therefore the same whichever metadata read asked, and it is NOT the
//! registry read's own wording for a missing catalog RECORD (`GetDataCatalog`), which this
//! suite also pins as a different sentence so neither can drift onto the other.
//!
//! Asserted over a TABLE: the rows span the metadata reads and the forms an unregistered
//! catalog name takes (one character, the documented punctuation, mixed case with digits,
//! the longest name the documented bound allows, a name differing only in case from one
//! this account holds, a name a delete removed, and one that only looks like the Glue Data
//! Catalog's) — because the sentence is built from the request rather than from any one
//! name. Athena and Glue are mounted on one shared engine, so the rows that must be SERVED
//! are served out of the catalog that really holds the databases, and every call goes over
//! the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the same router
//! the emulator boots.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, ERROR_CODE_INVALID_INPUT, MAX_DATA_CATALOG_NAME_CHARS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, message};
use crate::table_metadata::{athena, athena_ok, emulator, glue_ok, GLUE_CATALOG};

/// The database the rows that are SERVED read through: one this suite creates in the Data
/// Catalog itself, so a served listing reports a database that really exists.
const HELD_DATABASE: &str = "metadata_lookup_db";

/// A catalog name this account really registers, so the rows that differ from it only in
/// case or only by a delete are genuine misses rather than names nothing ever held.
const HELD_CATALOG: &str = "metadata_lookup_catalog";

/// A catalog name this account registers and then removes, so one row asks about a name
/// the registry held and no longer does.
const REMOVED_CATALOG: &str = "metadata_lookup_removed";

/// One metadata read: what it is called and the body it is handed for a catalog name.
struct MetadataRead {
    operation: &'static str,
    body: fn(&str) -> Value,
}

/// The reads that resolve a `CatalogName` before they read anything.
fn metadata_reads() -> Vec<MetadataRead> {
    vec![
        MetadataRead {
            operation: "ListDatabases",
            body: |catalog| json!({ "CatalogName": catalog }),
        },
        MetadataRead {
            operation: "ListTableMetadata",
            body: |catalog| {
                json!({ "CatalogName": catalog, "DatabaseName": HELD_DATABASE })
            },
        },
        MetadataRead {
            operation: "GetTableMetadata",
            body: |catalog| {
                json!({
                    "CatalogName": catalog,
                    "DatabaseName": HELD_DATABASE,
                    "TableName": "metadata_lookup_table",
                })
            },
        },
    ]
}

/// The catalog names no registration holds, each a different form a client can send:
/// lengths and characters from the `CreateDataCatalogInput$Name` documentation
/// ("alphanumeric, underscore, at sign, or hyphen characters", at most
/// [`MAX_DATA_CATALOG_NAME_CHARS`] of them), plus the near misses a registry holding
/// catalogs produces.
fn unregistered_names() -> Vec<(&'static str, String)> {
    vec![
        ("a single character", "c".to_string()),
        (
            "the documented punctuation",
            "metadata_lookup@absent-catalog".to_string(),
        ),
        ("mixed case with digits", "Catalog9Absent".to_string()),
        (
            "the longest name the documented bound allows",
            "l".repeat(MAX_DATA_CATALOG_NAME_CHARS),
        ),
        (
            "a name differing only in case from one this account holds",
            HELD_CATALOG.to_uppercase(),
        ),
        ("a name a delete removed", REMOVED_CATALOG.to_string()),
        (
            "a name that only looks like the Glue Data Catalog's",
            format!("{GLUE_CATALOG}ue"),
        ),
    ]
}

/// Mount Athena beside Glue, create the database the served rows read, register
/// [`HELD_CATALOG`] and register and then remove [`REMOVED_CATALOG`].
async fn router_with_a_registry() -> MultiServiceRouter {
    let router = emulator();
    glue_ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": HELD_DATABASE } }),
    )
    .await;
    // Registered as a GLUE catalog naming the catalog id this emulator holds — Glue's own
    // default, the account id — so the catalog it names is a catalog that exists.
    let account = engine::emulator_account_id().to_string();
    for name in [HELD_CATALOG, REMOVED_CATALOG] {
        athena_ok(
            &router,
            "CreateDataCatalog",
            &json!({
                "Name": name,
                "Type": state::DATA_CATALOG_TYPE_GLUE,
                "Parameters": { "catalog-id": account },
            }),
        )
        .await;
    }
    athena_ok(
        &router,
        "DeleteDataCatalog",
        &json!({ "Name": REMOVED_CATALOG }),
    )
    .await;
    router
}

#[tokio::test]
async fn every_metadata_read_refuses_an_unregistered_catalog_with_the_metadata_sentence() {
    let router = router_with_a_registry().await;

    for (form, name) in unregistered_names() {
        for read in metadata_reads() {
            let (status, answer) = athena(&router, read.operation, &(read.body)(&name)).await;
            let case = format!("{} on {}, {form}", read.operation, name);
            assert_refusal(
                &case,
                status,
                &answer,
                ERROR_CODE_INVALID_INPUT,
                &state::metadata_catalog_not_found(&name),
            );
        }
    }
}

#[tokio::test]
async fn the_metadata_sentence_is_one_sentence_and_not_the_registry_reads() {
    let router = router_with_a_registry().await;

    for (form, name) in unregistered_names() {
        // Every metadata read states the SAME sentence for the same name: the miss belongs
        // to the resolution step, not to the operation that hit it.
        let mut said: Vec<(&str, String)> = Vec::new();
        for read in metadata_reads() {
            let (status, answer) = athena(&router, read.operation, &(read.body)(&name)).await;
            assert_eq!(
                status, 400,
                "case {} on {name} ({form}): expected HTTP 400, got {status} with {answer:?}",
                read.operation
            );
            said.push((read.operation, message(&answer)));
        }
        let (first_operation, first) = said[0].clone();
        for (operation, other) in &said[1..] {
            assert_eq!(
                &first, other,
                "case {operation} on {name} ({form}): every metadata read states one \
                 sentence, {first_operation} said {first:?} and this one {other:?}"
            );
        }

        // The REGISTRY read of the same absent name states its own, different sentence.
        let (status, answer) = athena(&router, "GetDataCatalog", &json!({ "Name": name })).await;
        assert_refusal(
            &format!("GetDataCatalog on {name}, {form}"),
            status,
            &answer,
            ERROR_CODE_INVALID_INPUT,
            &state::data_catalog_not_found(&name),
        );
        assert_ne!(
            message(&answer),
            first,
            "case GetDataCatalog on {name} ({form}): the registry read's sentence must \
             differ from the metadata reads', both said {first:?}"
        );
    }
}

#[tokio::test]
async fn a_registered_catalog_is_still_read_through() {
    let router = router_with_a_registry().await;

    // The control: the names the registry DOES hold are served, so the refusals above are
    // about the absent catalog rather than about every metadata read.
    for catalog in [GLUE_CATALOG, HELD_CATALOG] {
        let page = athena_ok(&router, "ListDatabases", &json!({ "CatalogName": catalog })).await;
        let names: Vec<String> = page["DatabaseList"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|database| database["Name"].as_str().map(str::to_string))
            .collect();
        assert!(
            names.contains(&HELD_DATABASE.to_string()),
            "case {catalog}: the listing must report the database the catalog holds, got \
             {names:?}"
        );
    }
}
