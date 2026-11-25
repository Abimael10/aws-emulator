// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the account's IMPLICIT data catalog — the AWS Glue Data Catalog
//! Athena addresses by its reserved name — driven over the wire (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`) through the same router the emulator boots.
//!
//! THE RULE UNDER TEST: the implicit catalog is not a REGISTERED resource. Only the catalog
//! listing reports it; a registry read by name resolves nothing for it, so `GetDataCatalog`
//! refuses it with the read path's catalog-miss sentence exactly as it refuses a name no
//! create ever registered; and no delete removes it, so `DeleteDataCatalog` refuses it as a
//! PROTECTED resource — a different sentence from the miss, because a client reconciling
//! state has to tell "already gone" from "exists and cannot be removed". A catalog a
//! `CreateDataCatalog` call DID register is the control: it reads back and a delete removes
//! it.
//!
//! The rows differ along what the rule generalises over: the spelling of the reserved name a
//! request carries (the canonical one and the other cases Athena addresses that same catalog
//! by), the `DataCatalogType` and the name of a registered catalog standing beside it, and
//! whether the request arrives before or after a registered catalog was deleted. The two
//! refusal sentences are asserted as the literal text AWS answers, not as this service's own
//! constants, so a change of wording fails here.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DATA_CATALOG_TYPE_GLUE, DATA_CATALOG_TYPE_HIVE, DATA_CATALOG_TYPE_LAMBDA,
    ERROR_CODE_DATA_CATALOG_NOT_FOUND, ERROR_CODE_INVALID_INPUT, GLUE_DATA_CATALOG_NAME,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, router};

/// How AWS refuses a registry read of the reserved catalog: the read path's catalog-miss
/// sentence, whose fixed text is `DataCatalog ` and ` was not found.` around the `Name` the
/// request carried. Written out here rather than taken from this service's own constant, so
/// the assertion fails for any other wording.
fn expected_read_refusal(name: &str) -> String {
    format!("DataCatalog {name} was not found.")
}

/// How AWS refuses a delete of the reserved catalog: entirely fixed text naming the catalog
/// that cannot be deleted. Written out here for the same reason as [`expected_read_refusal`].
const EXPECTED_DELETE_REFUSAL: &str = "Cannot delete AwsDataCatalog.";

/// The spellings of the reserved name a request can carry: the canonical one Athena publishes
/// and the lowercased and uppercased forms its query layer addresses the same catalog by.
fn reserved_spellings() -> Vec<(&'static str, String)> {
    vec![
        ("the canonical name", GLUE_DATA_CATALOG_NAME.to_string()),
        ("the lowercased name", GLUE_DATA_CATALOG_NAME.to_lowercase()),
        ("the uppercased name", GLUE_DATA_CATALOG_NAME.to_uppercase()),
    ]
}

/// The catalogs the control rows register: the case, the name, and the create request. One
/// per `DataCatalogType` whose parameters a create can carry without another service holding
/// anything.
fn registered_catalogs() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "a GLUE catalog of another account",
            "reserved_control_glue",
            json!({
                "Name": "reserved_control_glue",
                "Type": DATA_CATALOG_TYPE_GLUE,
                "Parameters": { "catalog-id": "111122223333" },
            }),
        ),
        (
            "an external Hive metastore",
            "reserved-control-hive",
            json!({
                "Name": "reserved-control-hive",
                "Type": DATA_CATALOG_TYPE_HIVE,
                "Parameters": {
                    "metadata-function": "arn:aws:lambda:us-east-1:111122223333:function:hms",
                },
            }),
        ),
        (
            "a Lambda-backed catalog",
            "reserved_control_lambda",
            json!({
                "Name": "reserved_control_lambda",
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Parameters": {
                    "function": "arn:aws:lambda:us-east-1:111122223333:function:both",
                },
            }),
        ),
    ]
}

/// The catalog names one `ListDataCatalogs` page reports.
async fn listed_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = call(router, "ListDataCatalogs", &json!({})).await;
    assert_eq!(
        status, 200,
        "ListDataCatalogs must answer 200, got {status} with {body:?}"
    );
    body["DataCatalogsSummary"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|summary| {
            summary["CatalogName"]
                .as_str()
                .unwrap_or("<no CatalogName>")
                .to_string()
        })
        .collect()
}

