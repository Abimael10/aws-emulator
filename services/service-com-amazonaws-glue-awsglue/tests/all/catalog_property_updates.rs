// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateCatalog`, and the `GetCatalog`/`GetCatalogs` reads that report
//! what it wrote.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! RULE under test: the request's `CatalogInput` STATES the addressed catalog's whole
//! client-settable definition — "a `CatalogInput` object specifying the new properties of an
//! existing catalog" — so every member it carries is written and every member it omits is
//! cleared, while the catalog's name, its id, its creation instant and its tags are not the
//! request's to change and its update instant moves. What the write is refused by is read off
//! the catalog the id addresses: a catalog whose databases live in a federated source, the
//! account's own Data Catalog and a catalog in another account's are each the modelled client
//! refusal for that case, and an id this account holds no catalog under is the same miss a
//! read of it answers.
//!
//! The recorded calls — an update of a federated catalog and one of an unknown catalog id —
//! are one row each of the refusal tables below; the rows differ along which members the
//! input states, the form of the catalog id addressed, and which kind of catalog it names.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::messages;
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION};

/// A `FederatedCatalog` naming a source outside the Glue Data Catalog, as a lake-formation
/// client registers one: the connection is one of the federation connections AWS's own
/// managed integrations use, which is what makes the source one a create may federate to.
fn federation_source(name: &str) -> Value {
    json!({
        "Identifier": format!("arn:aws:s3tables:{REGION}:{ACCOUNT_ID}:bucket/{name}"),
        "ConnectionName": "aws:s3tables",
    })
}

