// SPDX-License-Identifier: CC0-1.0

//! The WORDING of the two refusals `CreateDataSource` states about the data source a request
//! describes, end to end over the restJson1 wire.
//!
//! The rules under test, one level above the two requests that filed the issue:
//!
//! 1. a create whose name is already taken WITHIN the API is refused 400
//!    `BadRequestException` stating "Data source with name " and " already exists" around the
//!    requested name, with no trailing stop: the fixed text is the service's, the one variable
//!    part is the name the REQUEST carried. The conflict is scoped per API, so the same name
//!    on another API is created;
//! 2. a create whose declared type is configured through a member the request does not carry
//!    is refused 400 `BadRequestException` stating "Missing " and " data source config" around
//!    the backend that configuration describes — the product's own name, without the vendor
//!    prefix the member's doc writes, so `eventBridgeConfig` is stated as `EventBridge` and
//!    `dynamodbConfig` as `DynamoDB`.
//!
//! The rows differ along what the rules generalise over: the NAME the conflict is about (a
//! plain name, one opening with an underscore, one carrying digits and underscores, a long
//! one), the API the name is taken on, and the declared TYPE whose configuration is missing —
//! every type this service configures through a member of its own, which is where the vendor
//! prefix of each backend's name is either dropped or not.
//!
//! The harness (`call`, `router`, `create_api`, `Answer`) is the error-contract suite's.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, create_api, router, Answer};

/// AppSync's own modelled error for a request that is not well formed, and its status
/// (`smithy_query(query="shape", shape="BadRequestException")`, `@httpError` 400).
const BAD_REQUEST: &str = "BadRequestException";
const BAD_REQUEST_STATUS: u16 = 400;

/// The names the conflict rows are about. A data source name is
/// `^[_A-Za-z][_0-9A-Za-z]*$`, 1..65536 (`smithy_query(query="shape", shape="ResourceName")`),
/// so the rows span a plain name, one opening with the underscore the pattern admits first,
/// one carrying digits and underscores, and a long one.
const NAMES: &[(&str, &str)] = &[
    ("a plain name", "orders"),
    ("a name opening with an underscore", "_orders"),
    ("a name carrying digits and underscores", "orders_by_region_1"),
    (
        "a long name",
        "orders_by_region_and_customer_and_warehouse_and_quarter",
    ),
];

/// The backend each configuration member names in the missing-configuration refusal: the
/// subject of that member's own doc on `CreateDataSourceRequest` with the vendor prefix
/// dropped — "the new Amazon DynamoDB configuration" stated as `DynamoDB`, "the new Amazon
/// EventBridge settings" as `EventBridge`, "the new Lambda configuration" as `Lambda`, "the
/// new OpenSearch configuration" as `OpenSearch`, "the new HTTP endpoint configuration" as
/// `HTTP endpoint` and "the new relational database configuration" as `relational database`.
/// Transcribed here independently of the service's own table, so a drift in either fails this
/// suite.
const MISSING_CONFIGS: &[(&str, &str)] = &[
    ("AWS_LAMBDA", "Lambda"),
    ("AMAZON_DYNAMODB", "DynamoDB"),
    ("AMAZON_ELASTICSEARCH", "OpenSearch"),
    ("AMAZON_OPENSEARCH_SERVICE", "OpenSearch"),
    ("HTTP", "HTTP endpoint"),
    ("RELATIONAL_DATABASE", "relational database"),
    ("AMAZON_EVENTBRIDGE", "EventBridge"),
];

#[tokio::test]
async fn a_name_already_taken_on_the_api_is_refused_in_the_services_own_words() {
    let router = router();
    let first = create_api(&router, "conflict-first").await;
    let second = create_api(&router, "conflict-second").await;
    for (case, name) in NAMES {
        let created = create_data_source(&router, &first, name).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the first create of a name is served, got {} {:?}",
            created.status, created.body
        );

        let refused = create_data_source(&router, &first, name).await;
        let expected = format!("Data source with name {name} already exists");
        assert_eq!(
            (
                refused.status,
                refused.error_type.as_str(),
                refused.message.as_str()
            ),
            (BAD_REQUEST_STATUS, BAD_REQUEST, expected.as_str()),
            "case {case:?}: expected the conflict stated as {expected:?}, got {} {:?}",
            refused.status,
            refused.body
        );

        // The conflict is the API's: the same name on another API is a data source of its own.
        let elsewhere = create_data_source(&router, &second, name).await;
        assert_eq!(
            elsewhere.status, 200,
            "case {case:?}: the same name on another API is served, got {} {:?}",
            elsewhere.status, elsewhere.body
        );
    }
}

#[tokio::test]
async fn a_type_whose_configuration_is_absent_is_refused_naming_that_backend() {
    let router = router();
    let api = create_api(&router, "missing-config").await;
    for (data_source_type, backend) in MISSING_CONFIGS {
        let answer = call(
            &router,
            "POST",
            &format!("/v1/apis/{api}/datasources"),
            Some(json!({
                "name": format!("without_{}", data_source_type.to_lowercase()),
                "type": data_source_type,
            })),
        )
        .await;
        let expected = format!("Missing {backend} data source config");
        assert_eq!(
            (
                answer.status,
                answer.error_type.as_str(),
                answer.message.as_str()
            ),
            (BAD_REQUEST_STATUS, BAD_REQUEST, expected.as_str()),
            "case {data_source_type:?}: expected the absent configuration stated as \
             {expected:?}, got {} {:?}",
            answer.status,
            answer.body
        );
    }
}

/// `CreateDataSource` for `name` on `api`, of the one type that needs no configuration at all
/// — `NONE`, "There is no data source" — so what a row asserts is the NAME and nothing else.
async fn create_data_source(router: &MultiServiceRouter, api: &str, name: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api}/datasources"),
        Some(json!({"name": name, "type": "NONE"})),
    )
    .await
}