/// The listing reports the implicit catalog, and the registry read of it resolves nothing —
/// whichever spelling the request carries, and whatever else the account holds.
#[tokio::test]
async fn the_implicit_catalog_is_listed_but_not_resolvable_by_name() {
    let router = router();

    // Before anything is registered, and again with a catalog of each type registered
    // beside it: what the account holds changes nothing about either half of the rule.
    for (stage, registered) in [("an empty registry", false), ("a filled registry", true)] {
        if registered {
            for (case, _, request) in registered_catalogs() {
                let (status, created) = call(&router, "CreateDataCatalog", &request).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the catalog must be created, got {status} with {created:?}"
                );
            }
        }

        let listed = listed_names(&router).await;
        assert!(
            listed.contains(&GLUE_DATA_CATALOG_NAME.to_string()),
            "case {stage:?}: the listing must report the implicit Glue Data Catalog, got \
             {listed:?}"
        );

        for (spelling, name) in reserved_spellings() {
            let (status, refused) =
                call(&router, "GetDataCatalog", &json!({ "Name": name })).await;
            assert_refusal(
                &format!("GetDataCatalog on {spelling} with {stage}"),
                status,
                &refused,
                ERROR_CODE_INVALID_INPUT,
                &expected_read_refusal(&name),
            );
        }
    }

    // The control: a catalog a create DID register resolves by name.
    for (case, name, _) in registered_catalogs() {
        let (status, read) = call(&router, "GetDataCatalog", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a registered catalog must read back, got {status} with {read:?}"
        );
        assert_eq!(
            read["DataCatalog"]["Name"].as_str(),
            Some(name),
            "case {case:?}: the read must report the catalog asked for, got {read:?}"
        );
    }
}

/// A delete that addresses the implicit catalog is refused as a protected resource, leaves it
/// listed, and reads differently from the miss a delete of an absent catalog answers.
#[tokio::test]
async fn the_implicit_catalog_is_refused_as_undeletable() {
    let router = router();

    for (spelling, name) in reserved_spellings() {
        let (status, refused) =
            call(&router, "DeleteDataCatalog", &json!({ "Name": name })).await;
        assert_refusal(
            &format!("DeleteDataCatalog on {spelling}"),
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            EXPECTED_DELETE_REFUSAL,
        );
        // The refusal of a protected catalog is not the refusal of an absent one: a client
        // that treats "was not found" as a reached desired state must not see it here.
        assert_ne!(
            EXPECTED_DELETE_REFUSAL,
            expected_read_refusal(&name),
            "case {spelling:?}: the protected-resource refusal must not read as the miss"
        );
        // `DeleteCatalogOnly` changes nothing: the catalog is protected whichever form of
        // the delete addresses it.
        let (status, refused) = call(
            &router,
            "DeleteDataCatalog",
            &json!({ "Name": name, "DeleteCatalogOnly": true }),
        )
        .await;
        assert_refusal(
            &format!("DeleteDataCatalog with DeleteCatalogOnly on {spelling}"),
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            EXPECTED_DELETE_REFUSAL,
        );
    }

    // Nothing was removed: the refused deletes left the catalog where the listing reports it.
    let listed = listed_names(&router).await;
    assert!(
        listed.contains(&GLUE_DATA_CATALOG_NAME.to_string()),
        "the refused deletes must leave the implicit catalog listed, got {listed:?}"
    );

    // The control: a registered catalog is genuinely removed, and a second delete of that
    // same name answers the delete path's MISS — the sentence the protected catalog never
    // answers.
    for (case, name, request) in registered_catalogs() {
        let (status, created) = call(&router, "CreateDataCatalog", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the catalog must be created, got {status} with {created:?}"
        );
        let (status, removed) =
            call(&router, "DeleteDataCatalog", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a registered catalog must be deletable, got {status} with \
             {removed:?}"
        );
        let listed = listed_names(&router).await;
        assert!(
            !listed.contains(&name.to_string()),
            "case {case:?}: the deleted catalog must be gone from the listing, got {listed:?}"
        );
        let (status, again) = call(&router, "DeleteDataCatalog", &json!({ "Name": name })).await;
        assert_refusal(
            &format!("a second DeleteDataCatalog of {case}"),
            status,
            &again,
            ERROR_CODE_DATA_CATALOG_NOT_FOUND,
            &format!("DataCatalog {name} was not found"),
        );
    }
}
