// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose resolvers write through a VERSIONED `AMAZON_DYNAMODB` data
//! source — end to end over the real `MultiServiceRouter`, with AppSync, DynamoDB, Lambda, IAM
//! and CloudFormation mounted on one engine.
//!
//! RULE — a write made through a data source configured for versioned items ("set to TRUE to
//! use Conflict Detection and Resolution with this data source",
//! `DynamodbDataSourceConfig$versioned`) MAINTAINS the two system attributes AppSync owns on the
//! item: `_version`, which is 1 on the write that creates the item and one more on every write
//! accepted after that, and `_lastChangedAt`, the write's own time in epoch milliseconds. Both
//! are written into the item the TABLE holds, and a delete leaves the item behind marked
//! `_deleted` with its version advanced rather than removing it. Where the resolver's
//! `syncConfig` asks for version-based detection (`SyncConfig$conflictDetection` `VERSION`), the
//! version the request object states has to equal the stored one for the write to be applied,
//! and a write that states another version is a CONFLICT whose outcome is the
//! `SyncConfig$conflictHandler`: `OPTIMISTIC_CONCURRENCY` — and `NONE`, and no handler at all —
//! writes nothing and reports a `ConflictUnhandled` error carrying the row as the server holds
//! it; `AUTOMERGE` merges the write into that row and writes the merged row as the next version;
//! `LAMBDA` invokes the configured function with the two rows and writes the row it resolved
//! with, or reports the conflict when it rejects the write.
//!
//! Five services have to agree for that to hold — DynamoDB holds the rows, Lambda runs the
//! conflict handler, CloudFormation archives that handler into its deployment package, IAM holds
//! its execution role, and AppSync publishes the endpoint, runs the resolver and decides the
//! conflict — so this is the suite that can pin it. Every write a case makes through the
//! endpoint is read back through DynamoDB's OWN api, so what a field reports is what that
//! service holds.
//!
//! The rows span what the rule generalises over: the operation the request object names
//! (`PutItem`, `UpdateItem`, `DeleteItem`), whether the version it states matches the stored one
//! and whether it states one at all, the condition the request itself states for the write, the
//! handler the resolver configures (none, `NONE`, `OPTIMISTIC_CONCURRENCY`, `AUTOMERGE`,
//! `LAMBDA`), and what the invoked handler decided (a rejection and a resolution).

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_lambda_data_source_and_authorizer::{
    appsync_ok, build_router, create_function, post_published_uri,
};

/// The Region every request here signs for, which is the one that target's control-plane
/// helpers address.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which every setup call is signed with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The table every resolver of this suite writes, and its one key attribute.
const TABLE: &str = "appsync-versioned-posts";
const KEY_ATTRIBUTE: &str = "id";

/// The versioned data source every resolver resolves against, and the retention window its
/// delta-sync configuration states in minutes ("the number of minutes that an Item is stored in
/// the data source", `DeltaSyncConfig$baseTableTTL`).
const DATA_SOURCE: &str = "versionedPosts";
const BASE_TABLE_TTL_MINUTES: i64 = 1_440;
const DELTA_SYNC_TABLE: &str = "appsync-versioned-posts-delta";

/// The `version` every request object of this suite states, as the AppSync DynamoDB resolver
/// reference writes it.
const REQUEST_VERSION: &str = "2018-05-29";

/// The system attributes AppSync owns on an item of a versioned data source.
const VERSION_ATTRIBUTE: &str = "_version";
const LAST_CHANGED_AT_ATTRIBUTE: &str = "_lastChangedAt";
const DELETED_ATTRIBUTE: &str = "_deleted";
const TTL_ATTRIBUTE: &str = "_ttl";

/// The error class and the sentence AppSync reports a conflict that was not resolved with.
const CONFLICT_ERROR_TYPE: &str = "ConflictUnhandled";
const CONFLICT_MESSAGE: &str = "Conflict resolver rejects mutation.";

