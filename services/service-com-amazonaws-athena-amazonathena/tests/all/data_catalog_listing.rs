// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ListDataCatalogs`: the data catalogs this account holds, a page at
//! a time, driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots.
//!
//! THE RULE UNDER TEST: a listing reports what the account HOLDS — every catalog a create
//! registered, plus the account's own Glue Data Catalog, which no create registered and
//! whose metadata this emulator serves — each as the summary of the record it was registered
//! from, in one stable order, a page at a time, with a continuation token only where the
//! listing has more to report. A token this store did not mint for THIS listing names no
//! page.
//!
//! The bounds are the model's, read with
//! `smithy_query(query="operation",operation="ListDataCatalogs")` and
//! `smithy_query(query="shape",shape="DataCatalogSummary,DataCatalogType")`: `MaxResults` is
//! `MaxDataCatalogsCount` (range 2..50), and a summary carries the catalog's name, its type,
//! the status of its creation or deletion, a federated catalog's connection type and the
//! text of an error, of which a catalog that was created has none.
//!
//! Each test asserts the rule over a TABLE rather than one request: the rows differ along
//! the catalog type, the page size and how many catalogs the account holds.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    page_token_malformed, DATA_CATALOG_STATUS_CREATE_COMPLETE, DATA_CATALOG_TYPE_FEDERATED,
    DATA_CATALOG_TYPE_GLUE, DATA_CATALOG_TYPE_HIVE, DATA_CATALOG_TYPE_LAMBDA,
    ERROR_CODE_INVALID_INPUT, GLUE_DATA_CATALOG_NAME,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, error_code, message, router};

/// The error shape every refusal of this operation carries: the client error it models
/// (HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The catalogs the rows register: the name, and the create request that registers it. One
/// per `DataCatalogType` a client can register, each with the parameters its own type
/// requires.
fn catalogs() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "a GLUE catalog in another account",
            "glue_other_account",
            json!({
                "Name": "glue_other_account",
                "Type": DATA_CATALOG_TYPE_GLUE,
                "Parameters": { "catalog-id": "111122223333" },
            }),
        ),
        (
            "an external Hive metastore",
            "hive-metastore",
            json!({
                "Name": "hive-metastore",
                "Type": DATA_CATALOG_TYPE_HIVE,
                "Parameters": {
                    "metadata-function":
                        "arn:aws:lambda:us-east-1:111122223333:function:hms",
                },
            }),
        ),
        (
            "a Lambda-backed catalog",
            "lambda_catalog",
            json!({
                "Name": "lambda_catalog",
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Parameters": {
                    "function": "arn:aws:lambda:us-east-1:111122223333:function:both",
                },
            }),
        ),
        (
            "a federated catalog describing a new connection",
            "federated_new",
            json!({
                "Name": "federated_new",
                "Type": DATA_CATALOG_TYPE_FEDERATED,
                "Parameters": {
                    "connection-type": "MYSQL",
                    "connection-properties": "{\"spill_bucket\":\"my_spill\"}",
                },
            }),
        ),
    ]
}

