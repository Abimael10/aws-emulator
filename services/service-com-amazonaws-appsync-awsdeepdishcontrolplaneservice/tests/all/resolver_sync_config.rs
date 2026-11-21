// SPDX-License-Identifier: CC0-1.0

//! The sync configuration a resolver or a function may be written with, end to end over
//! HTTP: what conflict resolution has to name, and what a refused configuration leaves
//! behind.
//!
//! Three rules are asserted here, each over a TABLE that spans the class it generalises
//! over rather than the one request that exposed the defect:
//!
//! 1. only one conflict handler is configured by a member beside it — "`LAMBDA`: Resolve
//!    conflicts with an Lambda function supplied in the `LambdaConflictHandlerConfig`"
//!    (`SyncConfig$conflictHandler`) — so a configuration naming that handler and
//!    supplying no `lambdaConflictHandlerConfig` is refused 400 `BadRequestException`
//!    with AppSync's fixed sentence for the absent member, while every other handler is
//!    served without one;
//! 2. a `lambdaConflictHandlerConfig` that IS supplied names "the Lambda function to use
//!    as the Conflict Handler" (`LambdaConflictHandlerConfig$lambdaConflictHandlerArn`) of
//!    the calling account, so a value that is no Lambda function ARN of this account is
//!    refused 400 with AppSync's fixed sentence for that member;
//! 3. a RESOLVER's configuration has something to detect a conflict on: `syncConfig` is
//!    "The `SyncConfig` for a resolver attached to a versioned data source"
//!    (`CreateResolverRequest$syncConfig`), and a data source is configured for versioned
//!    items by its DynamoDB settings ("Set to TRUE to use Conflict Detection and
//!    Resolution with this data source", `DynamodbDataSourceConfig$versioned`), so a
//!    resolver configuring conflict resolution against any other data source — or against
//!    none at all — is refused 400 with AppSync's sentence for that pairing.
//!
//! Rules 1 and 2 are over the members of `SyncConfig`, so they are asserted on a call that
//! writes a RESOLVER (create and update both) and on one that writes a FUNCTION; rule 3 is
//! the resolver pairing AppSync's own sentence states, so it is asserted over the data
//! source forms a resolver can resolve against. A refused create leaves the field it named
//! carrying no resolver and a refused update leaves the resolver already there exactly as
//! it was, which the walks below assert for every row.
//!
//! The rows differ along what the rules generalise over: every `ConflictHandlerType` value
//! the model lists, both `ConflictDetectionType` values, a
//! configuration carrying the Lambda member and one leaving it out, and — for the ARN —
//! an absent member, an empty value, a bare name, another account's function, another
//! service's resource and another resource of Lambda's own. Every assertion goes through
//! the router the production emulator mounts; the API harness is the resolver suites' own,
//! reused rather than copied.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::error_contract::{call, router, Answer};
use super::functions::{create_data_source, ACCOUNT, REGION};
use super::resolvers::{
    api_with_dependencies, create_resolver_body, get_resolver, DATA_SOURCE, TYPE,
};

/// AppSync's sentence for a configuration that resolves conflicts with Lambda and supplies
/// no Lambda configuration.
///
/// Written out here rather than read off the service's own constant, so a change of
/// wording is caught rather than followed. The same holds for the two below.
const LAMBDA_CONFIG_MISSING: &str =
    "Lambda config must not be null when configuring Lambda as the conflict handler.";

/// AppSync's sentence for a Lambda configuration that names no function of the calling
/// account.
const INVALID_LAMBDA_ARN: &str =
    "LambdaConflictHandlerArn is not a valid Lambda Arn in the calling account.";

/// AppSync's sentence for a resolver that configures conflict resolution against a data
/// source that keeps no versioned items.
const UNVERSIONED_DATA_SOURCE: &str =
    "Data source must be configured for versioned items when specifying SyncConfig on the \
     resolver.";

/// AppSync's fixed text for a field that carries no resolver.
const NO_RESOLVER: &str = "No resolver found.";