/// How long a poll loop waits for a state another service owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches a
/// transition.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The version a stale write states: one no row of this suite ever reaches, so the mismatch is
/// the only reason such a write is a conflict.
const STALE_VERSION: i64 = 99;

/// The schema every API of this suite is created with: the row the table holds, with the system
/// attributes a client reads to drive its next write, and one mutation per handler a row drives.
const SCHEMA: &str = "\
type Post {\n  id: String\n  title: String\n  tags: [String]\n  resolvedBy: String\n  \
  _version: Int\n  _lastChangedAt: Float\n  _deleted: Boolean\n}\n\
\n\
type Query {\n  post(id: String): Post\n}\n\
\n\
type Mutation {\n  \
  createPost(id: String, title: String, tags: [String]): Post\n  \
  createIfAbsent(id: String, title: String, tags: [String]): Post\n  \
  retitle(id: String, title: String, tags: [String], expected: Int): Post\n  \
  retitleUnstated(id: String, title: String, tags: [String]): Post\n  \
  retitleUnhandled(id: String, title: String, tags: [String], expected: Int): Post\n  \
  retitleOptimistic(id: String, title: String, tags: [String], expected: Int): Post\n  \
  retitleAutomerge(id: String, title: String, tags: [String], expected: Int): Post\n  \
  retitleLambda(id: String, title: String, tags: [String], expected: Int): Post\n  \
  removePost(id: String, expected: Int): Post\n}\n";

/// The conflict handler this suite's `LAMBDA` resolver names: it RESOLVES a conflict whose
/// incoming title asks for one — composing the resolved row from the row the server holds, the
/// title the write wanted and the resolver the write was of — and REJECTS every other one.
const CONFLICT_HANDLER: &str = r#"
def handler(event, context):
    new_item = event.get("newItem") or {}
    existing = event.get("existingItem") or {}
    resolver = event.get("resolver") or {}
    title = str(new_item.get("title") or "")
    if not title.startswith("resolve"):
        return {"action": "REJECT"}
    resolved = dict(existing)
    resolved["title"] = title
    resolved["resolvedBy"] = "{}.{}".format(
        resolver.get("parentType"), resolver.get("field")
    )
    resolved["seenVersion"] = existing.get("_version")
    return {"action": "RESOLVE", "item": resolved}
"#;

/// A SigV4 `Authorization` header whose credential is the ambient key. The signature is never
/// checked in this target.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One Amazon DynamoDB call over its own `awsJson1_0` wire, asserting the call was served.
async fn dynamodb_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization("dynamodb"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "setup: DynamoDB {operation} expected 200, got {status} {parsed}"
    );
    parsed
}

