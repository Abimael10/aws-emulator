// SPDX-License-Identifier: CC0-1.0

//! The data sources of an API of EITHER resource family, and the channel-namespace
//! handlers that integrate with them, end to end over HTTP.
//!
//! A data source belongs to the API its `apiId` path label names
//! (`/v1/apis/{apiId}/datasources`), and that label is ONE identifier space over both
//! families AppSync creates APIs in — a `GraphqlApi` (`CreateGraphqlApi`, POST /v1/apis)
//! and an `Api` (`CreateApi`, POST /v2/apis) — the same way the API-key surface's label
//! is (see `crate::api_keys_across_api_families`). Two rules follow, each asserted over a
//! TABLE spanning the class it generalises over rather than one recorded request:
//!
//! 1. every data-source operation (`CreateDataSource`, `GetDataSource`,
//!    `ListDataSources`, `UpdateDataSource`, `DeleteDataSource`) resolves the `apiId` of
//!    an API of either family, in either published form (the minted id and the API's
//!    ARN), so a data source can be created, read, listed, updated and deleted under any
//!    stored API — and a data source belongs to ONE API, so a name held only by another
//!    API is not found under this one;
//! 2. a channel namespace's `handlerConfigs` integration names "the unique name of the
//!    data source that has been configured on the API" (`Integration$dataSourceName`), so
//!    a name the `Api` does not hold is refused with `BadRequestException` (400)
//!    "DataSource not found." and no namespace is created, while the same request naming a
//!    data source the API holds succeeds and reads the integration back.
//!
//! Every assertion goes through the router the production emulator mounts. The harness
//! (`call`, `router`, `Answer`, `create_api`) is the error-contract suite's; the Event API
//! and namespace helpers are the channel-namespace suite's.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_identifier_resolution::label;
use crate::channel_namespaces::{create_event_api, create_namespace, get_namespace, modes};
use crate::error_contract::{call, create_api, router, Answer};

/// AppSync's fixed text for an API that is absent.
const API_NOT_FOUND: &str = "API not found.";

/// AppSync's fixed text for a handler integration naming a data source the API does not
/// hold.
const DATA_SOURCE_NOT_FOUND: &str = "DataSource not found.";

/// The data-source types a row drives the whole lifecycle with, each with the
/// configuration its type needs: `NONE` needs none, and `AWS_LAMBDA` needs the Lambda
/// configuration and the service role AppSync assumes to invoke it. Source: the
/// `DataSourceType` value set and the per-type configuration members of
/// `CreateDataSourceRequest` (`smithy_query(query="operation",
/// operation="CreateDataSource")`).
fn data_source_bodies(name: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("NONE", json!({"name": name, "type": "NONE"})),
        (
            "AWS_LAMBDA",
            json!({
                "name": name,
                "type": "AWS_LAMBDA",
                "lambdaConfig": {
                    "lambdaFunctionArn":
                        format!("arn:aws:lambda:eu-west-1:000000000000:function:{name}"),
                },
                "serviceRoleArn": "arn:aws:iam::000000000000:role/appsync-data-source",
            }),
        ),
    ]
}

/// Create one data source under the API `api_identifier` and return the answer.
async fn create_data_source(
    router: &MultiServiceRouter,
    api_identifier: &str,
    body: Value,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_identifier}/datasources"),
        Some(body),
    )
    .await
}

/// Read one data source back.
async fn get_data_source(router: &MultiServiceRouter, api_identifier: &str, name: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/v1/apis/{api_identifier}/datasources/{name}"),
        None,
    )
    .await
}