/// One page of the listing: `(status, DataCatalogsSummary, NextToken)`.
async fn list(router: &MultiServiceRouter, request: &Value) -> (u16, Vec<Value>, Option<String>) {
    let (status, body) = call(router, "ListDataCatalogs", request).await;
    let page = body["DataCatalogsSummary"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let token = body["NextToken"].as_str().map(str::to_string);
    (status, page, token)
}

/// The catalog names a page reports, in the order it reported them.
fn names(page: &[Value]) -> Vec<String> {
    page.iter()
        .map(|summary| {
            summary["CatalogName"]
                .as_str()
                .unwrap_or("<no CatalogName>")
                .to_string()
        })
        .collect()
}

/// Register every catalog of [`catalogs`].
async fn given_catalogs(router: &MultiServiceRouter) {
    for (case, _, request) in catalogs() {
        let (status, created) = call(router, "CreateDataCatalog", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the catalog must be created, got {status} with {created:?}"
        );
    }
}

/// An account that registered no catalog of its own still holds one: its Glue Data Catalog,
/// reported with the type and the completed status that catalog has, and readable through
/// `GetDataCatalog`.
#[tokio::test]
async fn an_account_that_registered_none_reports_its_glue_data_catalog() {
    let router = router();
    let (status, page, token) = list(&router, &json!({})).await;
    assert_eq!(status, 200, "the listing must answer 200, got {status}");
    assert_eq!(
        names(&page),
        vec![GLUE_DATA_CATALOG_NAME],
        "an account with no registered catalog must report its Glue Data Catalog alone, \
         got {page:?}"
    );
    let glue = &page[0];
    assert_eq!(
        glue["Type"].as_str(),
        Some(DATA_CATALOG_TYPE_GLUE),
        "the Glue Data Catalog must report its own type, got {glue:?}"
    );
    assert_eq!(
        glue["Status"].as_str(),
        Some(DATA_CATALOG_STATUS_CREATE_COMPLETE),
        "a synchronously created catalog has no part of its creation still owed, got {glue:?}"
    );
    assert_eq!(
        token, None,
        "a page that reported everything must hand back no token, got {token:?}"
    );

    // The catalog a listing reports is one the read of a catalog serves: the listing and
    // `GetDataCatalog` resolve the same set, so neither names a catalog the other does not.
    let (read_status, read) = call(
        &router,
        "GetDataCatalog",
        &json!({ "Name": GLUE_DATA_CATALOG_NAME }),
    )
    .await;
    assert_eq!(
        read_status, 200,
        "the Glue Data Catalog must be readable, got {read_status} with {read:?}"
    );
    let catalog = &read["DataCatalog"];
    assert_eq!(
        catalog["Name"].as_str(),
        Some(GLUE_DATA_CATALOG_NAME),
        "the read must report the catalog the listing named, got {catalog:?}"
    );
    for member in ["Type", "Status"] {
        assert_eq!(
            catalog[member], glue[member],
            "the read's {member} must be the listing's, got {catalog:?} and {glue:?}"
        );
    }
}

/// Every registered catalog is reported with the type and status its own record carries —
/// the ones `GetDataCatalog` answers with — beside the account's Glue Data Catalog, in name
/// order.
#[tokio::test]
async fn every_registered_catalog_is_reported_with_its_type_and_status() {
    let router = router();
    given_catalogs(&router).await;

    let (status, page, token) = list(&router, &json!({})).await;
    assert_eq!(status, 200, "the listing must answer 200, got {status}");
    assert_eq!(
        names(&page),
        vec![
            GLUE_DATA_CATALOG_NAME,
            "federated_new",
            "glue_other_account",
            "hive-metastore",
            "lambda_catalog",
        ],
        "every catalog the account holds must be reported in name order, got {page:?}"
    );
    assert_eq!(
        token, None,
        "a page that reported everything must hand back no token, got {token:?}"
    );

    for (case, name, request) in catalogs() {
        let summary = page
            .iter()
            .find(|summary| summary["CatalogName"] == json!(name))
            .unwrap_or_else(|| panic!("case {case:?}: expected {name} among {page:?}"));
        assert_eq!(
            summary["Type"], request["Type"],
            "case {case:?}: the summary must report the type the create registered, got \
             {summary:?}"
        );
        let (read_status, read) =
            call(&router, "GetDataCatalog", &json!({ "Name": name })).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: the catalog must be readable, got {read_status} with {read:?}"
        );
        let catalog = &read["DataCatalog"];
        assert_eq!(
            summary["Type"], catalog["Type"],
            "case {case:?}: the summary's type must be the read's, got {summary:?}"
        );
        assert_eq!(
            summary["ConnectionType"], catalog["ConnectionType"],
            "case {case:?}: the summary's connection type must be the read's, got {summary:?}"
        );
        assert_eq!(
            summary["Error"],
            Value::Null,
            "case {case:?}: a catalog that was created reports no error text, got {summary:?}"
        );
        // A federated catalog's creation is asynchronous, so the status the two calls read
        // depends on when each was made; what must hold for every type is that the status
        // is one the model declares for a creation.
        let status_reported = summary["Status"].as_str().unwrap_or("<no Status>");
        assert!(
            status_reported.starts_with("CREATE_"),
            "case {case:?}: expected a creation status, got {summary:?}"
        );
    }
}

