// SPDX-License-Identifier: CC0-1.0

//! The CONFIGURATION a data source is created and updated with, and the refusals a
//! request that does not describe a data source AppSync could reach takes, end to end
//! over the restJson1 wire.
//!
//! The rules under test, one level above the requests that filed the issue:
//!
//! 1. a create or update carrying a per-type configuration member the declared type does
//!    NOT name — a second one beside the type's own, or one in place of it — is refused as
//!    too many configurations: 400 `BadRequestException` naming the declared type by the
//!    service's own name for it, rather than as the absence of the one its type needs;
//! 2. a `RELATIONAL_DATABASE` data source whose `relationalDatabaseConfig` names no
//!    `relationalDatabaseSourceType` says nothing about how the database is reached, so
//!    it is refused too;
//! 3. a request carrying NO configuration at all is still refused as the ABSENT one, so
//!    the two faults keep their own sentences; and the declared type's own configuration,
//!    alone, is served and round-trips through `GetDataSource`;
//! 4. a data source that would act with the CALLER's own credentials
//!    (`DynamodbDataSourceConfig$useCallerCredentials`) needs an API that authenticates
//!    its callers with IAM, because that is where the identity it would act as comes
//!    from, so on an API in any other authorization mode it is refused.
//!
//! The rows differ along what the rules generalise over: the OPERATION (every row is run
//! through both `CreateDataSource` and `UpdateDataSource`), the declared TYPE, WHICH
//! configuration members the request carries, HOW MANY of them it carries, and whether
//! the declared type's own member is among them.
//!
//! The harness (`call`, `router`, `create_api`, `Answer`) is the error-contract suite's,
//! and the per-type configuration fragments are the service-role suite's catalog.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::data_source_service_role::{config_for, DATA_SOURCE_TYPES, ROLE_ARN};
use crate::error_contract::{call, create_api, router, Answer};

/// AppSync's own modelled error for a request that is not well formed, and its status
/// (`smithy_query(query="shape", shape="BadRequestException")`, `@httpError` 400).
const BAD_REQUEST: &str = "BadRequestException";
const BAD_REQUEST_STATUS: u16 = 400;

/// The name the service states each `DataSourceType` by in a refusal of a data source of
/// that type: the vendor prefix of its wire form dropped and each remaining word
/// capitalised, except `AMAZON_DYNAMODB`, which the service names `dynamo`. Transcribed
/// here independently of the service's own derivation, so a drift in either fails this
/// suite.
///
/// Source of the value set: `smithy_query(query="shape", shape="DataSourceType")`.
const TYPE_NAMES: &[(&str, &str)] = &[
    ("AWS_LAMBDA", "Lambda"),
    ("AMAZON_DYNAMODB", "dynamo"),
    ("AMAZON_ELASTICSEARCH", "Elasticsearch"),
    ("NONE", "None"),
    ("HTTP", "Http"),
    ("RELATIONAL_DATABASE", "RelationalDatabase"),
    ("AMAZON_OPENSEARCH_SERVICE", "OpensearchService"),
    ("AMAZON_EVENTBRIDGE", "Eventbridge"),
    ("AMAZON_BEDROCK_RUNTIME", "BedrockRuntime"),
];

/// The sentence a request carrying several configurations is refused with.
fn multiple_configs_sentence(data_source_type: &str) -> String {
    let name = TYPE_NAMES
        .iter()
        .find(|(value, _)| *value == data_source_type)
        .map_or_else(
            || panic!("case {data_source_type}: this suite carries no name for it"),
            |(_, name)| *name,
        );
    format!("Multiple configs given for data source of type {name}.")
}

/// The configuration member a `DataSourceType` is set up through, or `None` for a type
/// configured through no member at all.
fn config_member(data_source_type: &str) -> Option<String> {
    config_for(data_source_type)
        .as_object()
        .and_then(|members| members.keys().next().cloned())
}

/// A create/update body of `data_source_type` carrying the configuration fragments
/// `members` names, each taken from the shared catalog.
fn body(name: &str, data_source_type: &str, members: &[&str]) -> Value {
    let mut body = json!({
        "name": name,
        "type": data_source_type,
        "serviceRoleArn": ROLE_ARN,
    });
    let object = body.as_object_mut().expect("the body is an object");
    for member in members {
        let fragment = configuration_named(member);
        object.insert((*member).to_string(), fragment);
    }
    body
}