/// Create the table every row reads or writes and wait until DynamoDB first reports it `ACTIVE`
/// — a table that is not is one no call lands on.
async fn create_table(router: &MultiServiceRouter) {
    for name in [TABLE, DELTA_SYNC_TABLE] {
        dynamodb_ok(
            router,
            "CreateTable",
            json!({
                "TableName": name,
                "KeySchema": [{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}],
                "AttributeDefinitions": [
                    {"AttributeName": KEY_ATTRIBUTE, "AttributeType": "S"},
                ],
                "BillingMode": "PAY_PER_REQUEST",
            }),
        )
        .await;
    }
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = dynamodb_ok(router, "DescribeTable", json!({"TableName": TABLE})).await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the table must become ACTIVE, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What the table holds for `id`, read through DynamoDB's OWN api — so it says what that service
/// holds rather than what the endpoint answered. `None` for a key holding no row.
async fn held(router: &MultiServiceRouter, id: &str) -> Option<Value> {
    let read = dynamodb_ok(
        router,
        "GetItem",
        json!({
            "TableName": TABLE,
            "Key": {KEY_ATTRIBUTE: {"S": id}},
            "ConsistentRead": true,
        }),
    )
    .await;
    read.get("Item").filter(|item| !item.is_null()).cloned()
}

/// One created GraphQL API: where it is served, and the key a client presents.
struct Api {
    graphql_uri: String,
    api_key: String,
}

/// Create the API with an API key, install [`SCHEMA`], attach the VERSIONED data source and one
/// resolver per mutation — each with the sync configuration its own case drives.
async fn create_api(router: &MultiServiceRouter, name: &str, handler_arn: &str) -> Api {
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": name, "authenticationType": "API_KEY"})),
    )
    .await;
    let api = &created["graphqlApi"];
    let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
    let graphql_uri = api["uris"]["GRAPHQL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        graphql_uri.contains("127.0.0.1:"),
        "setup: uris.GRAPHQL must be a URL this emulator serves, got {graphql_uri:?}"
    );
    let key = appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/apikeys"),
        Some(json!({"description": "the suite's key"})),
    )
    .await;
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/schemacreation"),
        Some(json!({"definition": ::aws_smithy_types::base64::encode(SCHEMA)})),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let settled =
            appsync_ok(router, "GET", &format!("/v1/apis/{api_id}/schemacreation"), None).await;
        if settled["status"].as_str() == Some("SUCCESS") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the schema creation must be performed, last read {settled}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({
            "name": DATA_SOURCE,
            "type": "AMAZON_DYNAMODB",
            // The role "the system assumes when accessing the data source", as a deployed app
            // names one. IAM holds no policy for it here, which says nothing against the call.
            "serviceRoleArn": format!(
                "arn:aws:iam::{}:role/appsync-versioned-datasource",
                engine::EMULATOR_ACCOUNT_ID
            ),
            "dynamodbConfig": {
                "tableName": TABLE,
                "awsRegion": REGION,
                "versioned": true,
                "deltaSyncConfig": {
                    "baseTableTTL": BASE_TABLE_TTL_MINUTES,
                    "deltaSyncTableName": DELTA_SYNC_TABLE,
                    "deltaSyncTableTTL": 60,
                },
            },
        })),
    )
    .await;
    for (field, request, sync) in resolvers(handler_arn) {
        let mut body = json!({
            "fieldName": field,
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "requestMappingTemplate": request,
            "responseMappingTemplate": RESPONSE_TEMPLATE,
        });
        if let Some(sync) = sync {
            body["syncConfig"] = sync;
        }
        appsync_ok(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Mutation/resolvers"),
            Some(body),
        )
        .await;
    }
    Api {
        graphql_uri,
        api_key: key["apiKey"]["id"].as_str().unwrap_or_default().to_string(),
    }
}

/// Every resolver of this suite: the field, its request template, and the sync configuration it
/// is created with — one per handler the rule names, plus the create and the delete.
fn resolvers(handler_arn: &str) -> Vec<(&'static str, String, Option<Value>)> {
    let detection = json!("VERSION");
    let configured = |handler: &str| {
        Some(json!({
            "conflictDetection": detection.clone(),
            "conflictHandler": handler,
        }))
    };
    vec![
        (
            "createPost",
            put_template(None),
            Some(json!({
                "conflictDetection": detection.clone(),
                "conflictHandler": "OPTIMISTIC_CONCURRENCY",
            })),
        ),
        // The same create under a CONDITION of its own: the write is the caller's to state the
        // grounds of, and a versioned write is refused on exactly those grounds.
        (
            "createIfAbsent",
            put_template(Some(ABSENT_CONDITION)),
            configured("OPTIMISTIC_CONCURRENCY"),
        ),
        ("retitle", update_template(true), None),
        // An update whose request object states NO version at all, under version detection: it
        // claims nothing about the version it read, so there is nothing to detect a conflict
        // against.
        (
            "retitleUnstated",
            update_template(false),
            configured("OPTIMISTIC_CONCURRENCY"),
        ),
        (
            "retitleUnhandled",
            update_template(true),
            configured("NONE"),
        ),
        (
            "retitleOptimistic",
            update_template(true),
            configured("OPTIMISTIC_CONCURRENCY"),
        ),
        (
            "retitleAutomerge",
            update_template(true),
            configured("AUTOMERGE"),
        ),
        (
            "retitleLambda",
            update_template(true),
            Some(json!({
                "conflictDetection": detection.clone(),
                "conflictHandler": "LAMBDA",
                "lambdaConflictHandlerConfig": {"lambdaConflictHandlerArn": handler_arn},
            })),
        ),
        (
            "removePost",
            delete_template(),
            configured("OPTIMISTIC_CONCURRENCY"),
        ),
    ]
}

