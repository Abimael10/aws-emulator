// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ListPipelines` over an account that holds no pipeline.
//!
//! The RULE asserted below: a paginated listing with nothing to page through has exactly ONE
//! page — the empty one — and it is the last, so an account holding no pipeline is answered
//! 200 with an empty `Pipelines` list and no `NextToken`, whatever pagination inputs the
//! request carries. A `NextToken` names a POSITION to resume from, so it is read only where
//! there are records to resume: once the account holds pipelines, a token this service never
//! minted names no position in that listing and is refused 400
//! `InvalidPaginationTokenException` (`tests/all/pipeline_lifecycle.rs` has that half's own
//! table), and the same token is answered with the empty page again once the account is
//! emptied.
//!
//! The rows differ along what the rule generalises over: no pagination input at all, each
//! published `MaxResults` bound, the empty token the member's `@length(0, 3000)` admits, a
//! token this service minted, and tokens no listing of this service could have minted —
//! including one of the opaque form another service's listing hands out.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A Region that is NOT the emulator default, so nothing here passes because it was pinned
/// to the default one.
const REGION: &str = "eu-west-2";

/// What one call answered: the status, the modelled error code (empty on success) and the
/// parsed body.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

fn new_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised elsewhere;
    // these cases are about this service's behaviour.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_osis_amazonopensearchingestionservice::EmulatedService::new(engine),
    );
    router
}

async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Answer {
    let payload = match body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://osis.{REGION}.amazonaws.com{path}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/osis/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("build request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(['/', ':'])
                .next()
                .unwrap_or(value)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error_type,
        body,
    }
}

async fn list_pipelines(router: &MultiServiceRouter, query: &str) -> Answer {
    let path = if query.is_empty() {
        "/2022-01-01/osis/listPipelines".to_string()
    } else {
        format!("/2022-01-01/osis/listPipelines?{query}")
    };
    call(router, "GET", &path, None).await
}

async fn create_pipeline(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "POST",
        "/2022-01-01/osis/createPipeline",
        Some(json!({
            "PipelineName": name,
            "MinUnits": 1,
            "MaxUnits": 1,
            "PipelineConfigurationBody": "version: \"2\"\n",
        })),
    )
    .await
}

async fn delete_pipeline(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/2022-01-01/osis/deletePipeline/{name}"),
        None,
    )
    .await
}

/// A token no listing of THIS service minted: the opaque form a service's own listing hands
/// a client, which this service cannot resolve to a position of its own.
const FOREIGN_TOKEN: &str = "eyJuZXh0IjoicGlwZWxpbmUtMDAxIn0";

/// Every pagination input an empty account is listed with. The `MaxResults` values are the
/// published `@range(1, 100)` bounds; the `NextToken` values are the empty string the
/// member's `@length(0, 3000)` admits and tokens of forms this service could not have
/// minted.
const EMPTY_ACCOUNT_QUERIES: &[&str] = &[
    "",
    "maxResults=1",
    "maxResults=100",
    "nextToken=",
    "nextToken=not-a-cursor",
    "nextToken=6f7468657273657276696365",
    "maxResults=100&nextToken=eyJuZXh0IjoicGlwZWxpbmUtMDAxIn0",
    "maxResults=1&nextToken=not-a-cursor",
];

/// An account with no pipeline is a successful empty page, whatever pagination inputs the
/// request carries.
#[tokio::test]
async fn an_account_with_no_pipeline_is_one_empty_page() {
    let router = new_router();
    for query in EMPTY_ACCOUNT_QUERIES {
        let listed = list_pipelines(&router, query).await;
        assert_eq!(
            listed.status, 200,
            "case {query:?}: an empty account listing is a success, got {} / {:?}",
            listed.status, listed.body
        );
        assert_eq!(
            listed.body["Pipelines"].as_array().map(Vec::len),
            Some(0),
            "case {query:?}: expected an empty Pipelines list, got {:?}",
            listed.body["Pipelines"]
        );
        assert!(
            listed.body.get("NextToken").is_none_or(Value::is_null),
            "case {query:?}: the one empty page is the last, got {:?}",
            listed.body["NextToken"]
        );
    }
}

/// The token is read where it names a position to resume from: with records in the account a
/// token this service never minted is refused, and emptying the account answers the same
/// token with the empty page again.
#[tokio::test]
async fn a_token_is_resolved_only_against_an_account_that_has_records() {
    let router = new_router();
    let name = "token-position-pipeline";
    let created = create_pipeline(&router, name).await;
    assert_eq!(
        created.status, 200,
        "setup CreatePipeline answered {} / {:?}",
        created.status, created.body
    );

    let case = "a foreign token while the account holds a pipeline";
    let refused = list_pipelines(&router, &format!("nextToken={FOREIGN_TOKEN}")).await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected 400, got {} / {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_type, "InvalidPaginationTokenException",
        "case {case:?}: expected InvalidPaginationTokenException, got {:?}",
        refused.error_type
    );

    // A token this service DID mint resumes the listing it came from: with one pipeline and a
    // page size of one, the first page carries a token and the next page is empty.
    let case = "a token this service minted";
    let second = create_pipeline(&router, "token-position-second").await;
    assert_eq!(
        second.status, 200,
        "case {case:?}: setup CreatePipeline answered {} / {:?}",
        second.status, second.body
    );
    let first_page = list_pipelines(&router, "maxResults=1").await;
    let minted = first_page.body["NextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: a first page of two pipelines carries a token, got {:?}",
                first_page.body
            )
        })
        .to_string();
    let next_page = list_pipelines(&router, &format!("maxResults=1&nextToken={minted}")).await;
    assert_eq!(
        next_page.status, 200,
        "case {case:?}: expected 200, got {} / {:?}",
        next_page.status, next_page.body
    );
    assert_eq!(
        next_page.body["Pipelines"].as_array().map(Vec::len),
        Some(1),
        "case {case:?}: the resumed page holds the pipeline the first page did not, got {:?}",
        next_page.body["Pipelines"]
    );

    let case = "the same foreign token once the account is emptied";
    for name in [name, "token-position-second"] {
        let deleted = delete_pipeline(&router, name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: setup DeletePipeline answered {} / {:?}",
            deleted.status, deleted.body
        );
    }
    let listed = list_pipelines(&router, &format!("nextToken={FOREIGN_TOKEN}")).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: an empty account listing is a success, got {} / {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.body["Pipelines"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected an empty Pipelines list, got {:?}",
        listed.body["Pipelines"]
    );
}
