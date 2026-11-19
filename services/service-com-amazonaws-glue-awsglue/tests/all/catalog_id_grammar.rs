// SPDX-License-Identifier: CC0-1.0

//! What a `CatalogId` that addresses ONE catalog resource is, on both operations that take
//! one (`GetCatalog`, `DeleteCatalog`): a path of catalog NAMES, optionally under the
//! account whose Data Catalog holds it.
//!
//! Two rules are pinned here, each over a table of inputs:
//!
//! * a path SEGMENT outside the grammar a catalog name has to satisfy is a MALFORMED
//!   address — HTTP 400 `ValidationException` carrying AWS's constraint report, naming the
//!   catalog's own `name` and echoing `CatalogNameString`'s expression as the model
//!   declares it;
//! * the NESTING separator is part of the address and not of a name, so a nested id
//!   resolves and a nested catalog nothing created is the catalog miss — HTTP 400
//!   `EntityNotFoundException` with the miss sentence.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is the HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{
    call, constraint_message, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION,
};

/// `CatalogNameString`'s own expression, as the model declares it — anchors included
/// (`smithy_query(query="operation",operation="CreateCatalog")` reports `Name` as
/// `CatalogNameString` with `@pattern ^(?!(.*[.\/\\]|aws:)).*$`). Written here
/// independently of the handler, so this suite pins the text a refusal echoes.
const CATALOG_NAME_PATTERN: &str = r"^(?!(.*[.\/\\]|aws:)).*$";

/// The catalog miss sentence, written here independently of the handler.
const CATALOG_NOT_FOUND: &str = "Catalog not found.";

/// The refusal a `CatalogId` whose catalog name is `name` earns: AWS's constraint report
/// over one violation, at the catalog's own `name` and quoting the name that failed.
fn expected_refusal(name: &str) -> String {
    format!(
        "1 validation error detected: Value '{name}' at 'name' failed to satisfy constraint: \
         Member must satisfy regular expression pattern: {CATALOG_NAME_PATTERN}"
    )
}

/// Register one catalog of this account, the only way `CreateCatalog` registers one: a
/// federation pointing at a source one of AWS's own managed integrations registered.
async fn given_catalog(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateCatalog",
        &json!({
            "Name": name,
            "CatalogInput": {
                "FederatedCatalog": {
                    "Identifier": format!("arn:aws:s3tables:{REGION}:{ACCOUNT_ID}:bucket/{name}"),
                    "ConnectionName": "aws:s3tables",
                },
            },
        }),
    )
    .await;
}

/// The status and body of a `GetCatalog` for `catalog_id`.
async fn get(router: &MultiServiceRouter, catalog_id: &str) -> (u16, Value) {
    call(router, "GetCatalog", &json!({ "CatalogId": catalog_id })).await
}

/// The status and body of a `DeleteCatalog` for `catalog_id`.
async fn delete(router: &MultiServiceRouter, catalog_id: &str) -> (u16, Value) {
    call(router, "DeleteCatalog", &json!({ "CatalogId": catalog_id })).await
}

/// A `CatalogId` carrying a path segment that is no catalog NAME is refused as a malformed
/// request — the constraint report naming the catalog's own `name` and echoing
/// `CatalogNameString`'s expression as the model declares it — on the read and on the
/// delete alike, and the catalogs this account holds are left alone.
///
/// The rows differ along which character takes a segment outside the grammar, where the
/// segment carries it, which segment of a nested path it is, and whether the id names this
/// account explicitly.
#[tokio::test]
async fn a_catalog_id_whose_name_is_outside_the_grammar_is_a_malformed_request() {
    let router = emulator();
    given_catalog(&router, "stored-catalog").await;

    for (case, catalog_id, refused_name) in [
        ("a dot inside the name", "my.catalog".to_string(), "my.catalog"),
        ("a dot at the end", "catalog.".to_string(), "catalog."),
        ("a dot at the start", ".catalog".to_string(), ".catalog"),
        ("several dots", "lake.catalog.2026".to_string(), "lake.catalog.2026"),
        ("a backslash", "my\\catalog".to_string(), "my\\catalog"),
        ("the reserved prefix", "aws:catalog".to_string(), "aws:catalog"),
        ("the reserved prefix alone", "aws:".to_string(), "aws:"),
        (
            "a nested path whose child is outside the grammar",
            "s3tablescatalog/bad.child".to_string(),
            "bad.child",
        ),
        (
            "a nested path whose holder is outside the grammar",
            "bad.holder/analytics".to_string(),
            "bad.holder",
        ),
        (
            "this account and a name outside the grammar",
            format!("{ACCOUNT_ID}:bad.child"),
            "bad.child",
        ),
        (
            "this account spelled as the holder of a bad name",
            format!("{ACCOUNT_ID}/bad.child"),
            "bad.child",
        ),
    ] {
        for (surface, (status, body)) in [
            ("GetCatalog", get(&router, &catalog_id).await),
            ("DeleteCatalog", delete(&router, &catalog_id).await),
        ] {
            assert_eq!(
                status, 400,
                "case {case} on {surface}: expected 400 for {catalog_id:?}, got {status} with \
                 {body}"
            );
            assert!(
                error_code(&body).ends_with("ValidationException"),
                "case {case} on {surface}: expected ValidationException for {catalog_id:?}, got \
                 {} in {body}",
                error_code(&body)
            );
            assert_eq!(
                constraint_message(&body),
                expected_refusal(refused_name),
                "case {case} on {surface}: unexpected wording for {catalog_id:?}, got {}",
                constraint_message(&body)
            );
        }
    }

    // The refused calls read and removed nothing: the catalog this account holds still
    // reads back, and so does the account's own Data Catalog.
    for (case, catalog_id) in [
        ("the stored catalog", "stored-catalog"),
        ("the account's own Data Catalog", ACCOUNT_ID),
    ] {
        let (status, body) = get(&router, catalog_id).await;
        assert_eq!(
            status, 200,
            "case {case}: the refused calls must have changed nothing, got {status} with {body}"
        );
    }
}