/// The mapping template every row carries, so no row's refusal can come from a definition
/// member it left out.
const REQUEST_TEMPLATE: &str = "{\"version\": \"2018-05-29\"}";

/// One row of the sync table: the `syncConfig` a request carries, and the sentence AppSync
/// refuses it with, where it refuses it at all.
#[derive(Debug)]
struct Row {
    /// What the row exercises, named in every assertion of it.
    label: &'static str,
    /// The `syncConfig` the request carries, as the body member itself, so a row can carry
    /// a configuration with any member set or left out.
    sync_config: fn() -> Value,
    /// The refusal text the configuration is owed, or `None` where AppSync serves it.
    refusal: Option<&'static str>,
}

/// The ARN of a Lambda function of the account this emulator serves, written the way the
/// sibling suites write one.
fn own_function(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// The sync table, over the handlers a configuration may name and the functions it may
/// supply.
///
/// Source for the value sets: `smithy_query(query="shape",
/// shape="SyncConfig,ConflictHandlerType,ConflictDetectionType,
/// LambdaConflictHandlerConfig")` — `ConflictHandlerType` is
/// `OPTIMISTIC_CONCURRENCY | LAMBDA | AUTOMERGE | NONE` and `ConflictDetectionType` is
/// `VERSION | NONE`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "version detection resolved by optimistic concurrency",
            sync_config: || {
                json!({"conflictDetection": "VERSION", "conflictHandler": "OPTIMISTIC_CONCURRENCY"})
            },
            refusal: None,
        },
        Row {
            label: "version detection resolved by automerge",
            sync_config: || json!({"conflictDetection": "VERSION", "conflictHandler": "AUTOMERGE"}),
            refusal: None,
        },
        Row {
            label: "no detection and no resolution",
            sync_config: || json!({"conflictDetection": "NONE", "conflictHandler": "NONE"}),
            refusal: None,
        },
        Row {
            label: "a configuration naming no handler at all",
            sync_config: || json!({"conflictDetection": "VERSION"}),
            refusal: None,
        },
        Row {
            label: "the lambda handler supplying its function",
            sync_config: || {
                json!({
                    "conflictDetection": "VERSION",
                    "conflictHandler": "LAMBDA",
                    "lambdaConflictHandlerConfig": {
                        "lambdaConflictHandlerArn": own_function("resolve_conflicts"),
                    },
                })
            },
            refusal: None,
        },
        Row {
            label: "the lambda handler supplying a qualified function",
            sync_config: || {
                json!({
                    "conflictDetection": "NONE",
                    "conflictHandler": "LAMBDA",
                    "lambdaConflictHandlerConfig": {
                        "lambdaConflictHandlerArn": own_function("resolve_conflicts:live"),
                    },
                })
            },
            refusal: None,
        },
        Row {
            label: "a function supplied beside another handler",
            sync_config: || {
                json!({
                    "conflictDetection": "VERSION",
                    "conflictHandler": "AUTOMERGE",
                    "lambdaConflictHandlerConfig": {
                        "lambdaConflictHandlerArn": own_function("resolve_conflicts"),
                    },
                })
            },
            refusal: None,
        },
        Row {
            label: "the lambda handler supplying no configuration",
            sync_config: || json!({"conflictDetection": "VERSION", "conflictHandler": "LAMBDA"}),
            refusal: Some(LAMBDA_CONFIG_MISSING),
        },
        Row {
            label: "a configuration naming no function",
            sync_config: || {
                json!({
                    "conflictDetection": "VERSION",
                    "conflictHandler": "LAMBDA",
                    "lambdaConflictHandlerConfig": {},
                })
            },
            refusal: Some(INVALID_LAMBDA_ARN),
        },
        Row {
            label: "an empty function ARN",
            sync_config: || {
                json!({
                    "conflictDetection": "VERSION",
                    "conflictHandler": "LAMBDA",
                    "lambdaConflictHandlerConfig": {"lambdaConflictHandlerArn": ""},
                })
            },
            refusal: Some(INVALID_LAMBDA_ARN),
        },
        Row {
            label: "a bare function name",
            sync_config: || {
                json!({
                    "conflictDetection": "VERSION",
                    "conflictHandler": "LAMBDA",
                    "lambdaConflictHandlerConfig": {
                        "lambdaConflictHandlerArn": "resolve_conflicts",
                    },
                })
            },
            refusal: Some(INVALID_LAMBDA_ARN),
        },
        Row {
            label: "a function of another account",
            sync_config: || {
                json!({
                    "conflictDetection": "VERSION",
                    "conflictHandler": "LAMBDA",
                    "lambdaConflictHandlerConfig": {
                        "lambdaConflictHandlerArn":
                            format!("arn:aws:lambda:{REGION}:210987654321:function:resolve"),
                    },
                })
            },
            refusal: Some(INVALID_LAMBDA_ARN),
        },
        Row {
            label: "a resource of another service",
            sync_config: || {
                json!({
                    "conflictDetection": "VERSION",
                    "conflictHandler": "LAMBDA",
                    "lambdaConflictHandlerConfig": {
                        "lambdaConflictHandlerArn":
                            format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:resolve"),
                    },
                })
            },
            refusal: Some(INVALID_LAMBDA_ARN),
        },
        Row {
            label: "another resource of Lambda's own",
            sync_config: || {
                json!({
                    "conflictDetection": "VERSION",
                    "conflictHandler": "LAMBDA",
                    "lambdaConflictHandlerConfig": {
                        "lambdaConflictHandlerArn":
                            format!("arn:aws:lambda:{REGION}:{ACCOUNT}:layer:resolve"),
                    },
                })
            },
            refusal: Some(INVALID_LAMBDA_ARN),
        },
        Row {
            label: "a configuration supplied beside another handler naming no function",
            sync_config: || {
                json!({
                    "conflictDetection": "VERSION",
                    "conflictHandler": "AUTOMERGE",
                    "lambdaConflictHandlerConfig": {"lambdaConflictHandlerArn": "resolve"},
                })
            },
            refusal: Some(INVALID_LAMBDA_ARN),
        },
    ]
}