/// The catalog's fragment for the configuration member `member`, taken from whichever
/// type is configured through it.
fn configuration_named(member: &str) -> Value {
    for (data_source_type, _) in DATA_SOURCE_TYPES {
        let fragment = config_for(data_source_type);
        if let Some(value) = fragment.get(member) {
            return value.clone();
        }
    }
    panic!("case {member:?}: the catalog carries no fragment for it")
}

/// Send one `CreateDataSource`.
async fn create_data_source(
    router: &MultiServiceRouter,
    api_id: &str,
    body: Value,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(body),
    )
    .await
}

/// Send one `UpdateDataSource` against the data source `name`.
async fn update_data_source(
    router: &MultiServiceRouter,
    api_id: &str,
    name: &str,
    body: Value,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources/{name}"),
        Some(body),
    )
    .await
}

/// Read one data source back.
async fn get_data_source(router: &MultiServiceRouter, api_id: &str, name: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/v1/apis/{api_id}/datasources/{name}"),
        None,
    )
    .await
}

/// Create one data source the updates below are aimed at: a type configured through no
/// member, so the fixture itself carries no configuration to interfere with a row's.
async fn seed_data_source(router: &MultiServiceRouter, api_id: &str, name: &str) {
    let created = create_data_source(router, api_id, body(name, "NONE", &[])).await;
    assert_eq!(
        created.status, 200,
        "case seed the data source {name:?}: expected 200, got {created:?}"
    );
}

/// One row: the type declared, the configuration members carried, and whether the row is
/// refused as too many configurations.
struct Row {
    case: &'static str,
    data_source_type: &'static str,
    members: Vec<&'static str>,
}

/// The table: every row carries SEVERAL configurations, differing along the declared
/// type, which members they are, how many there are, and whether the declared type's own
/// member is among them.
fn multiple_config_rows() -> Vec<Row> {
    vec![
        Row {
            case: "the declared type's own configuration and one other",
            data_source_type: "AMAZON_DYNAMODB",
            members: vec!["dynamodbConfig", "lambdaConfig"],
        },
        Row {
            case: "two configurations, neither of the declared type",
            data_source_type: "AMAZON_DYNAMODB",
            members: vec!["lambdaConfig", "httpConfig"],
        },
        Row {
            case: "another type, its own configuration and one other",
            data_source_type: "AWS_LAMBDA",
            members: vec!["lambdaConfig", "eventBridgeConfig"],
        },
        Row {
            case: "a type configured through no member at all, with two configurations",
            data_source_type: "NONE",
            members: vec!["dynamodbConfig", "relationalDatabaseConfig"],
        },
        Row {
            case: "three configurations",
            data_source_type: "HTTP",
            members: vec!["httpConfig", "dynamodbConfig", "openSearchServiceConfig"],
        },
        Row {
            case: "every configuration member at once",
            data_source_type: "RELATIONAL_DATABASE",
            members: vec![
                "dynamodbConfig",
                "lambdaConfig",
                "elasticsearchConfig",
                "openSearchServiceConfig",
                "httpConfig",
                "relationalDatabaseConfig",
                "eventBridgeConfig",
            ],
        },
    ]
}

/// RULE 1, on the create: several configurations are refused as too many, naming the
/// declared type, and the refused create persists nothing.
#[tokio::test]
async fn a_create_with_several_configurations_is_refused_as_too_many() {
    let router = router();
    let api_id = create_api(&router, "Api_ds_multiple_create").await;

    for row in multiple_config_rows() {
        let name = "orders";
        let answer =
            create_data_source(&router, &api_id, body(name, row.data_source_type, &row.members))
                .await;
        let expected = multiple_configs_sentence(row.data_source_type);
        assert_eq!(
            (answer.status, answer.error_type.as_str()),
            (BAD_REQUEST_STATUS, BAD_REQUEST),
            "case {:?}: expected {BAD_REQUEST_STATUS} {BAD_REQUEST}, got {answer:?}",
            row.case
        );
        assert_eq!(
            answer.message, expected,
            "case {:?}: expected {expected:?}, got {answer:?}",
            row.case
        );
        let read = get_data_source(&router, &api_id, name).await;
        assert_eq!(
            read.status, 404,
            "case {:?}: a refused create persists nothing, got {read:?}",
            row.case
        );
    }
}

