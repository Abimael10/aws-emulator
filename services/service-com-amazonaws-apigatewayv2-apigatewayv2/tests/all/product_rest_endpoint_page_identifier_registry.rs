// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for WHICH REST endpoints a `ProductRestEndpointPage` may
//! document: where the `restApiId` and `stage` of a `RestEndpointIdentifier` are resolved.
//!
//! The rule these cases assert:
//!
//! * **A `RestEndpointIdentifier` resolves when the ACCOUNT holds an api of the
//!   `restApiId` it names carrying a stage of the `stage` it names, in ANY of the stores
//!   those apis live in** — the REST APIs the older API Gateway control plane owns
//!   (`apigateway:CreateRestApi`, then a deployment to the stage) and the apis this service
//!   itself creates (`CreateApi`, `CreateStage`), which share one account-wide api-id space
//!   and one `execute-api` data plane. `CreateProductRestEndpointPage` then answers 201 with
//!   the modelled member set, and `UpdateProductRestEndpointPage`, which re-generates the
//!   page's content from the same endpoint, answers 200.
//! * **Only a pair NO store holds is refused** 400 `BadRequestException` with the fixed
//!   sentence `Invalid restEndpointIdentifier stage or restApiId`, and it persists no page.
//!   The pair is resolved AS A PAIR: an api of one store with the stage name of another
//!   store's api names no endpoint.
//!
//! The rows vary what the rule generalises over: which store holds the endpoint, the
//! protocol the api serves, which of several stages of one api the identifier names, the
//! stage's name (including the modelled maximum) and the method and path of the endpoint
//! documented. The recorded lifecycle that filed the issue — a page created over an api and
//! stage this service itself minted — is ONE row of the first table.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{
    as_str, assert_minted_id, call, emulator_with_rest_apis, error_code, rest_endpoint,
};
use crate::product_rest_endpoint_page_lifecycle::{
    content_member, invoke_url, max_stage, pages_of, product, REGION,
};

/// The sentence API Gateway refuses an identifier that names no endpoint of the account
/// with. Entirely fixed text — it names neither the api nor the stage.
const UNRESOLVED: &str = "Invalid restEndpointIdentifier stage or restApiId";

/// The statuses the modelled `Status` enum admits for a page
/// (`smithy_query(query="shape", shape="Status")`). Which one a fresh page reports follows
/// from the clock, so a row asserts only that the member carries one of them.
const STATUS_VALUES: [&str; 3] = ["AVAILABLE", "FAILED", "IN_PROGRESS"];

/// One api of the account, as the store that holds it reports it.
struct HeldApi {
    api_id: String,
    /// Every stage of that api, in the order it was created.
    stages: Vec<String>,
}