/// The condition `createIfAbsent` states: the row it writes must not be there, which is how a
/// create that must not overwrite is written.
const ABSENT_CONDITION: &str = "attribute_not_exists(id)";

/// The `errorType` AppSync reports for a write the TABLE refused: the service the call was made
/// to, and the class of the exception its client raised.
const CONDITION_ERROR_TYPE: &str = "DynamoDB:ConditionalCheckFailedException";

/// The response template every resolver carries: a conflict the endpoint did not resolve is
/// reported as the field's own error, carrying the class it is of and the row the endpoint handed
/// the half as its result — the handling the AppSync conflict-detection reference writes.
const RESPONSE_TEMPLATE: &str = "#if($ctx.error)\n  \
                                 $util.error($ctx.error.message, $ctx.error.type, $ctx.result)\n\
                                 #end\n$util.toJson($ctx.result)";

/// The `PutItem` request object a create renders: the row's key, the values the call carries, the
/// version the caller believes is current — 1, which is the version a create states — and the
/// `condition` the write is to be refused on, where the case states one.
fn put_template(condition: Option<&str>) -> String {
    let stated = condition.map_or_else(String::new, |expression| {
        format!("  \"condition\": {{\"expression\": \"{expression}\"}},\n  ")
    });
    format!(
        "{{\n  \"version\": \"{REQUEST_VERSION}\",\n  \"operation\": \"PutItem\",\n  \
         \"key\": {{\"{KEY_ATTRIBUTE}\": $util.dynamodb.toDynamoDBJson($ctx.args.id)}},\n  \
         \"attributeValues\": {{\n    \
         \"title\": $util.dynamodb.toDynamoDBJson($ctx.args.title),\n    \
         \"tags\": $util.dynamodb.toDynamoDBJson($ctx.args.tags)\n  }},\n{stated}  \
         \"{VERSION_ATTRIBUTE}\": 1\n}}"
    )
}

/// The `UpdateItem` request object every retitle renders: the expression that writes the title
/// and the tags, the names and values it is parameterised by — the `#name` of an expression
/// attribute name among them — and, where the case states one, the version the caller expects.
fn update_template(states_version: bool) -> String {
    let stated = if states_version {
        format!(",\n  \"{VERSION_ATTRIBUTE}\": $ctx.args.expected")
    } else {
        String::new()
    };
    format!(
        "{{\n  \"version\": \"{REQUEST_VERSION}\",\n  \"operation\": \"UpdateItem\",\n  \
         \"key\": {{\"{KEY_ATTRIBUTE}\": $util.dynamodb.toDynamoDBJson($ctx.args.id)}},\n  \
         \"update\": {{\n    \
         \"expression\": \"SET #title = :title, #tags = :tags\",\n    \
         \"expressionNames\": {{\"#title\": \"title\", \"#tags\": \"tags\"}},\n    \
         \"expressionValues\": {{\n      \
         \":title\": $util.dynamodb.toDynamoDBJson($ctx.args.title),\n      \
         \":tags\": $util.dynamodb.toDynamoDBJson($ctx.args.tags)\n    }}\n  }}{stated}\n}}"
    )
}

/// The `DeleteItem` request object the delete renders: the row it addresses and the version the
/// caller states.
fn delete_template() -> String {
    format!(
        "{{\n  \"version\": \"{REQUEST_VERSION}\",\n  \"operation\": \"DeleteItem\",\n  \
         \"key\": {{\"{KEY_ATTRIBUTE}\": $util.dynamodb.toDynamoDBJson($ctx.args.id)}},\n  \
         \"{VERSION_ATTRIBUTE}\": $ctx.args.expected\n}}"
    )
}

