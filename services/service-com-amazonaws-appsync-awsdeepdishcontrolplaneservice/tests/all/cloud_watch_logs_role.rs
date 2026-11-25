// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Logs role an API is configured with, end to end over HTTP.
//!
//! Both log configurations this service takes carry one required member with the same
//! name and the same documentation — `LogConfig$cloudWatchLogsRoleArn`, "The service role
//! that AppSync assumes to publish to CloudWatch logs in your account"
//! (`CreateGraphqlApi`/`UpdateGraphqlApi`), and `EventLogConfig$cloudWatchLogsRoleArn`,
//! "The IAM service role that AppSync assumes to publish CloudWatch Logs in your account"
//! (`CreateApi`/`UpdateApi`) — both targeting a bare `String`
//! (`smithy_query(query="shape", shape="LogConfig,EventLogConfig")`), so the form of the
//! value is the service's to check and all four operations model `BadRequestException`
//! (400).
//!
//! ONE rule, asserted over a TABLE of values spanning the class rather than the single
//! recorded one: a role is assumed by its ARN, so a value that is not a well-formed IAM
//! ROLE ARN names no role AppSync could assume and the call is refused with AppSync's own
//! sentence, creating and changing nothing — while a value that IS one is stored and read
//! back verbatim. Asserted on every operation that carries either member, and the refusal
//! is checked to leave no resource behind (the name is absent from the listing) and no
//! stored configuration changed.
//!
//! Every assertion goes through the router the production emulator mounts. The harness
//! (`call`, `router`, `Answer`) is the error-contract suite's.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, router, Answer};

/// AppSync's fixed sentence for a `cloudWatchLogsRoleArn` it could not assume.
const INVALID_ROLE: &str =
    "The cloudWatchLogsRoleArn is invalid. Please refer to the documentation for more details.";

/// Values that are NOT the ARN of an IAM role, each with the reason it is not one: no ARN
/// at all, an ARN of another vendor, and an IAM ARN naming something other than a role.
const NOT_ROLE_ARNS: &[(&str, &str)] = &[
    ("no ARN at all", "not-a-valid-arn"),
    ("an empty value", ""),
    ("an ARN missing its resource", "arn:aws:iam::000000000000:"),
    (
        "an ARN of another vendor",
        "arn:aws:logs:eu-west-1:000000000000:log-group:/aws/appsync/apis",
    ),
    (
        "an IAM user rather than a role",
        "arn:aws:iam::000000000000:user/someone",
    ),
    (
        "an IAM policy rather than a role",
        "arn:aws:iam::000000000000:policy/appsync-logs",
    ),
    (
        "a role-like resource type that is not role",
        "arn:aws:iam::000000000000:rolename/appsync-logs",
    ),
];

/// Values that ARE the ARN of an IAM role, spanning the forms a role ARN takes: this
/// account and another, with and without a path.
const ROLE_ARNS: &[(&str, &str)] = &[
    (
        "a role of this account",
        "arn:aws:iam::000000000000:role/appsync-logs",
    ),
    (
        "a role of another account",
        "arn:aws:iam::123456789012:role/appsync-logs",
    ),
    (
        "a role under a path",
        "arn:aws:iam::000000000000:role/service-role/appsync-logs",
    ),
];

/// A `CreateGraphqlApi`/`UpdateGraphqlApi` body whose `logConfig` names `role_arn`.
fn graphql_api_body(name: &str, role_arn: &str) -> Value {
    json!({
        "name": name,
        "authenticationType": "API_KEY",
        "queryDepthLimit": 0,
        "resolverCountLimit": 0,
        "xrayEnabled": false,
        "logConfig": {
            "fieldLogLevel": "ERROR",
            "cloudWatchLogsRoleArn": role_arn,
            "excludeVerboseContent": false,
        },
    })
}

/// A `CreateApi`/`UpdateApi` body whose `eventConfig.logConfig` names `role_arn`.
fn event_api_body(name: &str, role_arn: &str) -> Value {
    json!({
        "name": name,
        "eventConfig": {
            "authProviders": [{"authType": "API_KEY"}],
            "connectionAuthModes": [{"authType": "API_KEY"}],
            "defaultPublishAuthModes": [{"authType": "API_KEY"}],
            "defaultSubscribeAuthModes": [{"authType": "API_KEY"}],
            "logConfig": {"logLevel": "ERROR", "cloudWatchLogsRoleArn": role_arn},
        },
    })
}

/// Whether a listing of APIs reports one named `name`, read through the member the
/// listing answers under.
async fn listing_holds(router: &MultiServiceRouter, path: &str, member: &str, name: &str) -> bool {
    let listed = call(router, "GET", path, None).await;
    listed.body[member]
        .as_array()
        .is_some_and(|apis| apis.iter().any(|api| api["name"] == json!(name)))
}

/// Assert `answer` is the service's refusal for a role it could not assume.
fn assert_refused(case: &str, operation: &str, answer: &Answer) {
    assert_eq!(
        (
            answer.status,
            answer.error_type.as_str(),
            answer.message.as_str()
        ),
        (400, "BadRequestException", INVALID_ROLE),
        "case {case:?} ({operation}): expected the invalid-role refusal, got {answer:?}"
    );
}