/// Create one api through THIS service with `stages` on it, and answer the `ApiId` it
/// minted.
///
/// This is the other store an identifier is resolved against: an api this service creates is
/// the account's as much as a REST API of the older control plane is, and its stages are
/// invoked on the same `execute-api` endpoint.
async fn v2_api(
    emulator: &MultiServiceRouter,
    case: &str,
    request: Value,
    stages: &[String],
) -> HeldApi {
    let (status, created) = call(emulator, "POST", "/v2/apis", Some(request)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {created}"
    );
    let api_id = as_str(&created, "apiId");
    for stage in stages {
        let (status, body) = call(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(json!({ "stageName": stage })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateStage for {stage:?}, got {status} {body}"
        );
    }
    HeldApi {
        api_id,
        stages: stages.to_vec(),
    }
}

/// One admitted row: an endpoint the account holds, and the method and path of it the page
/// documents.
struct Row {
    case: &'static str,
    /// Which store holds the api, built per row so one row's api cannot satisfy another's.
    endpoint: EndpointSource,
    method: &'static str,
    path: &'static str,
}

/// Where the api an identifier names lives.
enum EndpointSource {
    /// A REST API of the older control plane, with one stage of this name deployed on it.
    RestControlPlane { name: &'static str, stage: String },
    /// An api of THIS service: the `CreateApi` request, the stages to put on it, and which
    /// of those stages the identifier names.
    OwnControlPlane {
        request: Value,
        stages: Vec<String>,
        stage_index: usize,
    },
}

/// Create the endpoint a row documents and answer `(restApiId, stage)` as the identifier
/// must name them: the id the owning store MINTED, never one a case made up.
async fn held(
    emulator: &MultiServiceRouter,
    case: &str,
    source: &EndpointSource,
) -> (String, String) {
    match source {
        EndpointSource::RestControlPlane { name, stage } => {
            (rest_endpoint(emulator, name, stage).await, stage.clone())
        }
        EndpointSource::OwnControlPlane {
            request,
            stages,
            stage_index,
        } => {
            let api = v2_api(emulator, case, request.clone(), stages).await;
            let stage = api.stages[*stage_index].clone();
            (api.api_id, stage)
        }
    }
}

fn admitted_rows() -> Vec<Row> {
    vec![
        Row {
            case: "an endpoint of a REST API the older control plane holds",
            endpoint: EndpointSource::RestControlPlane {
                name: "orders-rest",
                stage: "prod".to_string(),
            },
            method: "GET",
            path: "/items",
        },
        Row {
            case: "an endpoint of an HTTP api this service holds",
            endpoint: EndpointSource::OwnControlPlane {
                request: json!({ "name": "orders-http", "protocolType": "HTTP" }),
                stages: vec!["live".to_string()],
                stage_index: 0,
            },
            method: "GET",
            path: "/items",
        },
        Row {
            case: "the second stage of an api this service holds, not its first",
            endpoint: EndpointSource::OwnControlPlane {
                request: json!({ "name": "multi-stage", "protocolType": "HTTP" }),
                stages: vec!["dev".to_string(), "beta".to_string(), "prod".to_string()],
                stage_index: 2,
            },
            method: "POST",
            path: "/orders/{orderId}",
        },
        Row {
            case: "a stage at the modelled maximum of IdentifierParts$Stage (len 1..128)",
            endpoint: EndpointSource::OwnControlPlane {
                request: json!({ "name": "max-stage", "protocolType": "HTTP" }),
                stages: vec![max_stage()],
                stage_index: 0,
            },
            method: "DELETE",
            path: "/",
        },
        Row {
            case: "an endpoint of a WEBSOCKET api this service holds",
            endpoint: EndpointSource::OwnControlPlane {
                request: json!({
                    "name": "socket-api",
                    "protocolType": "WEBSOCKET",
                    "routeSelectionExpression": "$request.body.action"
                }),
                stages: vec!["ws".to_string()],
                stage_index: 0,
            },
            method: "GET",
            path: "/items",
        },
    ]
}

/// The `identifierParts` of one endpoint, as a request writes them.
fn parts(rest_api_id: &str, stage: &str, method: &str, path: &str) -> Value {
    json!({
        "restApiId": rest_api_id,
        "stage": stage,
        "method": method,
        "path": path
    })
}

#[tokio::test]
async fn an_endpoint_any_store_of_the_accounts_apis_holds_is_documented() {
    let emulator = emulator_with_rest_apis();
    let product_id = product(&emulator, "Catalog").await;
    let collection = pages_of(&product_id);

    for row in admitted_rows() {
        let (rest_api_id, stage) = held(&emulator, row.case, &row.endpoint).await;
        let identifier_parts = parts(&rest_api_id, &stage, row.method, row.path);
        let (status, created) = call(
            &emulator,
            "POST",
            &collection,
            Some(json!({
                "restEndpointIdentifier": { "identifierParts": identifier_parts.clone() },
                "tryItState": "ENABLED"
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: expected 201 from CreateProductRestEndpointPage over an endpoint the \
             account holds, got {status} {created}",
            row.case
        );

        // The modelled success member set, so an admitted create is a page and not an empty
        // answer.
        let page_id = as_str(&created, "productRestEndpointPageId");
        assert_minted_id(row.case, "productRestEndpointPageId", &page_id);
        assert!(
            as_str(&created, "productRestEndpointPageArn").contains(&product_id),
            "case {:?}: expected the page's ARN to name its product, got {created}",
            row.case
        );
        assert_eq!(
            created.get("restEndpointIdentifier"),
            Some(&json!({ "identifierParts": identifier_parts.clone() })),
            "case {:?}: expected the identified endpoint echoed back, got {created}",
            row.case
        );
        assert_eq!(
            content_member(&created, "endpoint"),
            Some(invoke_url(&rest_api_id, &stage, REGION)),
            "case {:?}: expected the invoke URL of the identified stage, got {created}",
            row.case
        );
        let reported_status = as_str(&created, "status");
        assert!(
            STATUS_VALUES.contains(&reported_status.as_str()),
            "case {:?}: expected one of {STATUS_VALUES:?} as the page's status, got \
             {reported_status:?} {created}",
            row.case
        );
        assert_eq!(
            as_str(&created, "tryItState"),
            "ENABLED",
            "case {:?}: expected the try-it state the request asked for, got {created}",
            row.case
        );
        assert!(
            created.get("lastModified").is_some(),
            "case {:?}: expected the page to report when it was last modified, got {created}",
            row.case
        );

        // What the create reported is what a later read reports: the page really is stored
        // under the product the URI named.
        let (status, fetched) =
            call(&emulator, "GET", &format!("{collection}/{page_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetProductRestEndpointPage, got {status} {fetched}",
            row.case
        );
        assert_eq!(
            fetched.get("restEndpointIdentifier"),
            created.get("restEndpointIdentifier"),
            "case {:?}: expected the read page to document the endpoint the create stored, \
             got {fetched}",
            row.case
        );

        // The update RE-GENERATES the page's content from the same endpoint, so it resolves
        // that endpoint again: an endpoint the account holds is admitted there too.
        let (status, patched) = call(
            &emulator,
            "PATCH",
            &format!("{collection}/{page_id}"),
            Some(json!({ "displayContent": { "overrides": { "operationName": "ListItems" } } })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from UpdateProductRestEndpointPage over an endpoint the \
             account holds, got {status} {patched}",
            row.case
        );
        assert_eq!(
            content_member(&patched, "operationName"),
            Some("ListItems".to_string()),
            "case {:?}: expected the patched operation name, got {patched}",
            row.case
        );
    }
}

#[tokio::test]
async fn an_endpoint_no_store_of_the_accounts_apis_holds_is_refused() {
    let emulator = emulator_with_rest_apis();
    let product_id = product(&emulator, "Catalog").await;
    let collection = pages_of(&product_id);

    // One api in each store, with stage names of its own, so every refused row below names
    // parts that DO exist somewhere — just not as one pair of one api.
    let rest_api_id = rest_endpoint(&emulator, "held-rest", "restprod").await;
    let own = v2_api(
        &emulator,
        "the account's own api",
        json!({ "name": "held-http", "protocolType": "HTTP" }),
        &["ownprod".to_string()],
    )
    .await;
    let stageless = v2_api(
        &emulator,
        "an api with no stage at all",
        json!({ "name": "stageless", "protocolType": "HTTP" }),
        &[],
    )
    .await;

    let rows = vec![
        (
            "an api id nothing minted, with a stage name an api does have",
            parts("nosuchapi0", "restprod", "GET", "/items"),
        ),
        (
            "a REST API of the account with the stage name of another api",
            parts(&rest_api_id, "ownprod", "GET", "/items"),
        ),
        (
            "an api of this service with the stage name of the REST API",
            parts(&own.api_id, "restprod", "GET", "/items"),
        ),
        (
            "an api of this service that carries no stage at all",
            parts(&stageless.api_id, "ownprod", "GET", "/items"),
        ),
        (
            "an api id that differs from a held one only in case",
            parts(&own.api_id.to_uppercase(), "ownprod", "GET", "/items"),
        ),
        (
            "a stage name that differs from a held one only in case",
            parts(&own.api_id, "OWNPROD", "GET", "/items"),
        ),
    ];

    for (case, identifier_parts) in rows {
        let (status, body) = call(
            &emulator,
            "POST",
            &collection,
            Some(json!({
                "restEndpointIdentifier": { "identifierParts": identifier_parts }
            })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from CreateProductRestEndpointPage, got {status} \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            UNRESOLVED,
            "case {case:?}: expected the refusal to read {UNRESOLVED:?}, got {body}"
        );
    }

    // Every create above was refused, so the product holds no page: the refusals persisted
    // nothing, and the pair that DOES resolve is the first page it answers for.
    let (status, listed) = call(
        &emulator,
        "POST",
        &collection,
        Some(json!({
            "restEndpointIdentifier": {
                "identifierParts": parts(&own.api_id, "ownprod", "GET", "/items")
            }
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 for the pair one store holds, got {status} {listed}"
    );
}