/// The configuration a resolver an update addresses is created with: one AppSync serves.
fn settled_sync_config() -> Value {
    json!({"conflictDetection": "VERSION", "conflictHandler": "OPTIMISTIC_CONCURRENCY"})
}

/// A UNIT resolver on `field` against `data_source`, configured as `sync_config` asks.
fn resolver_body(field: &str, data_source: &str, sync_config: Value) -> Value {
    json!({
        "fieldName": field,
        "dataSourceName": data_source,
        "requestMappingTemplate": REQUEST_TEMPLATE,
        "syncConfig": sync_config,
    })
}

/// Send one `UpdateResolver` restating the resolver on `field` with `sync_config`.
async fn update_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    field: &str,
    sync_config: Value,
) -> Answer {
    let mut body = resolver_body(field, DATA_SOURCE, sync_config);
    body.as_object_mut()
        .expect("the request body is a JSON object")
        .remove("fieldName");
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers/{field}"),
        Some(body),
    )
    .await
}

/// Assert `answer` is the refusal `message` states.
fn assert_refused(case: &str, answer: &Answer, message: &str) {
    assert_eq!(
        (
            answer.status,
            answer.error_type.as_str(),
            answer.message.as_str()
        ),
        (400, "BadRequestException", message),
        "case {case}: expected 400 BadRequestException {message:?}, got {answer:?}"
    );
}

/// Assert `answer` is the 404 a field carrying no resolver is owed.
fn assert_no_resolver(case: &str, answer: &Answer) {
    assert_eq!(
        (
            answer.status,
            answer.error_type.as_str(),
            answer.message.as_str()
        ),
        (404, "NotFoundException", NO_RESOLVER),
        "case {case}: expected 404 NotFoundException {NO_RESOLVER:?}, got {answer:?}"
    );
}