/// Every page size walks the same catalogs in the same order, and the pages concatenate to
/// everything the account holds.
#[tokio::test]
async fn every_page_size_walks_every_catalog() {
    let router = router();
    given_catalogs(&router).await;
    let (_, whole, _) = list(&router, &json!({})).await;
    let whole = names(&whole);

    // `MaxDataCatalogsCount` is 2..50, so the smallest page a request may ask for is two;
    // the largest row asks for more catalogs than the account holds.
    for page_size in 2..=whole.len() + 1 {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut request = json!({ "MaxResults": page_size });
            if let Some(token) = &token {
                request["NextToken"] = json!(token);
            }
            let (status, page, next) = list(&router, &request).await;
            assert_eq!(
                status, 200,
                "page size {page_size}: the listing must answer 200, got {status}"
            );
            assert!(
                page.len() <= page_size,
                "page size {page_size}: a page must hold at most that many, got {page:?}"
            );
            pages += 1;
            walked.extend(names(&page));
            token = next;
            if token.is_none() || pages > whole.len() + 2 {
                break;
            }
        }
        assert_eq!(
            walked, whole,
            "page size {page_size}: the pages must concatenate to every catalog"
        );
        assert_eq!(
            token, None,
            "page size {page_size}: the last page must hand back no token, got {token:?}"
        );
    }
}

/// A `NextToken` this store did not mint for THIS listing names no page.
#[tokio::test]
async fn a_token_this_listing_did_not_mint_is_refused() {
    let router = router();
    given_catalogs(&router).await;

    // (case, the token sent) — a value that is not a token, a hex value nothing minted, and
    // a token this store DID mint for another listing of the same account.
    let (status, listed) = call(&router, "ListWorkGroups", &json!({ "MaxResults": 1 })).await;
    assert_eq!(
        status, 200,
        "the workgroup listing must answer 200, got {status} with {listed:?}"
    );
    let work_groups_token = listed["NextToken"].as_str().map(str::to_string);
    let mut rows: Vec<(&str, String)> = vec![
        ("a value that is not a token at all", "made-up-token".to_string()),
        ("a hex value nothing minted", ::hex::encode([0_u8; 48])),
    ];
    if let Some(token) = work_groups_token {
        rows.push(("a token minted for another listing", token));
    }

    for (case, token) in rows {
        let (status, body) = call(
            &router,
            "ListDataCatalogs",
            &json!({ "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_REQUEST,
            "case {case:?}: expected {INVALID_REQUEST}, got {body:?}"
        );
        assert_eq!(
            body["AthenaErrorCode"].as_str(),
            Some(ERROR_CODE_INVALID_INPUT),
            "case {case:?}: expected the input class, got {body:?}"
        );
        assert_eq!(
            message(&body),
            page_token_malformed(&token),
            "case {case:?}: expected the sentence for a token that names no page, got {:?}",
            message(&body)
        );
    }
}

/// A catalog a delete removed is no longer reported.
#[tokio::test]
async fn a_deleted_catalog_leaves_the_listing() {
    let router = router();
    given_catalogs(&router).await;

    let removed = "lambda_catalog";
    let (status, deleted) =
        call(&router, "DeleteDataCatalog", &json!({ "Name": removed })).await;
    assert_eq!(
        status, 200,
        "the delete must be accepted, got {status} with {deleted:?}"
    );

    let (status, page, _) = list(&router, &json!({})).await;
    assert_eq!(status, 200, "the listing must answer 200, got {status}");
    assert!(
        !names(&page).iter().any(|name| name == removed),
        "a deleted catalog must leave the listing, got {page:?}"
    );
}
