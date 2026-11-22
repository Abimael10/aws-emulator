// SPDX-License-Identifier: CC0-1.0

//! The catalog-name grammar a `CatalogId` is measured against, on EVERY surface that
//! addresses a catalog resource by one: the two single-catalog reads and writes
//! (`GetCatalog`, `DeleteCatalog`, `UpdateCatalog`) and the listing's `ParentCatalogId`
//! (`GetCatalogs`).
//!
//! RULE under test: a `CatalogId` addresses a catalog by the path of catalog NAMES leading
//! to it, so every segment of that path has to satisfy the grammar a catalog's own `Name`
//! satisfies (`CatalogNameString`, `@pattern ^(?!(.*[.\/\\]|aws:)).*$`) — a segment outside
//! it names no catalog that could ever have been created, so the request is MALFORMED and is
//! refused with the constraint report AWS answers a value outside a `@pattern` with (HTTP
//! 400, `ValidationException`, the member path `name`, the expression echoed as the model
//! spells it), decided from the request alone and so ahead of authorization, of the account's
//! own Data Catalog and of the store. A segment the grammar ADMITS is an address that
//! resolves, whose miss is the catalog miss.
//!
//! [`crate::catalog_id_grammar`] pins the same rule on the read and the delete; this suite
//! pins that it is one rule over every surface that takes such an id, and that a refused call
//! leaves the store exactly as it was.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is the HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::catalog_id_grammar::{expected_refusal, CATALOG_NOT_FOUND};
use crate::catalog_property_updates::{get_catalog, given_linked_catalog_with_tags};
use crate::wire::{call, constraint_message, emulator, error_code, error_message, ok, ACCOUNT_ID};

/// An account OTHER than the one this endpoint serves, so an id naming it addresses a catalog
/// in a Data Catalog this store holds no grant on.
const FOREIGN_ACCOUNT_ID: &str = "210987654321";

/// The description the staged catalog is created with, so a later read proves no refused call
/// wrote over it.
const STAGED_DESCRIPTION: &str = "the definition the create stored";

/// The four requests that address a catalog resource by a `CatalogId`, each paired with the
/// operation name and the body that addresses `catalog_id` with it.
///
/// `UpdateCatalog`'s `CatalogInput` is the definition it would write; it is spelled empty here
/// because every row of this suite is decided before any definition is read or written.
fn catalog_addressing_surfaces(catalog_id: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("GetCatalog", json!({ "CatalogId": catalog_id })),
        ("DeleteCatalog", json!({ "CatalogId": catalog_id })),
        (
            "UpdateCatalog",
            json!({ "CatalogId": catalog_id, "CatalogInput": {} }),
        ),
        ("GetCatalogs", json!({ "ParentCatalogId": catalog_id })),
    ]
}

/// Assert the call `operation` with `body` is refused 400 with `code` and, where one is given,
/// exactly that wording in the member the refusal carries it in.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    code: &str,
    wording: Option<&str>,
) {
    let (status, answer) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?} on {operation}: expected the refusal's status 400, got {status} with \
         {answer}"
    );
    assert!(
        error_code(&answer).ends_with(code),
        "case {case:?} on {operation}: expected {code}, got {} in {answer}",
        error_code(&answer)
    );
    if let Some(wording) = wording {
        // A constraint verdict comes back in the lowercase `message` of the validation shape,
        // where a MODELLED Glue refusal carries its `Message`.
        let got = if code == "ValidationException" {
            constraint_message(&answer)
        } else {
            error_message(&answer)
        };
        assert_eq!(
            got, wording,
            "case {case:?} on {operation}: unexpected wording, got {got}"
        );
    }
}

/// Every surface that addresses a catalog by a `CatalogId` refuses an id whose name part is
/// outside the grammar as a MALFORMED request, with AWS's constraint report naming the
/// catalog's own `name` — and the refused calls read, wrote and removed nothing.
///
/// The rows differ along which forbidden character takes a segment outside the grammar, where
/// in the segment it sits, how many of them there are, which segment of a nested path fails,
/// and whether the id names an account at all — this one or another, whose authorization
/// refusal the verdict comes ahead of.
#[tokio::test]
async fn every_catalog_addressing_surface_refuses_a_name_outside_the_grammar() {
    let router = emulator();
    given_linked_catalog_with_tags(&router, "staged-catalog", &json!({ "owner": "analytics" }))
        .await;

    for (case, catalog_id, refused_segment) in [
        ("a dot inside the name", "lake.house".to_string(), "lake.house"),
        ("a dot at the end", "analytics.".to_string(), "analytics."),
        ("a dot at the start", ".analytics".to_string(), ".analytics"),
        (
            "several dots",
            "lake.house.2026".to_string(),
            "lake.house.2026",
        ),
        ("nothing but dots", "...".to_string(), "..."),
        (
            "a backslash",
            "lake\\house".to_string(),
            "lake\\house",
        ),
        ("the reserved prefix", "aws:lake".to_string(), "aws:lake"),
        (
            "the reserved prefix with nothing after it",
            "aws:".to_string(),
            "aws:",
        ),
        (
            "a nested path whose child is outside the grammar",
            "s3tablescatalog/bucket.name".to_string(),
            "bucket.name",
        ),
        (
            "a nested path whose holder is outside the grammar",
            "s3tables.catalog/bucket".to_string(),
            "s3tables.catalog",
        ),
        (
            "this account and a name outside the grammar",
            format!("{ACCOUNT_ID}:lake.house"),
            "lake.house",
        ),
        (
            "another account and a name outside the grammar",
            format!("{FOREIGN_ACCOUNT_ID}:lake.house"),
            "lake.house",
        ),
    ] {
        for (operation, body) in catalog_addressing_surfaces(&catalog_id) {
            assert_refused(
                &router,
                case,
                operation,
                &body,
                "ValidationException",
                Some(&expected_refusal(refused_segment)),
            )
            .await;
        }
    }

    // Nothing the refused calls touched: the staged catalog still carries the definition its
    // create stored, the account's own Data Catalog still reads back, and the listing still
    // reports the one catalog this account holds.
    let read = get_catalog(&router, "staged-catalog").await;
    assert_eq!(
        read.get("Description").and_then(Value::as_str),
        Some(STAGED_DESCRIPTION),
        "the refused calls must have written nothing, got {read}"
    );
    let root = get_catalog(&router, ACCOUNT_ID).await;
    assert_eq!(
        root.get("Name").and_then(Value::as_str),
        Some(ACCOUNT_ID),
        "the account's own Data Catalog must still read back, got {root}"
    );
    let page = ok(&router, "GetCatalogs", &json!({})).await;
    assert_eq!(
        page["CatalogList"].as_array().map_or(0, Vec::len),
        1,
        "the refused calls must have removed nothing, got {page}"
    );
}