/// A `TargetRedshiftCatalog` resource link, the kind of catalog whose own definition this
/// Data Catalog holds — and so the kind an update of its properties writes.
///
/// One copy, shared with [`crate::catalog_definition_write_scope`], so the two suites that
/// drive a catalog update cannot drift apart on what such a catalog looks like.
pub fn resource_link(name: &str) -> Value {
    json!({ "CatalogArn": format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:catalog/{name}") })
}

/// Register one catalog that is a resource link, carrying `tags`, so an update has a catalog
/// to write and a tag set beside the definition it writes.
///
/// One copy, shared with [`crate::catalog_definition_write_scope`]: the request a catalog
/// create is driven with is maintained here.
pub async fn given_linked_catalog_with_tags(
    router: &MultiServiceRouter,
    name: &str,
    tags: &Value,
) {
    ok(
        router,
        "CreateCatalog",
        &json!({
            "Name": name,
            "CatalogInput": {
                "Description": "the definition the create stored",
                "Parameters": { "owner": "analytics" },
                "TargetRedshiftCatalog": resource_link(name),
            },
            "Tags": tags,
        }),
    )
    .await;
}

/// The same, with the tag set the rows of this suite stage their catalogs with.
async fn given_linked_catalog(router: &MultiServiceRouter, name: &str) {
    given_linked_catalog_with_tags(router, name, &json!({ "owner": "data-platform" })).await;
}

/// Assert the members no `CatalogInput` carries are the ones the create left: a catalog's name
/// and its two id members address it, its kind is read off its own definition, its ARN is
/// minted from both, and `CreateTime` is "the time at which the catalog was created".
///
/// One copy, shared with [`crate::catalog_definition_write_scope`].
pub fn assert_server_owned_members_unchanged(case: &str, created: &Value, read: &Value) {
    for member in [
        "Name",
        "CatalogId",
        "CatalogIdentifier",
        "CatalogType",
        "ResourceArn",
        "CreateTime",
    ] {
        assert_eq!(
            read.get(member),
            created.get(member),
            "case {case}: {member} is not this request's to change, got {read}"
        );
    }
}

/// The `Catalog` a `GetCatalog` for `catalog_id` reports.
///
/// One copy, shared with [`crate::catalog_definition_write_scope`].
pub async fn get_catalog(router: &MultiServiceRouter, catalog_id: &str) -> Value {
    let read = ok(router, "GetCatalog", &json!({ "CatalogId": catalog_id })).await;
    read.get("Catalog")
        .cloned()
        .unwrap_or_else(|| panic!("GetCatalog must report a Catalog, got {read}"))
}

/// The same catalog as the paging read reports it, which is how a row checks the definition
/// is held in the STORE rather than invented by one handler.
async fn get_catalog_through_the_listing(router: &MultiServiceRouter, name: &str) -> Value {
    let body = ok(router, "GetCatalogs", &json!({})).await;
    body.get("CatalogList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("GetCatalogs must report a CatalogList, got {body}"))
        .iter()
        .find(|catalog| catalog.get("Name").and_then(Value::as_str) == Some(name))
        .cloned()
        .unwrap_or_else(|| panic!("GetCatalogs must report {name}, got {body}"))
}

/// Update `catalog_id` with `input` and assert the call is served with the modelled empty
/// answer.
///
/// One copy, shared with [`crate::catalog_definition_write_scope`].
pub async fn update(router: &MultiServiceRouter, catalog_id: &str, input: &Value) {
    let answer = ok(
        router,
        "UpdateCatalog",
        &json!({ "CatalogId": catalog_id, "CatalogInput": input }),
    )
    .await;
    assert_eq!(
        answer,
        json!({}),
        "UpdateCatalog answers an empty structure, got {answer}"
    );
}

// ---------------------------------------------------------------------------
// The write
// ---------------------------------------------------------------------------

/// What an update states is what a read reports, member for member — and what it omits is
/// gone, because the input is the whole definition and not a patch over the stored one.
///
/// The rows differ along which members of the `CatalogInput` the update states: the minimum
/// that keeps the catalog the kind it is, a description and parameters, the property map, the
/// two Lake Formation default-permission lists, and a parameter map replacing a bigger one.
#[tokio::test]
async fn an_update_states_the_catalogs_whole_definition() {
    let router = emulator();
    let name = "linked-catalog";
    given_linked_catalog(&router, name).await;
    let created = get_catalog(&router, name).await;

    let cases: Vec<(&str, Value)> = vec![
        (
            "a description and parameters",
            json!({
                "Description": "updated through UpdateCatalog",
                "Parameters": { "owner": "platform", "tier": "gold" },
                "TargetRedshiftCatalog": resource_link(name),
            }),
        ),
        (
            "the resource link alone, which clears everything else",
            json!({ "TargetRedshiftCatalog": resource_link(name) }),
        ),
        (
            "a smaller parameter map replacing a bigger one",
            json!({
                "Parameters": { "owner": "platform" },
                "TargetRedshiftCatalog": resource_link(name),
            }),
        ),
        (
            "the catalog's properties",
            json!({
                "Description": "with properties",
                "TargetRedshiftCatalog": resource_link(name),
                "CatalogProperties": {
                    "DataLakeAccessProperties": { "DataLakeAccess": false },
                },
            }),
        ),
        (
            "default permissions that grant something",
            json!({
                "TargetRedshiftCatalog": resource_link(name),
                "CreateDatabaseDefaultPermissions": [{
                    "Principal": { "DataLakePrincipalIdentifier": "IAM_ALLOWED_PRINCIPALS" },
                    "Permissions": ["ALL"],
                }],
                "CreateTableDefaultPermissions": [{
                    "Principal": { "DataLakePrincipalIdentifier": "IAM_ALLOWED_PRINCIPALS" },
                    "Permissions": ["SELECT"],
                }],
            }),
        ),
        (
            "allowing full table external data access",
            json!({
                "TargetRedshiftCatalog": resource_link(name),
                "AllowFullTableExternalDataAccess": "True",
            }),
        ),
    ];

    for (case, input) in &cases {
        update(&router, name, input).await;
        let read = get_catalog(&router, name).await;

        // The members the input states come back as sent; the members it omits are reported
        // the way a catalog that never carried them is reported.
        for member in [
            "Description",
            "Parameters",
            "TargetRedshiftCatalog",
            "FederatedCatalog",
            "AllowFullTableExternalDataAccess",
        ] {
            assert_eq!(
                read.get(member).cloned().unwrap_or(Value::Null),
                input.get(member).cloned().unwrap_or(Value::Null),
                "case {case}: expected {member} to be what the update stated, got {read}"
            );
        }
        for member in [
            "CreateDatabaseDefaultPermissions",
            "CreateTableDefaultPermissions",
        ] {
            let stated = input.get(member).cloned().unwrap_or_else(|| json!([]));
            let reported = read.get(member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                reported.as_array().map(Vec::len),
                stated.as_array().map(Vec::len),
                "case {case}: expected {member} to hold what the update stated, got {read}"
            );
        }

        // The members no `CatalogInput` carries are not the update's to change.
        assert_server_owned_members_unchanged(case, &created, &read);
        // "The time at which the catalog was last updated" moves with the write.
        let created_at = created
            .get("CreateTime")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case}: the create stamped a CreateTime"));
        let updated_at = read
            .get("UpdateTime")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case}: expected an UpdateTime, got {read}"));
        assert!(
            updated_at >= created_at,
            "case {case}: expected an UpdateTime at or after {created_at}, got {updated_at}"
        );

        // The definition is the store's, not one reader's.
        let listed = get_catalog_through_the_listing(&router, name).await;
        assert_eq!(
            listed, read,
            "case {case}: the paging read must report the same catalog, expected {read}, got \
             {listed}"
        );
    }

    // The catalog's tags are written through the tagging operations and the `Catalog` shape
    // carries no tag member, so a definition write has nothing to report about them: that the
    // write leaves the tag set as the create recorded it is read back through `GetTags` by
    // [`crate::catalog_definition_write_scope`], which pins what a definition write touches at
    // all, where what this suite pins is WHICH definition the write leaves and the refusals it
    // earns.
}

/// An update reaches the catalog its id addresses under any casing of that catalog's name —
/// the id resolution a read of one catalog uses — and leaves every other catalog alone.
#[tokio::test]
async fn an_update_reaches_one_catalog_under_any_casing_of_its_name() {
    let router = emulator();
    given_linked_catalog(&router, "mixed-case").await;
    given_linked_catalog(&router, "bystander").await;
    let bystander = get_catalog(&router, "bystander").await;

    for (case, catalog_id) in [
        ("the name as created", "mixed-case".to_string()),
        ("upper case", "MIXED-CASE".to_string()),
        (
            "qualified by the account",
            format!("{ACCOUNT_ID}:mixed-case"),
        ),
    ] {
        update(
            &router,
            &catalog_id,
            &json!({
                "Description": format!("stated through {case}"),
                "TargetRedshiftCatalog": resource_link("mixed-case"),
            }),
        )
        .await;
        let read = get_catalog(&router, "mixed-case").await;
        assert_eq!(
            read.get("Description").and_then(Value::as_str),
            Some(format!("stated through {case}").as_str()),
            "case {case}: the update must reach the catalog that id addresses, got {read}"
        );
        assert_eq!(
            get_catalog(&router, "bystander").await,
            bystander,
            "case {case}: no other catalog is touched"
        );
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// A catalog whose databases live in a federated source carries a pointer at that source and
/// nothing a `CatalogInput` could state, so the update is the modelled refusal and the stored
/// definition is untouched.
///
/// The rows span the forms a federation takes: the s3tables bucket a lake-formation client
/// registers, a Redshift namespace, and a federated catalog that also carries properties of
/// its own.
#[tokio::test]
async fn an_update_of_a_federated_catalog_is_the_modelled_refusal() {
    let router = emulator();

    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "an s3tables-federated catalog",
            "federated-s3tables",
            json!({ "FederatedCatalog": federation_source("federated-s3tables") }),
        ),
        (
            "a federated catalog naming its connection type",
            "federated-redshift",
            json!({
                "FederatedCatalog": {
                    "Identifier": format!(
                        "arn:aws:redshift:{REGION}:{ACCOUNT_ID}:namespace/orders"
                    ),
                    "ConnectionName": "aws:redshift",
                    "ConnectionType": "REDSHIFT",
                },
            }),
        ),
        (
            "a federated catalog carrying parameters",
            "federated-parameterised",
            json!({
                "Description": "federated and parameterised",
                "Parameters": { "owner": "lakeformation" },
                "FederatedCatalog": federation_source("federated-parameterised"),
            }),
        ),
    ];

    for (case, name, input) in &cases {
        ok(
            &router,
            "CreateCatalog",
            &json!({ "Name": name, "CatalogInput": input }),
        )
        .await;
        let before = get_catalog(&router, name).await;

        // Repeated refusals, and one that states a different definition: neither is served.
        for (attempt, stated) in [
            ("the stored definition", (*input).clone()),
            (
                "a definition that drops the federation",
                json!({ "Description": "take the federation off" }),
            ),
        ] {
            let (status, body) = call(
                &router,
                "UpdateCatalog",
                &json!({ "CatalogId": name, "CatalogInput": stated }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case} ({attempt}): expected 400, got {status} with {body}"
            );
            assert!(
                error_code(&body).contains("InvalidInputException"),
                "case {case} ({attempt}): expected InvalidInputException, got {body}"
            );
            assert_eq!(
                error_message(&body),
                messages::FEDERATED_CATALOG_NOT_UPDATABLE,
                "case {case} ({attempt}): expected the federated-catalog refusal, got {body}"
            );

            let after = get_catalog(&router, name).await;
            assert_eq!(
                after, before,
                "case {case} ({attempt}): a refused update must change nothing, expected \
                 {before}, got {after}"
            );
        }
    }
}

/// An id this account holds no catalog under is the modelled miss, and the update creates
/// nothing under it: the rows span a name never created, one differing only in case from a
/// catalog that exists (which DOES resolve, so it is excluded), an id of another shape, and a
/// well-formed but absent qualified id.
#[tokio::test]
async fn an_update_of_an_unknown_catalog_id_is_the_modelled_miss() {
    let router = emulator();
    given_linked_catalog(&router, "present-catalog").await;
    let present = get_catalog(&router, "present-catalog").await;

    for (case, catalog_id) in [
        ("a name never created", "absent-catalog".to_string()),
        (
            "a qualified id never created",
            format!("{ACCOUNT_ID}:absent-catalog"),
        ),
        ("an id that is no catalog name at all", "Absent/Catalog".to_string()),
        ("a name one character longer", "present-catalogs".to_string()),
    ] {
        let (status, body) = call(
            &router,
            "UpdateCatalog",
            &json!({
                "CatalogId": &catalog_id,
                "CatalogInput": { "Description": "nothing holds this" },
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            messages::CATALOG_NOT_FOUND,
            "case {case}: expected the catalog miss, got {body}"
        );

        // The refused update created nothing under that id, and left the catalog it nearly
        // addressed as it was.
        let (status, body) = call(
            &router,
            "GetCatalog",
            &json!({ "CatalogId": &catalog_id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a refused update must not create a catalog, got {status} with {body}"
        );
        assert_eq!(
            get_catalog(&router, "present-catalog").await,
            present,
            "case {case}: the stored catalog must be untouched"
        );
    }
}

/// The account's own Data Catalog is the container the catalogs a create registers live IN,
/// not one of them, so no `CatalogInput` describes it and the update is refused — and a
/// catalog id naming ANOTHER account's Data Catalog is refused as unauthorized, before any
/// entity is resolved, exactly as a read and a delete of it are.
#[tokio::test]
async fn an_update_of_a_catalog_no_create_registered_is_refused() {
    let router = emulator();

    for (case, catalog_id, expected_code, expected_message) in [
        (
            "the account's own Data Catalog",
            ACCOUNT_ID.to_string(),
            "InvalidInputException",
            messages::ROOT_CATALOG_NOT_UPDATABLE.to_string(),
        ),
        (
            "another account's Data Catalog",
            "123456789012".to_string(),
            "AccessDeniedException",
            messages::not_authorized_on_catalog(
                "UpdateCatalog",
                &format!("arn:aws:glue:{REGION}:123456789012:catalog/123456789012"),
            ),
        ),
        (
            "a catalog inside another account's Data Catalog",
            "123456789012:theirs".to_string(),
            "AccessDeniedException",
            messages::not_authorized_on_catalog(
                "UpdateCatalog",
                &format!("arn:aws:glue:{REGION}:123456789012:catalog/theirs"),
            ),
        ),
    ] {
        let (status, body) = call(
            &router,
            "UpdateCatalog",
            &json!({
                "CatalogId": &catalog_id,
                "CatalogInput": { "Description": "not this request's to state" },
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains(expected_code),
            "case {case}: expected {expected_code}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message,
            "case {case}: expected the refusal for {catalog_id}, got {body}"
        );
    }
}