/// Rule: a `logConfig.cloudWatchLogsRoleArn` that names no IAM role is refused by
/// `CreateGraphqlApi`, which then creates nothing, and by `UpdateGraphqlApi`, which then
/// leaves the stored configuration alone; a role ARN is stored and read back verbatim.
#[tokio::test]
async fn a_graphql_api_log_role_must_be_an_iam_role_arn() {
    let router = router();

    // A value that is a role ARN is accepted and reported back exactly as submitted.
    for (index, (case, role_arn)) in ROLE_ARNS.iter().enumerate() {
        let name = format!("Api_log_role_{index}");
        let created = call(
            &router,
            "POST",
            "/v1/apis",
            Some(graphql_api_body(&name, role_arn)),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateGraphqlApi 200, got {created:?}"
        );
        assert_eq!(
            created.body["graphqlApi"]["logConfig"]["cloudWatchLogsRoleArn"],
            json!(role_arn),
            "case {case:?}: expected the role ARN back, got {:?}",
            created.body["graphqlApi"]["logConfig"]
        );
        let api_id = created.body["graphqlApi"]["apiId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let read = call(&router, "GET", &format!("/v1/apis/{api_id}"), None).await;
        assert_eq!(
            read.body["graphqlApi"]["logConfig"]["cloudWatchLogsRoleArn"],
            json!(role_arn),
            "case {case:?} (GetGraphqlApi): expected the role ARN back, got {:?}",
            read.body["graphqlApi"]["logConfig"]
        );
    }

    // One stored API, whose configuration a refused update must not change.
    let stored_role = ROLE_ARNS[0].1;
    let stored = call(
        &router,
        "POST",
        "/v1/apis",
        Some(graphql_api_body("Api_log_role_stored", stored_role)),
    )
    .await;
    let api_id = stored.body["graphqlApi"]["apiId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    for (index, (case, role_arn)) in NOT_ROLE_ARNS.iter().enumerate() {
        let name = format!("Api_log_role_refused_{index}");
        let created = call(
            &router,
            "POST",
            "/v1/apis",
            Some(graphql_api_body(&name, role_arn)),
        )
        .await;
        assert_refused(case, "CreateGraphqlApi", &created);
        assert!(
            !listing_holds(&router, "/v1/apis", "graphqlApis", &name).await,
            "case {case:?}: expected no API named {name:?} after the refusal"
        );

        let updated = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}"),
            Some(graphql_api_body("Api_log_role_stored", role_arn)),
        )
        .await;
        assert_refused(case, "UpdateGraphqlApi", &updated);
        let read = call(&router, "GET", &format!("/v1/apis/{api_id}"), None).await;
        assert_eq!(
            read.body["graphqlApi"]["logConfig"]["cloudWatchLogsRoleArn"],
            json!(stored_role),
            "case {case:?}: expected the stored role to survive the refused update, got {:?}",
            read.body["graphqlApi"]["logConfig"]
        );
    }
}

/// Rule: the same holds for an `Api`'s `eventConfig.logConfig.cloudWatchLogsRoleArn`, on
/// `CreateApi` and `UpdateApi`.
#[tokio::test]
async fn an_event_api_log_role_must_be_an_iam_role_arn() {
    let router = router();

    for (index, (case, role_arn)) in ROLE_ARNS.iter().enumerate() {
        let name = format!("Events_log_role_{index}");
        let created = call(&router, "POST", "/v2/apis", Some(event_api_body(&name, role_arn))).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateApi 200, got {created:?}"
        );
        assert_eq!(
            created.body["api"]["eventConfig"]["logConfig"]["cloudWatchLogsRoleArn"],
            json!(role_arn),
            "case {case:?}: expected the role ARN back, got {:?}",
            created.body["api"]["eventConfig"]["logConfig"]
        );
        let api_id = created.body["api"]["apiId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let read = call(&router, "GET", &format!("/v2/apis/{api_id}"), None).await;
        assert_eq!(
            read.body["api"]["eventConfig"]["logConfig"]["cloudWatchLogsRoleArn"],
            json!(role_arn),
            "case {case:?} (GetApi): expected the role ARN back, got {:?}",
            read.body["api"]["eventConfig"]["logConfig"]
        );
    }

    let stored_role = ROLE_ARNS[0].1;
    let stored = call(
        &router,
        "POST",
        "/v2/apis",
        Some(event_api_body("Events_log_role_stored", stored_role)),
    )
    .await;
    let api_id = stored.body["api"]["apiId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    for (index, (case, role_arn)) in NOT_ROLE_ARNS.iter().enumerate() {
        // `ApiName` admits letters, digits, spaces, `_` and `-` only, up to 50 characters
        // (`smithy_query(query="shape", shape="ApiName")`), so the name is keyed by the
        // row's index rather than by its label.
        let name = format!("Events_refused_{index}");
        let created = call(&router, "POST", "/v2/apis", Some(event_api_body(&name, role_arn))).await;
        assert_refused(case, "CreateApi", &created);
        assert!(
            !listing_holds(&router, "/v2/apis", "apis", &name).await,
            "case {case:?}: expected no Api named {name:?} after the refusal"
        );

        let updated = call(
            &router,
            "POST",
            &format!("/v2/apis/{api_id}"),
            Some(event_api_body("Events_log_role_stored", role_arn)),
        )
        .await;
        assert_refused(case, "UpdateApi", &updated);
        let read = call(&router, "GET", &format!("/v2/apis/{api_id}"), None).await;
        assert_eq!(
            read.body["api"]["eventConfig"]["logConfig"]["cloudWatchLogsRoleArn"],
            json!(stored_role),
            "case {case:?}: expected the stored role to survive the refused update, got {:?}",
            read.body["api"]["eventConfig"]["logConfig"]
        );
    }
}