/// An id every segment of which the grammar ADMITS is an address that RESOLVES, so the answer
/// is the one that id's own resolution earns and never the malformed-request verdict: the
/// catalog miss for a name this account holds no catalog under — including the nested form the
/// Data Catalog publishes, whose separator is part of the address — and the authorization
/// refusal for a catalog in another account's Data Catalog.
///
/// The rows differ along how deep the path nests, which near miss of the grammar the name
/// takes (the reserved letters without their colon, a colon that is no account prefix), and
/// which account the id names.
#[tokio::test]
async fn an_id_the_grammar_admits_resolves_on_every_surface() {
    let router = emulator();

    for (case, catalog_id, code) in [
        (
            "a bare name nothing created",
            "never-created".to_string(),
            "EntityNotFoundException",
        ),
        (
            "the nested form the Data Catalog publishes",
            "s3tablescatalog/analytics".to_string(),
            "EntityNotFoundException",
        ),
        (
            "three levels of nesting",
            "holder/middle/leaf".to_string(),
            "EntityNotFoundException",
        ),
        (
            "the reserved letters without their colon",
            "awscatalog".to_string(),
            "EntityNotFoundException",
        ),
        (
            "a colon that is no account prefix",
            "my-aws:catalog".to_string(),
            "EntityNotFoundException",
        ),
        (
            "this account and a name nothing created",
            format!("{ACCOUNT_ID}:never-created"),
            "EntityNotFoundException",
        ),
        (
            "another account's Data Catalog",
            format!("{FOREIGN_ACCOUNT_ID}:analytics"),
            "AccessDeniedException",
        ),
    ] {
        let wording = (code == "EntityNotFoundException").then_some(CATALOG_NOT_FOUND);
        for (operation, body) in catalog_addressing_surfaces(&catalog_id) {
            assert_refused(&router, case, operation, &body, code, wording).await;
        }
    }
}

/// The grammar check takes none of the answers the catalogs this account HOLDS have: a name
/// the grammar admits reads back, updates and reads the update back, and the account's own
/// Data Catalog keeps both of its own answers — so the refusals above turn on the grammar and
/// are not a blanket over every id.
///
/// The rows differ along the character classes a name the grammar admits is built from.
#[tokio::test]
async fn a_name_the_grammar_admits_still_round_trips() {
    let router = emulator();

    for (case, name) in [
        ("letters and hyphens", "admitted-catalog"),
        ("underscores and digits", "lake_house_2026"),
        ("a single character", "c"),
    ] {
        given_linked_catalog_with_tags(&router, name, &json!({ "owner": "analytics" })).await;

        let read = get_catalog(&router, name).await;
        assert_eq!(
            read.get("Name").and_then(Value::as_str),
            Some(name),
            "case {case}: the read must report the stored catalog, got {read}"
        );

        ok(
            &router,
            "UpdateCatalog",
            &json!({
                "CatalogId": name,
                "CatalogInput": { "Description": "stated by the update" },
            }),
        )
        .await;
        let read = get_catalog(&router, name).await;
        assert_eq!(
            read.get("Description").and_then(Value::as_str),
            Some("stated by the update"),
            "case {case}: the update must be readable back, got {read}"
        );
    }

    // The account's own Data Catalog: its id reads back the catalog every account has, and the
    // listing under it reports the catalogs the rows above registered.
    let root = get_catalog(&router, ACCOUNT_ID).await;
    assert_eq!(
        root.get("Name").and_then(Value::as_str),
        Some(ACCOUNT_ID),
        "the account's own id must report the Data Catalog every account has, got {root}"
    );
    let page = ok(
        &router,
        "GetCatalogs",
        &json!({ "ParentCatalogId": ACCOUNT_ID }),
    )
    .await;
    assert_eq!(
        page["CatalogList"].as_array().map_or(0, Vec::len),
        3,
        "the listing must report the catalogs the rows registered, got {page}"
    );
}