/// The NESTING separator is part of the ADDRESS and not of a name, so an id carrying it
/// resolves: a nested catalog this account does not hold is the catalog miss — the modelled
/// `EntityNotFoundException` with its fixed sentence — on the read and on the delete alike,
/// never a malformed request.
///
/// The rows differ along how deep the path nests, whether the id names this account and
/// which separator it names it with, and along the near misses the name grammar admits.
#[tokio::test]
async fn a_nested_catalog_id_resolves_and_reports_the_catalog_miss() {
    let router = emulator();

    for (case, catalog_id) in [
        (
            "this account as the holder of a nested name nothing created",
            format!("{ACCOUNT_ID}/no-such-nested-catalog"),
        ),
        (
            "the nested form the Data Catalog publishes",
            "s3tablescatalog/analytics".to_string(),
        ),
        (
            "this account and that nested form",
            format!("{ACCOUNT_ID}:s3tablescatalog/analytics"),
        ),
        ("three levels of nesting", "holder/middle/leaf".to_string()),
        ("a bare name nothing created", "never-created".to_string()),
        (
            "the reserved letters without the colon",
            "awscatalog".to_string(),
        ),
        (
            "a colon that is not the account prefix",
            "my-aws:catalog".to_string(),
        ),
    ] {
        for (surface, (status, body)) in [
            ("GetCatalog", get(&router, &catalog_id).await),
            ("DeleteCatalog", delete(&router, &catalog_id).await),
        ] {
            assert_eq!(
                status, 400,
                "case {case} on {surface}: expected 400 for {catalog_id:?}, got {status} with \
                 {body}"
            );
            assert!(
                error_code(&body).ends_with("EntityNotFoundException"),
                "case {case} on {surface}: expected EntityNotFoundException for {catalog_id:?}, \
                 got {} in {body}",
                error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                CATALOG_NOT_FOUND,
                "case {case} on {surface}: expected the catalog miss wording for {catalog_id:?}, \
                 got {}",
                error_message(&body)
            );
        }
    }
}

/// The account's own Data Catalog keeps its own two answers, which the grammar check runs
/// before and must not take: its id reads back the catalog every account has, and the
/// delete of it stays the modelled `InvalidInputException` stating the rule a deletable
/// catalog's name satisfies — with the catalog still readable afterwards.
#[tokio::test]
async fn the_accounts_own_data_catalog_keeps_its_answers() {
    let router = emulator();

    let (status, body) = get(&router, ACCOUNT_ID).await;
    assert_eq!(
        status, 200,
        "case the account's own id: GetCatalog must answer 200, got {status} with {body}"
    );
    assert_eq!(
        body["Catalog"]["Name"].as_str(),
        Some(ACCOUNT_ID),
        "case the account's own id: the read must report the account's own Data Catalog, got \
         {body}"
    );

    let (status, body) = delete(&router, ACCOUNT_ID).await;
    assert_eq!(
        status, 400,
        "case deleting the account's own id: expected 400, got {status} with {body}"
    );
    assert!(
        error_code(&body).ends_with("InvalidInputException"),
        "case deleting the account's own id: expected InvalidInputException, got {} in {body}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        "Only lowercase letters, numbers, underscores and hyphens are allowed for catalog name",
        "case deleting the account's own id: unexpected wording, got {}",
        error_message(&body)
    );

    let (status, body) = get(&router, ACCOUNT_ID).await;
    assert_eq!(
        status, 200,
        "case reading after the refused delete: the catalog must still be there, got {status} \
         with {body}"
    );
}

/// A name the grammar admits still resolves end to end: the catalog a create registered
/// under it reads back, and a delete of it removes it — so the refusals above turn on the
/// grammar and are not a blanket over every id.
#[tokio::test]
async fn a_name_the_grammar_admits_still_resolves() {
    let router = emulator();

    for (case, name) in [
        ("letters and hyphens", "admitted-catalog"),
        ("underscores and digits", "lake_catalog_2026"),
        ("a single character", "c"),
    ] {
        given_catalog(&router, name).await;

        let (status, body) = get(&router, name).await;
        assert_eq!(
            status, 200,
            "case {case}: GetCatalog must answer 200 for {name:?}, got {status} with {body}"
        );
        assert_eq!(
            body["Catalog"]["Name"].as_str(),
            Some(name),
            "case {case}: the read must report the stored catalog, got {body}"
        );

        let (status, body) = delete(&router, name).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteCatalog must answer 200 for {name:?}, got {status} with {body}"
        );
        let (status, body) = get(&router, name).await;
        assert_eq!(
            status, 400,
            "case {case}: the deleted catalog must be gone, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {case}: the deleted catalog must report the miss, got {} in {body}",
            error_code(&body)
        );
    }
}