/// POST one mutation to the address the API published and answer what it answered.
async fn mutate(case: &str, api: &Api, field: &str, arguments: &Value) -> Value {
    let document = document(field, arguments);
    let (status, answered) = post_published_uri(
        case,
        &api.graphql_uri,
        &document,
        &[("x-api-key", api.api_key.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the endpoint answers a mutation 200, got {status} and {answered}"
    );
    answered
}

/// The document a client POSTs to drive `field`, with one variable per argument the case states.
fn document(field: &str, arguments: &Value) -> Value {
    let declared: Vec<String> = arguments
        .as_object()
        .map(|given| {
            given
                .iter()
                .map(|(name, value)| format!("${name}: {}", variable_type(value)))
                .collect()
        })
        .unwrap_or_default();
    let passed: Vec<String> = arguments
        .as_object()
        .map(|given| given.keys().map(|name| format!("{name}: ${name}")).collect())
        .unwrap_or_default();
    json!({
        "query": format!(
            "mutation Drive({}) {{ {field}({}) {{ id title tags resolvedBy {VERSION_ATTRIBUTE} \
             {LAST_CHANGED_AT_ATTRIBUTE} {DELETED_ATTRIBUTE} }} }}",
            declared.join(", "),
            passed.join(", "),
        ),
        "variables": arguments.clone(),
    })
}

/// The GraphQL type the variable carrying `value` is declared as, which the schema's own
/// argument types decide.
fn variable_type(value: &Value) -> &'static str {
    match value {
        Value::Number(_) => "Int",
        Value::Array(_) => "[String]",
        _ => "String",
    }
}

/// The `errors` a result reports, as the `(errorType, message)` pairs its entries state.
fn reported(answered: &Value) -> Vec<(String, String)> {
    answered["errors"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["errorType"].as_str().unwrap_or_default().to_string(),
                        entry["message"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The whole number the attribute `name` of a row the TABLE holds carries, or `None` where the
/// row carries no such attribute.
fn attribute_number(item: &Value, name: &str) -> Option<i64> {
    item.get(name)?.get("N")?.as_str()?.parse().ok()
}

#[tokio::test]
async fn a_versioned_data_source_maintains_the_version_and_change_time_of_every_write() {
    let router = build_router();
    create_table(&router).await;
    let handler = create_function(&router, "versioned-conflict-handler", CONFLICT_HANDLER).await;
    let api = create_api(&router, "appsync-versioned-writes", &handler).await;

    // The rows are the three operations in the order one item's life runs them, each stating the
    // version the last accepted write left: a create, two accepted updates, and the delete.
    let id = "post-lifecycle";
    let created = mutate(
        "a create",
        &api,
        "createPost",
        &json!({"id": id, "title": "first", "tags": ["seed"]}),
    )
    .await;
    assert_eq!(
        (
            reported(&created),
            created["data"]["createPost"][VERSION_ATTRIBUTE].as_i64(),
            created["data"]["createPost"]["title"].as_str(),
        ),
        (Vec::new(), Some(1), Some("first")),
        "case \"a create\": the first write of an item is version 1, got {created}"
    );
    let first_changed_at = created["data"]["createPost"][LAST_CHANGED_AT_ATTRIBUTE]
        .as_f64()
        .unwrap_or_default();
    assert!(
        first_changed_at > 0.0,
        "case \"a create\": the write stamps when it happened, got {created}"
    );

    // The row the TABLE holds carries both attributes, because that is what a client reads back.
    let stored = held(&router, id)
        .await
        .unwrap_or_else(|| panic!("case \"a create\": the table must hold the row it wrote"));
    assert_eq!(
        (
            attribute_number(&stored, VERSION_ATTRIBUTE),
            stored["title"]["S"].as_str(),
        ),
        (Some(1), Some("first")),
        "case \"a create\": the table holds the stamped row, got {stored}"
    );
    assert!(
        attribute_number(&stored, LAST_CHANGED_AT_ATTRIBUTE).is_some_and(|at| at > 0),
        "case \"a create\": the table holds the change time, got {stored}"
    );

    // Each accepted write after it advances the version by exactly one.
    for (case, expected, title, version) in [
        ("the first accepted update", 1, "second", 2),
        ("the second accepted update", 2, "third", 3),
    ] {
        let answered = mutate(
            case,
            &api,
            "retitleOptimistic",
            &json!({"id": id, "title": title, "tags": ["seed"], "expected": expected}),
        )
        .await;
        let field = &answered["data"]["retitleOptimistic"];
        assert_eq!(
            (
                reported(&answered),
                field[VERSION_ATTRIBUTE].as_i64(),
                field["title"].as_str(),
            ),
            (Vec::new(), Some(version), Some(title)),
            "case {case:?}: an accepted write advances the version by one, got {answered}"
        );
        let stored = held(&router, id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the table must still hold the row"));
        assert_eq!(
            (
                attribute_number(&stored, VERSION_ATTRIBUTE),
                stored["title"]["S"].as_str(),
            ),
            (Some(version), Some(title)),
            "case {case:?}: the table holds the advanced row, got {stored}"
        );
    }

    // A write whose request object states NO version at all claims nothing about the version it
    // read, so under version detection there is nothing to detect a conflict against: the write
    // is applied and advances the version like any other accepted one.
    let unstated = mutate(
        "a write stating no version",
        &api,
        "retitleUnstated",
        &json!({"id": id, "title": "fourth", "tags": ["seed"]}),
    )
    .await;
    assert_eq!(
        (
            reported(&unstated),
            unstated["data"]["retitleUnstated"][VERSION_ATTRIBUTE].as_i64(),
            unstated["data"]["retitleUnstated"]["title"].as_str(),
        ),
        (Vec::new(), Some(4), Some("fourth")),
        "case \"a write stating no version\": a write that claims no version is applied and \
         advances it, got {unstated}"
    );

    // A delete of such a data source leaves the row BEHIND, marked deleted, with its version
    // advanced and a reaping marker a retention window past the write.
    let removed = mutate(
        "a delete",
        &api,
        "removePost",
        &json!({"id": id, "expected": 4}),
    )
    .await;
    assert_eq!(
        (
            reported(&removed),
            removed["data"]["removePost"][VERSION_ATTRIBUTE].as_i64(),
            removed["data"]["removePost"][DELETED_ATTRIBUTE].as_bool(),
        ),
        (Vec::new(), Some(5), Some(true)),
        "case \"a delete\": a versioned delete marks the row and advances the version, got \
         {removed}"
    );
    let stored = held(&router, id).await.unwrap_or_else(|| {
        panic!("case \"a delete\": a versioned delete leaves the row behind as a tombstone")
    });
    let changed_at = attribute_number(&stored, LAST_CHANGED_AT_ATTRIBUTE).unwrap_or_default();
    assert_eq!(
        (
            attribute_number(&stored, VERSION_ATTRIBUTE),
            stored[DELETED_ATTRIBUTE]["BOOL"].as_bool(),
            attribute_number(&stored, TTL_ATTRIBUTE),
        ),
        (
            Some(5),
            Some(true),
            Some(changed_at / 1_000 + BASE_TABLE_TTL_MINUTES * 60),
        ),
        "case \"a delete\": the tombstone carries the marker, the version and the reaping stamp \
         a retention window past the write, got {stored}"
    );

    // A delete of a row that is not there removes nothing and leaves no tombstone behind.
    let absent = mutate(
        "a delete of no row",
        &api,
        "removePost",
        &json!({"id": "post-never-written", "expected": 1}),
    )
    .await;
    assert_eq!(
        (reported(&absent), absent["data"]["removePost"].as_null()),
        (Vec::new(), Some(())),
        "case \"a delete of no row\": nothing is removed and nothing is reported, got {absent}"
    );
    assert!(
        held(&router, "post-never-written").await.is_none(),
        "case \"a delete of no row\": no tombstone is written for a row that never existed"
    );
}

#[tokio::test]
async fn a_versioned_write_is_refused_on_the_condition_its_own_request_stated() {
    let router = build_router();
    create_table(&router).await;
    let handler = create_function(&router, "versioned-condition-handler", CONFLICT_HANDLER).await;
    let api = create_api(&router, "appsync-versioned-conditions", &handler).await;

    // A conditional create of a row that is not there is ACCEPTED and stamped like any other
    // first write.
    let id = "post-conditional";
    let created = mutate(
        "a conditional create of a row that is not there",
        &api,
        "createIfAbsent",
        &json!({"id": id, "title": "first", "tags": ["seed"]}),
    )
    .await;
    assert_eq!(
        (
            reported(&created),
            created["data"]["createIfAbsent"][VERSION_ATTRIBUTE].as_i64(),
        ),
        (Vec::new(), Some(1)),
        "case \"a conditional create of a row that is not there\": the write is accepted and \
         stamped, got {created}"
    );

    // The same create again is REFUSED on exactly the grounds it stated: the condition no longer
    // holds, so the table refuses the write, the field reports what that service answered, and
    // the row stays as the accepted write left it.
    let refused = mutate(
        "a conditional create of a row that is there",
        &api,
        "createIfAbsent",
        &json!({"id": id, "title": "second", "tags": ["later"]}),
    )
    .await;
    let errors = reported(&refused);
    assert_eq!(
        (
            errors.iter().map(|(class, _)| class.as_str()).collect::<Vec<&str>>(),
            refused["data"]["createIfAbsent"].as_null(),
        ),
        (vec![CONDITION_ERROR_TYPE], Some(())),
        "case \"a conditional create of a row that is there\": the stated condition refuses the \
         write, got {refused}"
    );
    let stored = held(&router, id).await.unwrap_or_else(|| {
        panic!("case \"a conditional create of a row that is there\": the table holds the row")
    });
    assert_eq!(
        (
            attribute_number(&stored, VERSION_ATTRIBUTE),
            stored["title"]["S"].as_str(),
        ),
        (Some(1), Some("first")),
        "case \"a conditional create of a row that is there\": a refused write changes nothing, \
         got {stored}"
    );
}

#[tokio::test]
async fn a_stale_write_is_decided_by_the_conflict_handler_the_resolver_configures() {
    let router = build_router();
    create_table(&router).await;
    let handler = create_function(&router, "versioned-conflict-decider", CONFLICT_HANDLER).await;
    let api = create_api(&router, "appsync-versioned-conflicts", &handler).await;

    /// One case: the field it drives, the title the stale write carries, and what the conflict
    /// comes to — the version the row ends at, the title it ends with, and whether the write is
    /// reported as a conflict that was not resolved.
    struct Row<'a> {
        case: &'a str,
        field: &'a str,
        title: &'a str,
        version: i64,
        held_title: &'a str,
        reported_conflict: bool,
    }
    // The rows span every handler the rule names, and both decisions the invoked one takes.
    let rows: &[Row<'_>] = &[
        Row {
            case: "no handler configured at all",
            field: "retitle",
            title: "ignored",
            // A resolver that configures no sync at all compares no version, so the write is
            // applied as it stands.
            version: 2,
            held_title: "ignored",
            reported_conflict: false,
        },
        Row {
            case: "the NONE handler",
            field: "retitleUnhandled",
            title: "ignored",
            version: 1,
            held_title: "seeded",
            reported_conflict: true,
        },
        Row {
            case: "the OPTIMISTIC_CONCURRENCY handler",
            field: "retitleOptimistic",
            title: "ignored",
            version: 1,
            held_title: "seeded",
            reported_conflict: true,
        },
        Row {
            case: "the AUTOMERGE handler",
            field: "retitleAutomerge",
            title: "ignored",
            // The merge keeps the server's value for a scalar both rows carry and writes the
            // merged row as the next version.
            version: 2,
            held_title: "seeded",
            reported_conflict: false,
        },
        Row {
            case: "a LAMBDA handler that rejects the write",
            field: "retitleLambda",
            title: "reject-this",
            version: 1,
            held_title: "seeded",
            reported_conflict: true,
        },
        Row {
            case: "a LAMBDA handler that resolves the conflict",
            field: "retitleLambda",
            title: "resolve-this",
            version: 2,
            held_title: "resolve-this",
            reported_conflict: false,
        },
    ];
    for row in rows {
        let case = row.case;
        let id = format!("post-{}", case.replace(' ', "-"));
        let seeded = mutate(
            case,
            &api,
            "createPost",
            &json!({"id": id, "title": "seeded", "tags": ["seed"]}),
        )
        .await;
        assert_eq!(
            (
                reported(&seeded),
                seeded["data"]["createPost"][VERSION_ATTRIBUTE].as_i64(),
            ),
            (Vec::new(), Some(1)),
            "case {case:?}: the row has to be seeded at version 1, got {seeded}"
        );

        let answered = mutate(
            case,
            &api,
            row.field,
            &json!({
                "id": id,
                "title": row.title,
                "tags": ["merged"],
                "expected": STALE_VERSION,
            }),
        )
        .await;
        let errors = reported(&answered);
        if row.reported_conflict {
            assert_eq!(
                errors,
                vec![(
                    CONFLICT_ERROR_TYPE.to_string(),
                    CONFLICT_MESSAGE.to_string()
                )],
                "case {case:?}: a conflict that was not resolved is reported as one, got \
                 {answered}"
            );
            // The row the server holds travels with the error, so the caller can retry from it.
            let carried = &answered["errors"][0]["data"];
            assert_eq!(
                (
                    carried[VERSION_ATTRIBUTE].as_i64(),
                    carried["title"].as_str(),
                ),
                (Some(1), Some("seeded")),
                "case {case:?}: the reported conflict carries the row the server holds, got \
                 {answered}"
            );
            assert!(
                answered["data"][row.field].is_null(),
                "case {case:?}: a rejected write resolves the field to nothing, got {answered}"
            );
        } else {
            assert_eq!(
                (errors, answered["data"][row.field][VERSION_ATTRIBUTE].as_i64()),
                (Vec::new(), Some(row.version)),
                "case {case:?}: a resolved conflict is written as the next version, got \
                 {answered}"
            );
        }

        // Whatever the handler decided, what the TABLE holds is what says it: the row the write
        // left behind, read through DynamoDB's own api.
        let stored = held(&router, &id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the table must hold the row"));
        assert_eq!(
            (
                attribute_number(&stored, VERSION_ATTRIBUTE),
                stored["title"]["S"].as_str(),
            ),
            (Some(row.version), Some(row.held_title)),
            "case {case:?}: expected the row to end at version {} titled {:?}, got {stored}",
            row.version,
            row.held_title
        );
    }

    // The AUTOMERGE row's own composition: the merged row keeps the server's scalar and gains
    // the elements the incoming list carries, which is what makes it a merge rather than a win.
    let merged = held(&router, "post-the-AUTOMERGE-handler")
        .await
        .unwrap_or_else(|| panic!("case \"the AUTOMERGE handler\": the table must hold the row"));
    assert_eq!(
        merged["tags"],
        json!({"L": [{"S": "seed"}, {"S": "merged"}]}),
        "case \"the AUTOMERGE handler\": a merged list keeps what the server holds and gains \
         what the write carried, got {merged}"
    );

    // The resolving handler's own answer: it was shown the row the server holds and the row the
    // write would make, and the row it composed is what the table now holds.
    let resolved = held(&router, "post-a-LAMBDA-handler-that-resolves-the-conflict")
        .await
        .unwrap_or_else(|| {
            panic!("case \"a LAMBDA handler that resolves the conflict\": the table holds the row")
        });
    assert_eq!(
        (
            resolved["resolvedBy"]["S"].as_str(),
            attribute_number(&resolved, "seenVersion"),
        ),
        (Some("Mutation.retitleLambda"), Some(1)),
        "case \"a LAMBDA handler that resolves the conflict\": the handler is told which \
         resolver the write is of and the row the server holds, got {resolved}"
    );
}