#[tokio::test]
async fn a_create_writes_a_resolver_only_for_a_configured_conflict_handler() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_sync_config_create").await;

    for (index, row) in rows().iter().enumerate() {
        let field = format!("sync_created_{index}");
        let sync_config = (row.sync_config)();

        // Nothing is attached yet, so the field carries no resolver whatever the row asks
        // for.
        let before = get_resolver(&router, &api_id, TYPE, &field).await;
        assert_no_resolver(&format!("before create {}", row.label), &before);

        let created = create_resolver_body(
            &router,
            &api_id,
            TYPE,
            resolver_body(&field, DATA_SOURCE, sync_config.clone()),
        )
        .await;

        if let Some(message) = row.refusal {
            assert_refused(&format!("create {}", row.label), &created, message);

            // The refusal wrote nothing, so the field it named still carries no resolver.
            let read = get_resolver(&router, &api_id, TYPE, &field).await;
            assert_no_resolver(&format!("after refused create {}", row.label), &read);
            continue;
        }

        assert_eq!(
            created.status, 200,
            "case create {}: expected 200, got {created:?}",
            row.label
        );
        assert_eq!(
            created.body["resolver"]["syncConfig"], sync_config,
            "case create {}: expected the sync configuration the request set {sync_config:?}, \
             got {:?}",
            row.label, created.body["resolver"]["syncConfig"]
        );

        // The stored record an independent read answers is that same body.
        let read = get_resolver(&router, &api_id, TYPE, &field).await;
        assert_eq!(
            read.body["resolver"], created.body["resolver"],
            "case read back {}: expected the stored record {:?}, got {:?}",
            row.label, created.body["resolver"], read.body["resolver"]
        );
    }
}

#[tokio::test]
async fn an_update_is_judged_by_the_same_rules_and_a_refusal_keeps_the_stored_resolver() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_sync_config_update").await;

    for (index, row) in rows().iter().enumerate() {
        let field = format!("sync_updated_{index}");
        let created = create_resolver_body(
            &router,
            &api_id,
            TYPE,
            resolver_body(&field, DATA_SOURCE, settled_sync_config()),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case update {}: expected the resolver to attach first, got {created:?}",
            row.label
        );
        let stored = created.body["resolver"].clone();

        let sync_config = (row.sync_config)();
        let updated = update_resolver(&router, &api_id, &field, sync_config.clone()).await;

        if let Some(message) = row.refusal {
            assert_refused(&format!("update {}", row.label), &updated, message);
            let read = get_resolver(&router, &api_id, TYPE, &field).await;
            assert_eq!(
                read.body["resolver"], stored,
                "case update {}: expected the stored resolver untouched {stored:?}, got {:?}",
                row.label, read.body["resolver"]
            );
            continue;
        }

        assert_eq!(
            updated.status, 200,
            "case update {}: expected 200, got {updated:?}",
            row.label
        );
        let read = get_resolver(&router, &api_id, TYPE, &field).await;
        assert_eq!(
            read.body["resolver"]["syncConfig"], sync_config,
            "case update {}: expected the sync configuration the update set {sync_config:?}, \
             got {:?}",
            row.label, read.body["resolver"]["syncConfig"]
        );
    }
}

/// The name of a DynamoDB data source that keeps UNVERSIONED items, and the table and role
/// it is created with.
const UNVERSIONED_DYNAMO: &str = "ds_sync_unversioned";
const UNVERSIONED_TABLE: &str = "sync_items";
const UNVERSIONED_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/appsync-sync";

/// The name of a data source of a type that keeps no items at all.
const ITEMLESS_DATA_SOURCE: &str = "ds_sync_none";