/// RULE 1, on the update: the same refusal from the other operation that carries those
/// members, and the stored data source is left as it was.
#[tokio::test]
async fn an_update_with_several_configurations_is_refused_as_too_many() {
    let router = router();
    let api_id = create_api(&router, "Api_ds_multiple_update").await;
    let name = "orders";
    seed_data_source(&router, &api_id, name).await;
    let before = get_data_source(&router, &api_id, name).await;

    for row in multiple_config_rows() {
        let answer = update_data_source(
            &router,
            &api_id,
            name,
            body(name, row.data_source_type, &row.members),
        )
        .await;
        let expected = multiple_configs_sentence(row.data_source_type);
        assert_eq!(
            (answer.status, answer.error_type.as_str()),
            (BAD_REQUEST_STATUS, BAD_REQUEST),
            "case {:?}: expected {BAD_REQUEST_STATUS} {BAD_REQUEST}, got {answer:?}",
            row.case
        );
        assert_eq!(
            answer.message, expected,
            "case {:?}: expected {expected:?}, got {answer:?}",
            row.case
        );
        let after = get_data_source(&router, &api_id, name).await;
        assert_eq!(
            after.body["dataSource"], before.body["dataSource"],
            "case {:?}: a refused update changes nothing, expected {:?}, got {after:?}",
            row.case, before.body["dataSource"]
        );
    }
}

/// RULE 2: a relational-database data source whose configuration names no source type is
/// refused, by both operations, and one that names it is served.
#[tokio::test]
async fn a_relational_database_without_a_source_type_is_refused() {
    let router = router();
    let api_id = create_api(&router, "Api_ds_relational").await;
    let name = "warehouse";

    // The configurations differ along what the relational configuration holds BESIDES
    // the source type: nothing at all, and an endpoint configuration. (An explicitly
    // EMPTY source type is outside `RelationalDatabaseSourceType`'s value set, so it is
    // refused by the request boundary before this rule is reached; that the empty value
    // is read as naming no source type is held by this service's own unit test.)
    let refused: &[(&str, Value)] = &[
        ("a configuration that holds nothing at all", json!({})),
        (
            "a configuration naming the endpoint but no source type",
            json!({
                "rdsHttpEndpointConfig": {
                    "awsRegion": "eu-west-1",
                    "dbClusterIdentifier": "orders-cluster",
                    "databaseName": "orders",
                },
            }),
        ),
    ];
    for (case, config) in refused {
        let mut request = body(name, "RELATIONAL_DATABASE", &[]);
        request
            .as_object_mut()
            .expect("the body is an object")
            .insert("relationalDatabaseConfig".to_string(), config.clone());
        let answer = create_data_source(&router, &api_id, request).await;
        assert_eq!(
            (answer.status, answer.error_type.as_str()),
            (BAD_REQUEST_STATUS, BAD_REQUEST),
            "case {case:?}: expected {BAD_REQUEST_STATUS} {BAD_REQUEST}, got {answer:?}"
        );
        assert_eq!(
            answer.message, "Missing relational database source type",
            "case {case:?}: expected the missing source-type sentence, got {answer:?}"
        );
        let read = get_data_source(&router, &api_id, name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused create persists nothing, got {read:?}"
        );
    }

    // The same request naming the source type is served and round-trips.
    let served = create_data_source(
        &router,
        &api_id,
        body(name, "RELATIONAL_DATABASE", &["relationalDatabaseConfig"]),
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case \"a relational database naming its source type\": expected 200, got {served:?}"
    );
    let read = get_data_source(&router, &api_id, name).await;
    assert_eq!(
        read.body["dataSource"]["relationalDatabaseConfig"]["relationalDatabaseSourceType"],
        json!("RDS_HTTP_ENDPOINT"),
        "case \"a relational database naming its source type\": expected the source type \
         back, got {read:?}"
    );

    // And the UPDATE path takes the same refusal, leaving the stored data source alone.
    let mut request = body(name, "RELATIONAL_DATABASE", &[]);
    request
        .as_object_mut()
        .expect("the body is an object")
        .insert("relationalDatabaseConfig".to_string(), json!({}));
    let answer = update_data_source(&router, &api_id, name, request).await;
    assert_eq!(
        (answer.status, answer.message.as_str()),
        (
            BAD_REQUEST_STATUS,
            "Missing relational database source type"
        ),
        "case \"an update leaving the source type out\": expected the same refusal, \
         got {answer:?}"
    );
    let after = get_data_source(&router, &api_id, name).await;
    assert_eq!(
        after.body["dataSource"], read.body["dataSource"],
        "case \"an update leaving the source type out\": a refused update changes nothing, \
         expected {:?}, got {after:?}",
        read.body["dataSource"]
    );
}