/// The data-source names `ListDataSources` reports for `api_identifier`.
async fn listed_names(router: &MultiServiceRouter, api_identifier: &str) -> Vec<String> {
    let listed = call(
        router,
        "GET",
        &format!("/v1/apis/{api_identifier}/datasources"),
        None,
    )
    .await;
    listed.body["dataSources"]
        .as_array()
        .map(|sources| {
            sources
                .iter()
                .map(|source| source["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Rule 1: the whole data-source lifecycle runs under an API of either family, addressed
/// in either published form.
#[tokio::test]
async fn the_data_source_lifecycle_runs_under_an_api_of_either_family() {
    let router = router();

    // The two families, each created by its own operation, and the two published forms
    // of the identity either one answers with.
    let graphql_id = create_api(&router, "data-sources-either-family").await;
    let graphql_arn = call(&router, "GET", &format!("/v1/apis/{graphql_id}"), None)
        .await
        .body["graphqlApi"]["arn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let event_id = create_event_api(&router, "Events_data_sources", &["API_KEY"]).await;
    let event_arn = call(&router, "GET", &format!("/v2/apis/{event_id}"), None)
        .await
        .body["api"]["apiArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    for (family, api_id) in [("GraphqlApi", &graphql_id), ("Api", &event_id)] {
        for (type_name, body) in data_source_bodies("orders") {
            let case = format!("{family} / {type_name}");

            let created = create_data_source(&router, api_id, body.clone()).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected CreateDataSource 200, got {created:?}"
            );
            assert_eq!(
                created.body["dataSource"]["type"],
                json!(type_name),
                "case {case:?}: expected the submitted type back, got {:?}",
                created.body["dataSource"]
            );
            // The data source is named under the API's OWN id, whichever form named it.
            assert!(
                created.body["dataSource"]["dataSourceArn"]
                    .as_str()
                    .is_some_and(|arn| arn.contains(api_id.as_str())),
                "case {case:?}: expected the ARN to name the API, got {:?}",
                created.body["dataSource"]["dataSourceArn"]
            );

            // Read back, and listed.
            let read = get_data_source(&router, api_id, "orders").await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected GetDataSource 200, got {read:?}"
            );
            assert_eq!(
                read.body["dataSource"], created.body["dataSource"],
                "case {case:?}: expected the stored data source back, got {:?}",
                read.body["dataSource"]
            );
            assert_eq!(
                listed_names(&router, api_id).await,
                vec!["orders".to_string()],
                "case {case:?}: expected the listing to hold the created data source"
            );

            // Updated: the type is replaced and the reader reports the replacement.
            let updated = call(
                &router,
                "POST",
                &format!("/v1/apis/{api_id}/datasources/orders"),
                Some(json!({"name": "orders", "type": "NONE", "description": "updated"})),
            )
            .await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: expected UpdateDataSource 200, got {updated:?}"
            );
            assert_eq!(
                get_data_source(&router, api_id, "orders").await.body["dataSource"]["description"],
                json!("updated"),
                "case {case:?}: expected the update to be stored"
            );

            // Deleted: gone from the reader and from the listing.
            let deleted = call(
                &router,
                "DELETE",
                &format!("/v1/apis/{api_id}/datasources/orders"),
                None,
            )
            .await;
            assert_eq!(
                deleted.status, 200,
                "case {case:?}: expected DeleteDataSource 200, got {deleted:?}"
            );
            assert_eq!(
                get_data_source(&router, api_id, "orders").await.status,
                404,
                "case {case:?}: expected the deleted data source to be absent"
            );
            assert!(
                listed_names(&router, api_id).await.is_empty(),
                "case {case:?}: expected an empty listing after the delete"
            );
        }
    }

    // Either published form of either API's identity reaches the same API's data sources.
    // An ARN goes into the single non-greedy `{apiId}` segment percent-encoded, the way an
    // AWS SDK sends one (`crate::api_identifier_resolution::label`).
    for (case, identifier) in [
        ("a GraphqlApi by its id", label(&graphql_id)),
        ("a GraphqlApi by its ARN", label(&graphql_arn)),
        ("an Api by its id", label(&event_id)),
        ("an Api by its ARN", label(&event_arn)),
    ] {
        let created = create_data_source(
            &router,
            &identifier,
            json!({"name": "by_reference", "type": "NONE"}),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateDataSource 200, got {created:?}"
        );
        let deleted = call(
            &router,
            "DELETE",
            &format!("/v1/apis/{identifier}/datasources/by_reference"),
            None,
        )
        .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected DeleteDataSource 200, got {deleted:?}"
        );
    }

    // A data source belongs to ONE API: a name held under the Event API is not found
    // under the GraphQL one, and an `apiId` that names no API of either family is the
    // absent API.
    let created = create_data_source(&router, &event_id, json!({"name": "only_here", "type": "NONE"})).await;
    assert_eq!(
        created.status, 200,
        "case seed the Event API's data source: expected 200, got {created:?}"
    );
    let crossed = get_data_source(&router, &graphql_id, "only_here").await;
    assert_eq!(
        crossed.status, 404,
        "case a name held by the other API: expected 404, got {crossed:?}"
    );
    let absent = get_data_source(&router, "aaaaaaaaaaaaaaaaaaaaaaaaaa", "only_here").await;
    assert_eq!(
        (absent.status, absent.message.as_str()),
        (404, API_NOT_FOUND),
        "case an apiId nothing created: expected the absent API, got {absent:?}"
    );
}

/// Rule 2: a channel-namespace handler integrates with a data source the `Api` holds —
/// a name it does not hold is refused and creates nothing.
#[tokio::test]
async fn a_channel_namespace_handler_integrates_with_a_data_source_the_api_holds() {
    let router = router();
    let api_id = create_event_api(&router, "Events_handler_configs", &["API_KEY"]).await;
    let created = create_data_source(&router, &api_id, json!({"name": "orders", "type": "NONE"})).await;
    assert_eq!(
        created.status, 200,
        "case seed the data source: expected 200, got {created:?}"
    );

    // Both handlers, and both behaviours a handler can take (`HandlerBehavior`: `CODE`,
    // `DIRECT` — `smithy_query(query="shape", shape="HandlerBehavior")`), each named
    // against a data source the API does not hold.
    let rows: Vec<(&str, Value)> = vec![
        (
            "onPublish DIRECT naming a data source nothing created",
            json!({"onPublish": {"behavior": "DIRECT", "integration": {"dataSourceName": "absent"}}}),
        ),
        (
            "onPublish CODE naming a data source nothing created",
            json!({"onPublish": {"behavior": "CODE", "integration": {"dataSourceName": "absent"}}}),
        ),
        (
            "onSubscribe DIRECT naming a data source nothing created",
            json!({"onSubscribe": {"behavior": "DIRECT", "integration": {"dataSourceName": "absent"}}}),
        ),
        (
            "one handler on the stored data source and one on a name nothing created",
            json!({
                "onPublish": {"behavior": "DIRECT", "integration": {"dataSourceName": "orders"}},
                "onSubscribe": {"behavior": "CODE", "integration": {"dataSourceName": "absent"}},
            }),
        ),
    ];

    for (index, (case, handler_configs)) in rows.iter().enumerate() {
        let name = format!("refused-{index}");
        let answer = create_namespace(
            &router,
            &api_id,
            json!({
                "name": name,
                "publishAuthModes": modes(&["API_KEY"]),
                "subscribeAuthModes": modes(&["API_KEY"]),
                "handlerConfigs": handler_configs,
            }),
        )
        .await;
        assert_eq!(
            (
                answer.status,
                answer.error_type.as_str(),
                answer.message.as_str()
            ),
            (400, "BadRequestException", DATA_SOURCE_NOT_FOUND),
            "case {case:?}: expected the absent-data-source refusal, got {answer:?}"
        );
        assert_eq!(
            get_namespace(&router, &api_id, &name).await.status,
            404,
            "case {case:?}: expected no namespace to be created"
        );
    }

    // The same shape naming the data source the API holds succeeds, and the integration
    // is read back.
    let accepted = create_namespace(
        &router,
        &api_id,
        json!({
            "name": "accepted",
            "publishAuthModes": modes(&["API_KEY"]),
            "subscribeAuthModes": modes(&["API_KEY"]),
            "handlerConfigs": {
                "onPublish": {"behavior": "DIRECT", "integration": {"dataSourceName": "orders"}},
            },
        }),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "case a handler on the stored data source: expected 200, got {accepted:?}"
    );
    assert_eq!(
        get_namespace(&router, &api_id, "accepted").await.body["channelNamespace"]
            ["handlerConfigs"]["onPublish"]["integration"]["dataSourceName"],
        json!("orders"),
        "case a handler on the stored data source: expected the integration read back"
    );

    // And the update path, which carries the same member, is refused on the same ground
    // and leaves the stored handler alone.
    let updated = call(
        &router,
        "POST",
        &format!("/v2/apis/{api_id}/channelNamespaces/accepted"),
        Some(json!({
            "handlerConfigs": {
                "onPublish": {"behavior": "DIRECT", "integration": {"dataSourceName": "absent"}},
            },
        })),
    )
    .await;
    assert_eq!(
        (
            updated.status,
            updated.error_type.as_str(),
            updated.message.as_str()
        ),
        (400, "BadRequestException", DATA_SOURCE_NOT_FOUND),
        "case an update naming a data source nothing created: expected the refusal, got \
         {updated:?}"
    );
    assert_eq!(
        get_namespace(&router, &api_id, "accepted").await.body["channelNamespace"]
            ["handlerConfigs"]["onPublish"]["integration"]["dataSourceName"],
        json!("orders"),
        "case an update naming a data source nothing created: expected the stored handler to \
         survive"
    );
}