#[tokio::test]
async fn a_resolver_configures_conflict_resolution_only_against_versioned_items() {
    let router = router();
    let (api_id, function_id) = api_with_dependencies(&router, "Api_sync_config_versioned").await;

    // Beside the versioned data source the API is seeded with, one DynamoDB data source
    // that does NOT keep versioned items and one of a type that keeps no items at all, so
    // the rule is read on the `versioned` member rather than on the data source's type.
    let created = call(
        &router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({
            "name": UNVERSIONED_DYNAMO,
            "type": "AMAZON_DYNAMODB",
            "serviceRoleArn": UNVERSIONED_ROLE,
            "dynamodbConfig": {"tableName": UNVERSIONED_TABLE, "awsRegion": REGION},
        })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case create the unversioned data source: expected 200, got {created:?}"
    );
    create_data_source(&router, &api_id, ITEMLESS_DATA_SOURCE).await;

    // (case, the data source the resolver resolves against, the refusal it is owed)
    let cases: &[(&str, Option<&str>, Option<&str>)] = &[
        ("a data source configured for versioned items", Some(DATA_SOURCE), None),
        (
            "a DynamoDB data source that keeps unversioned items",
            Some(UNVERSIONED_DYNAMO),
            Some(UNVERSIONED_DATA_SOURCE),
        ),
        (
            "a data source that keeps no items at all",
            Some(ITEMLESS_DATA_SOURCE),
            Some(UNVERSIONED_DATA_SOURCE),
        ),
        // A pipeline resolver resolves through its functions, so it names no data source
        // whose items could carry a version.
        ("a resolver naming no data source", None, Some(UNVERSIONED_DATA_SOURCE)),
    ];

    for (index, (case, data_source, refusal)) in cases.iter().enumerate() {
        let field = format!("sync_versioned_{index}");
        let body = match data_source {
            Some(name) => resolver_body(&field, name, settled_sync_config()),
            None => json!({
                "fieldName": field,
                "kind": "PIPELINE",
                "pipelineConfig": {"functions": [function_id]},
                "requestMappingTemplate": REQUEST_TEMPLATE,
                "syncConfig": settled_sync_config(),
            }),
        };
        let created = create_resolver_body(&router, &api_id, TYPE, body).await;

        if let Some(message) = refusal {
            assert_refused(case, &created, message);
            let read = get_resolver(&router, &api_id, TYPE, &field).await;
            assert_no_resolver(&format!("after refused create against {case}"), &read);
            continue;
        }

        assert_eq!(
            created.status, 200,
            "case {case}: expected 200, got {created:?}"
        );
        let read = get_resolver(&router, &api_id, TYPE, &field).await;
        assert_eq!(
            read.body["resolver"]["syncConfig"],
            settled_sync_config(),
            "case {case}: expected the stored sync configuration, got {:?}",
            read.body["resolver"]["syncConfig"]
        );
    }
}

#[tokio::test]
async fn a_function_is_written_only_for_a_configured_conflict_handler() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_sync_config_function").await;

    // The same member rules, on the call that writes a FUNCTION: the pairing rules above
    // are about the members of `SyncConfig`, not about one operation.
    for (index, row) in rows().iter().enumerate() {
        let name = format!("fn_sync_{index}");
        let sync_config = (row.sync_config)();
        let created = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}/functions"),
            Some(json!({
                "name": name,
                "dataSourceName": DATA_SOURCE,
                "functionVersion": "2018-05-29",
                "requestMappingTemplate": REQUEST_TEMPLATE,
                "syncConfig": sync_config,
            })),
        )
        .await;

        if let Some(message) = row.refusal {
            assert_refused(&format!("create function {}", row.label), &created, message);
            continue;
        }

        assert_eq!(
            created.status, 200,
            "case create function {}: expected 200, got {created:?}",
            row.label
        );
        let function_id = created.body["functionConfiguration"]["functionId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case create function {}: expected a minted functionId, got {created:?}",
                    row.label
                )
            })
            .to_string();

        // The stored record an independent read answers carries the configuration the
        // request set.
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/functions/{function_id}"),
            None,
        )
        .await;
        assert_eq!(
            read.body["functionConfiguration"]["syncConfig"], sync_config,
            "case read back function {}: expected {sync_config:?}, got {:?}",
            row.label, read.body["functionConfiguration"]["syncConfig"]
        );
    }
}