/// RULE 3: ONE configuration the declared type does not name is refused as too many,
/// NO configuration at all is refused as the absent one, and the declared type's own
/// configuration is served and round-trips — asserted over every `DataSourceType` the
/// model declares.
#[tokio::test]
async fn one_configuration_is_judged_against_the_declared_type() {
    let router = router();
    let api_id = create_api(&router, "Api_ds_one_config").await;

    for (data_source_type, _) in DATA_SOURCE_TYPES {
        let own = config_member(data_source_type);
        let name = format!("ds_{}", data_source_type.to_lowercase());

        // A configuration of ANOTHER type, alone: a configuration the declared type does
        // not name is an extra one however few there are, so the refusal states too many
        // — including for a type configured through no member at all, which names none.
        let foreign = if own.as_deref() == Some("lambdaConfig") {
            "dynamodbConfig"
        } else {
            "lambdaConfig"
        };
        let answer =
            create_data_source(&router, &api_id, body(&name, data_source_type, &[foreign])).await;
        assert_eq!(
            (answer.status, answer.error_type.as_str()),
            (BAD_REQUEST_STATUS, BAD_REQUEST),
            "case {data_source_type} carrying only {foreign}: expected \
             {BAD_REQUEST_STATUS} {BAD_REQUEST}, got {answer:?}"
        );
        assert_eq!(
            answer.message,
            multiple_configs_sentence(data_source_type),
            "case {data_source_type} carrying only {foreign}: expected the too-many \
             sentence, got {answer:?}"
        );

        // NO configuration at all: a type that names one is refused as the absent one,
        // and a type that names none is complete.
        let bare_name = format!("{name}_bare");
        let answer =
            create_data_source(&router, &api_id, body(&bare_name, data_source_type, &[])).await;
        match own.as_deref() {
            Some(member) => {
                assert_eq!(
                    (answer.status, answer.error_type.as_str()),
                    (BAD_REQUEST_STATUS, BAD_REQUEST),
                    "case {data_source_type} carrying no configuration: expected \
                     {BAD_REQUEST_STATUS} {BAD_REQUEST}, got {answer:?}"
                );
                assert!(
                    answer.message.starts_with("Missing "),
                    "case {data_source_type} carrying no configuration: expected the absent \
                     configuration {member:?} to be stated, got {answer:?}"
                );
                assert!(
                    !answer.message.contains("Multiple configs"),
                    "case {data_source_type} carrying no configuration: nothing given is \
                     not too many, got {answer:?}"
                );
            }
            None => assert_eq!(
                answer.status, 200,
                "case {data_source_type} carrying no configuration: a type configured \
                 through no member is complete, got {answer:?}"
            ),
        }

        // The declared type's own configuration, alone, is served and round-trips.
        let served_name = format!("{name}_served");
        let members: Vec<&str> = own.as_deref().into_iter().collect();
        let served =
            create_data_source(&router, &api_id, body(&served_name, data_source_type, &members))
                .await;
        assert_eq!(
            served.status, 200,
            "case {data_source_type} carrying its own configuration: expected 200, got \
             {served:?}"
        );
        let read = get_data_source(&router, &api_id, &served_name).await;
        assert_eq!(
            read.body["dataSource"]["type"],
            json!(data_source_type),
            "case {data_source_type} carrying its own configuration: expected the stored \
             data source back, got {read:?}"
        );
    }
}

/// RULE 4: a data source that would act with the CALLER's own credentials needs an API
/// that authenticates its callers with IAM; on an API in any other authorization mode it
/// is refused, and nothing else about the request changes that.
///
/// The rows differ along the API's authorization modes (the authorization type itself and
/// an additional provider), along whether the data source asks for the caller's
/// credentials at all, and along the operation (create and update).
#[tokio::test]
async fn caller_credentials_need_an_iam_authenticated_api() {
    let router = router();

    // (case, the API's authenticationType, its additional providers, whether a data
    // source of that API may act as the caller)
    let apis: &[(&str, &str, Value, bool)] = &[
        ("an API-key API", "API_KEY", json!([]), false),
        (
            "an API-key API whose additional provider is not IAM either",
            "API_KEY",
            json!([{
                "authenticationType": "OPENID_CONNECT",
                "openIDConnectConfig": {"issuer": "https://issuer.example.com"},
            }]),
            false,
        ),
        ("an IAM-authenticated API", "AWS_IAM", json!([]), true),
        (
            "an API-key API that also set up IAM as an additional provider",
            "API_KEY",
            json!([{"authenticationType": "AWS_IAM"}]),
            true,
        ),
    ];

    for (index, (case, authentication_type, providers, permitted)) in apis.iter().enumerate() {
        let api_id = api_with_authorization(&router, index, authentication_type, providers).await;

        // A DynamoDB data source asking for the caller's credentials.
        let name = "orders";
        let mut request = body(name, "AMAZON_DYNAMODB", &["dynamodbConfig"]);
        request["dynamodbConfig"]["useCallerCredentials"] = json!(true);
        let answer = create_data_source(&router, &api_id, request.clone()).await;
        if *permitted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the create to be served, got {answer:?}"
            );
            let read = get_data_source(&router, &api_id, name).await;
            assert_eq!(
                read.body["dataSource"]["dynamodbConfig"]["useCallerCredentials"],
                json!(true),
                "case {case:?}: expected the setting back, got {read:?}"
            );
            // The update path serves it too.
            let updated = update_data_source(&router, &api_id, name, request).await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: expected the update to be served, got {updated:?}"
            );
            continue;
        }
        assert_eq!(
            (answer.status, answer.error_type.as_str()),
            (BAD_REQUEST_STATUS, BAD_REQUEST),
            "case {case:?}: expected {BAD_REQUEST_STATUS} {BAD_REQUEST}, got {answer:?}"
        );
        assert_eq!(
            answer.message, "Caller credentials can only be used with AWS IAM authenticated API",
            "case {case:?}: expected the caller-credentials sentence, got {answer:?}"
        );
        let read = get_data_source(&router, &api_id, name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused create persists nothing, got {read:?}"
        );

        // The same data source WITHOUT the setting is served on the very same API, so the
        // refusal is about the caller's credentials rather than about the data source.
        let served = create_data_source(
            &router,
            &api_id,
            body(name, "AMAZON_DYNAMODB", &["dynamodbConfig"]),
        )
        .await;
        assert_eq!(
            served.status, 200,
            "case {case:?} without the setting: expected 200, got {served:?}"
        );

        // And an update that would turn it on is refused, leaving the data source as it
        // was.
        let before = get_data_source(&router, &api_id, name).await;
        let mut request = body(name, "AMAZON_DYNAMODB", &["dynamodbConfig"]);
        request["dynamodbConfig"]["useCallerCredentials"] = json!(true);
        let updated = update_data_source(&router, &api_id, name, request).await;
        assert_eq!(
            (updated.status, updated.message.as_str()),
            (
                BAD_REQUEST_STATUS,
                "Caller credentials can only be used with AWS IAM authenticated API"
            ),
            "case {case:?}: expected the update to take the same refusal, got {updated:?}"
        );
        let after = get_data_source(&router, &api_id, name).await;
        assert_eq!(
            after.body["dataSource"], before.body["dataSource"],
            "case {case:?}: a refused update changes nothing, expected {:?}, got {after:?}",
            before.body["dataSource"]
        );
    }
}

/// Create one GraphQL API with the authorization modes a row names, and answer its
/// `apiId`.
async fn api_with_authorization(
    router: &MultiServiceRouter,
    index: usize,
    authentication_type: &str,
    providers: &Value,
) -> String {
    let mut request = json!({
        "name": format!("Api_caller_credentials_{index}"),
        "authenticationType": authentication_type,
    });
    if providers.as_array().is_some_and(|list| !list.is_empty()) {
        request["additionalAuthenticationProviders"] = providers.clone();
    }
    let created = call(router, "POST", "/v1/apis", Some(request)).await;
    assert_eq!(
        created.status, 200,
        "case seed an API with {authentication_type}: expected 200, got {created:?}"
    );
    created.body["graphqlApi"]["apiId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

