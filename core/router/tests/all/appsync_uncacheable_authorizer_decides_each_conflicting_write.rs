// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API gated by an `AWS_LAMBDA` authorizer whose decisions the authorizer
//! itself declares the reuse window of, writing through a VERSIONED `AMAZON_DYNAMODB` data source
//! whose resolver detects conflicts by version and hands every conflict to a LAMBDA conflict
//! handler — end to end over the real `MultiServiceRouter`, with AppSync, DynamoDB, Lambda, IAM
//! and CloudFormation mounted on one engine.
//!
//! RULE 1 — a write through a data source configured for versioned items, made by a resolver
//! asking for version-based detection ("`VERSION`: Detect conflicts based on object versions",
//! `SyncConfig$conflictDetection`), is a CONFLICT where the version the request object states is
//! not the one the item holds, and what becomes of that conflict is the function the resolver
//! names ("resolve conflicts with an Lambda function supplied in the
//! `LambdaConflictHandlerConfig`", `SyncConfig$conflictHandler` `LAMBDA`): the row that function
//! RESOLVED with is written as the item's next version, and a conflict it did NOT resolve writes
//! nothing — the field answers nothing, exactly one `ConflictUnhandled` error of that field is
//! reported, and the item the table holds keeps the version and every attribute the last accepted
//! write left on it. A write whose stated version IS the one held is no conflict, so no function
//! is consulted for it at all.
//!
//! RULE 2 — the decision such a request was admitted by is taken by the API's Lambda authorizer,
//! and may be REUSED only while a positive window of it is live. The window in force is the one
//! the function stated for itself ("the Lambda function can override this by returning a
//! `ttlOverride` key in its response",
//! `smithy_query(query="shape", shape="LambdaAuthorizerConfig")`), else the one the API
//! configured, and "the default is 0 seconds, which disables caching" — so a window that is zero,
//! stated, configured or defaulted, is one authorizer invocation PER REQUEST, and a positive one
//! is a single invocation shared by every request inside it.
//!
//! Five services have to agree for either rule to hold — DynamoDB holds the rows, Lambda runs
//! both the authorizer and the conflict handler, CloudFormation archives each into its deployment
//! package, IAM holds their execution roles, and AppSync publishes the endpoint, decides the
//! request and then decides the conflict — so this is the suite that can pin them together, which
//! is what a client making an optimistic-concurrency write under a per-request authorizer depends
//! on. Every write a case makes through the published endpoint is read back through DynamoDB's
//! OWN api, so what a field reports is what that service holds.
//!
//! The rows span what each rule generalises over: for RULE 1 the version the write states (one no
//! row reaches, and the one the row holds) and what the invoked handler decided (a rejection and
//! a resolution); for RULE 2 the window the API configures (each published bound of its range,
//! one inside it, and a configuration leaving it out so the documented default stands) against
//! the window the authorizer states for itself (none, zero, positive, and a value that is no
//! window at all).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_lambda_data_source_and_authorizer::{
    appsync_ok, build_router, create_function, post_published_uri,
};

/// The Region every request here signs for, which is the one that target's control-plane helpers
/// address.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which every setup call is signed with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The table every resolver of this suite writes, and its one key attribute.
const TABLE: &str = "appsync-authorized-conflicts";
const KEY_ATTRIBUTE: &str = "id";

/// The versioned data source every resolver resolves against.
const DATA_SOURCE: &str = "versionedPosts";

/// The `version` every request object of this suite states, as the AppSync DynamoDB resolver
/// reference writes it.
const REQUEST_VERSION: &str = "2018-05-29";

/// The attribute AppSync owns on an item of a versioned data source that carries its version,
/// which is also the member a request object states the version it expects in.
const VERSION_ATTRIBUTE: &str = "_version";

/// The attributes this suite's own rows carry: the authorizer decision the REQUEST was served
/// under, as the resolver read it, and the one the conflict HANDLER was told about when it
/// resolved the write.
const DECIDED_BY_ATTRIBUTE: &str = "decidedBy";
const RESOLVED_BY_ATTRIBUTE: &str = "resolvedBy";

/// The error class and the sentence AppSync reports a conflict that was not resolved with.
const CONFLICT_ERROR_TYPE: &str = "ConflictUnhandled";
const CONFLICT_MESSAGE: &str = "Conflict resolver rejects mutation.";

/// The version the write that creates an item stamps, and a version no row of this suite ever
/// reaches — so a write stating it conflicts for that reason alone.
const FIRST_VERSION: i64 = 1;
const STALE_VERSION: i64 = 99;

/// The published bounds of `authorizerResultTtlInSeconds` (`@range` 0..3600, `@default` 0;
/// `smithy_query(query="shape", shape="LambdaAuthorizerConfig")`), and a window well inside that
/// range so a positive one is exercised away from its bound as well as at it.
const TTL_MIN: i64 = 0;
const TTL_MAX: i64 = 3600;
const TTL_INSIDE: i64 = 300;

/// How long a poll loop waits for a state another service owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches a
/// transition.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The status a schema creation settles at once the document has been installed.
const SCHEMA_CREATED: &str = "SUCCESS";

/// How many requests a RULE 2 row makes presenting one token, which is what tells one authorizer
/// invocation from several.
const REQUESTS: usize = 3;

/// The token every SEED write presents: its own, so the decision a row's own token is served
/// under is never one this suite took for the seeding.
const SEED_TOKEN: &str = "seed-caller";

/// The schema every API of this suite is created with: the row the table holds, with the
/// attributes that say which authorizer decision each half of a request ran under, and the two
/// mutations a row drives.
const SCHEMA: &str = "\
type Post {\n  id: String\n  title: String\n  decidedBy: String\n  resolvedBy: String\n  \
  _version: Int\n  _lastChangedAt: Float\n}\n\
\n\
type Query {\n  post(id: String): Post\n}\n\
\n\
type Mutation {\n  \
  createPost(id: String, title: String): Post\n  \
  retitle(id: String, title: String, expected: Int): Post\n}\n";

/// The conflict handler every resolver of this suite names.
///
/// It RESOLVES a conflict whose incoming title asks for one — composing the resolved row from the
/// row the server holds, the title the write wanted, the decision the write was made under and
/// the decision IT was told about — and REJECTS every other one.
const CONFLICT_HANDLER: &str = r#"
def handler(event, context):
    new_item = event.get("newItem") or {}
    existing = event.get("existingItem") or {}
    identity = event.get("identity") or {}
    title = str(new_item.get("title") or "")
    if not title.startswith("resolve"):
        return {"action": "REJECT"}
    resolved = dict(existing)
    resolved["title"] = "resolved:" + title
    resolved["decidedBy"] = new_item.get("decidedBy")
    resolved["resolvedBy"] = (identity.get("resolverContext") or {}).get("decision")
    return {"action": "RESOLVE", "item": resolved}
"#;

/// The authorizer every API of this suite is gated by.
///
/// It authorizes every call and publishes the id of its OWN invocation — so a row can tell one
/// invocation from several — and states the window the token asks it to state, so one package
/// serves a row whose authorizer declares its decision uncacheable and one that states no window
/// at all.
const AUTHORIZER_HANDLER: &str = r#"
def handler(event, context):
    token = event.get("authorizationToken") or ""
    answered = {
        "isAuthorized": True,
        "resolverContext": {"decision": context.aws_request_id},
    }
    _, marker, stated = token.partition("ttl-")
    if marker and stated:
        answered["ttlOverride"] = int(stated)
    return answered
"#;

/// The response template every resolver carries: a conflict the endpoint did not resolve is
/// reported as the field's own error, carrying the class it is of and the row the endpoint handed
/// the half as its result — the handling the AppSync conflict-detection reference writes.
const RESPONSE_TEMPLATE: &str = "#if($ctx.error)\n  \
                                 $util.error($ctx.error.message, $ctx.error.type, $ctx.result)\n\
                                 #end\n$util.toJson($ctx.result)";

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

/// Create the table every row reads or writes and wait until DynamoDB first reports it `ACTIVE` —
/// a table that is not is one no call lands on.
async fn create_table(router: &MultiServiceRouter) {
    dynamodb_ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": KEY_ATTRIBUTE, "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
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

/// The whole number the attribute `name` of a row the TABLE holds carries, or `None` where the row
/// carries no such attribute.
fn attribute_number(item: &Value, name: &str) -> Option<i64> {
    item.get(name)?.get("N")?.as_str()?.parse().ok()
}

/// The string the attribute `name` of a row the TABLE holds carries, or `None` where the row
/// carries no such attribute.
fn attribute_text<'a>(item: &'a Value, name: &str) -> Option<&'a str> {
    item.get(name)?.get("S")?.as_str()
}

/// Create one `AWS_LAMBDA` API gated by `authorizer_arn` whose decisions are cached for
/// `configured` seconds (for the documented default where that is `None`), install [`SCHEMA`],
/// attach the VERSIONED data source, and attach the create and the version-detected retitle whose
/// conflicts `conflict_handler_arn` decides. Answers the URL the API published for its GraphQL
/// endpoint, which is the address a client dials.
async fn create_api(
    router: &MultiServiceRouter,
    name: &str,
    authorizer_arn: &str,
    conflict_handler_arn: &str,
    configured: Option<i64>,
) -> String {
    let mut lambda_authorizer_config = json!({"authorizerUri": authorizer_arn});
    if let Some(seconds) = configured {
        lambda_authorizer_config["authorizerResultTtlInSeconds"] = json!(seconds);
    }
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({
            "name": name,
            "authenticationType": "AWS_LAMBDA",
            "lambdaAuthorizerConfig": lambda_authorizer_config,
        })),
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
        "setup {name:?}: uris.GRAPHQL must be a URL this emulator serves, got {graphql_uri:?}"
    );
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/schemacreation"),
        Some(json!({"definition": ::aws_smithy_types::base64::encode(SCHEMA)})),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let settled = appsync_ok(
            router,
            "GET",
            &format!("/v1/apis/{api_id}/schemacreation"),
            None,
        )
        .await;
        if settled["status"].as_str() == Some(SCHEMA_CREATED) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup {name:?}: the schema creation must be performed, last read {settled}"
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
                "arn:aws:iam::{}:role/appsync-authorized-conflicts",
                engine::EMULATOR_ACCOUNT_ID
            ),
            "dynamodbConfig": {
                "tableName": TABLE,
                "awsRegion": REGION,
                "versioned": true,
            },
        })),
    )
    .await;
    for (field, request, sync) in [
        (
            "createPost",
            put_template(),
            json!({"conflictDetection": "VERSION", "conflictHandler": "OPTIMISTIC_CONCURRENCY"}),
        ),
        (
            "retitle",
            update_template(),
            json!({
                "conflictDetection": "VERSION",
                "conflictHandler": "LAMBDA",
                "lambdaConflictHandlerConfig": {
                    "lambdaConflictHandlerArn": conflict_handler_arn,
                },
            }),
        ),
    ] {
        appsync_ok(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Mutation/resolvers"),
            Some(json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": DATA_SOURCE,
                "requestMappingTemplate": request,
                "responseMappingTemplate": RESPONSE_TEMPLATE,
                "syncConfig": sync,
            })),
        )
        .await;
    }
    graphql_uri
}

/// The `PutItem` request object the create renders: the row's key, the title the call carries, the
/// authorizer decision the request was served under, and the version a create states.
fn put_template() -> String {
    format!(
        "{{\n  \"version\": \"{REQUEST_VERSION}\",\n  \"operation\": \"PutItem\",\n  \
         \"key\": {{\"{KEY_ATTRIBUTE}\": $util.dynamodb.toDynamoDBJson($ctx.args.id)}},\n  \
         \"attributeValues\": {{\n    \
         \"title\": $util.dynamodb.toDynamoDBJson($ctx.args.title),\n    \
         \"{DECIDED_BY_ATTRIBUTE}\": \
         $util.dynamodb.toDynamoDBJson($ctx.identity.resolverContext.decision)\n  }},\n  \
         \"{VERSION_ATTRIBUTE}\": {FIRST_VERSION}\n}}"
    )
}

/// The `UpdateItem` request object the retitle renders: the expression that writes the title and
/// the decision the request was served under, and the version the caller expects — which is what
/// version detection compares against the one the row holds.
fn update_template() -> String {
    format!(
        "{{\n  \"version\": \"{REQUEST_VERSION}\",\n  \"operation\": \"UpdateItem\",\n  \
         \"key\": {{\"{KEY_ATTRIBUTE}\": $util.dynamodb.toDynamoDBJson($ctx.args.id)}},\n  \
         \"update\": {{\n    \
         \"expression\": \"SET #title = :title, #decidedBy = :decidedBy\",\n    \
         \"expressionNames\": {{\"#title\": \"title\", \
         \"#decidedBy\": \"{DECIDED_BY_ATTRIBUTE}\"}},\n    \
         \"expressionValues\": {{\n      \
         \":title\": $util.dynamodb.toDynamoDBJson($ctx.args.title),\n      \
         \":decidedBy\": \
         $util.dynamodb.toDynamoDBJson($ctx.identity.resolverContext.decision)\n    }}\n  }},\n  \
         \"{VERSION_ATTRIBUTE}\": $ctx.args.expected\n}}"
    )
}

/// The members every mutation of this suite selects, which is the whole row a client reads back.
const SELECTION: &str = "id title decidedBy resolvedBy _version _lastChangedAt";

/// The document a client POSTs to create the row `id`.
fn seed_document(id: &str) -> Value {
    json!({
        "query": format!(
            "mutation Seed($id: String, $title: String) \
             {{ createPost(id: $id, title: $title) {{ {SELECTION} }} }}"
        ),
        "variables": {"id": id, "title": "seeded"},
    })
}

/// The document a client POSTs to retitle the row `id`, stating the version it believes is
/// current.
fn retitle_document(id: &str, title: &str, expected: i64) -> Value {
    json!({
        "query": format!(
            "mutation Write($id: String, $title: String, $expected: Int) \
             {{ retitle(id: $id, title: $title, expected: $expected) {{ {SELECTION} }} }}"
        ),
        "variables": {"id": id, "title": title, "expected": expected},
    })
}

/// POST one document to the address the API published, presenting `token` the way AppSync reads
/// an authorizer's input, and answer what the endpoint answered.
async fn drive(case: &str, graphql_uri: &str, token: &str, document: &Value) -> Value {
    let (status, answered) = post_published_uri(
        case,
        graphql_uri,
        document,
        &[("authorization", token.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the published endpoint to answer HTTP 200, got {status} and \
         {answered}"
    );
    answered
}

/// Create the row `id` at [`FIRST_VERSION`] and answer the decision that write was served under,
/// which a later case compares what it holds against.
async fn seed(case: &str, graphql_uri: &str, id: &str) -> String {
    let answered = drive(case, graphql_uri, SEED_TOKEN, &seed_document(id)).await;
    let field = &answered["data"]["createPost"];
    assert_eq!(
        (
            reported(&answered),
            field[VERSION_ATTRIBUTE].as_i64(),
            field["title"].as_str(),
        ),
        (Vec::new(), Some(FIRST_VERSION), Some("seeded")),
        "case {case:?}: the row has to be seeded at version {FIRST_VERSION}, got {answered}"
    );
    field[DECIDED_BY_ATTRIBUTE]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the seeded row has to carry the decision it was written under, \
                 got {answered}"
            )
        })
        .to_string()
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

/// RULE 1: under version detection, a conflict handed to a LAMBDA handler that REJECTS the write
/// leaves the row exactly as the last accepted write left it and is reported as a conflict nobody
/// resolved; one that resolves it is written as the next version; and a write stating the version
/// the row holds is no conflict at all, so no handler decides it.
#[tokio::test]
async fn a_version_conflict_is_written_only_where_the_lambda_handler_resolved_it() {
    let router = build_router();
    create_table(&router).await;
    let authorizer = create_function(&router, "conflict-request-authorizer", AUTHORIZER_HANDLER)
        .await;
    let conflict_handler =
        create_function(&router, "conflict-write-decider", CONFLICT_HANDLER).await;
    // The API this rule is graded on declares its decisions uncacheable, which is the shape a
    // client making a write under a per-request authorizer runs: every row below is admitted by an
    // invocation of its own.
    let graphql_uri = create_api(
        &router,
        "appsync-authorized-conflicts",
        &authorizer,
        &conflict_handler,
        Some(TTL_MIN),
    )
    .await;

    /// One case: the title the write carries, the version it states, and what the conflict comes
    /// to — whether it is reported as one nobody resolved, the version the row ends at, and the
    /// title it ends with.
    struct Row<'a> {
        case: &'a str,
        title: &'a str,
        expected: i64,
        reported_conflict: bool,
        version: i64,
        held_title: &'a str,
    }
    // The rows span the version the write states and what the invoked handler decided.
    let rows: &[Row<'_>] = &[
        Row {
            case: "a stale write whose conflict handler rejects it",
            title: "reject-this",
            expected: STALE_VERSION,
            reported_conflict: true,
            version: FIRST_VERSION,
            held_title: "seeded",
        },
        Row {
            case: "a stale write whose conflict handler resolves it",
            title: "resolve-this",
            expected: STALE_VERSION,
            reported_conflict: false,
            version: 2,
            held_title: "resolved:resolve-this",
        },
        Row {
            // The title this row carries is one the handler REJECTS, so the write landing says
            // the handler was never consulted: there was no conflict to hand it.
            case: "a write stating the version the row holds",
            title: "unconflicted-title",
            expected: FIRST_VERSION,
            reported_conflict: false,
            version: 2,
            held_title: "unconflicted-title",
        },
    ];
    for row in rows {
        let case = row.case;
        let id = format!("post-{}", case.replace(' ', "-"));
        let seeded_under = seed(case, &graphql_uri, &id).await;

        let answered = drive(
            case,
            &graphql_uri,
            "write-caller-ttl-0",
            &retitle_document(&id, row.title, row.expected),
        )
        .await;
        if row.reported_conflict {
            assert_eq!(
                reported(&answered),
                vec![(
                    CONFLICT_ERROR_TYPE.to_string(),
                    CONFLICT_MESSAGE.to_string(),
                )],
                "case {case:?}: a conflict nobody resolved is reported as exactly that, got \
                 {answered}"
            );
            assert!(
                answered["data"]["retitle"].is_null(),
                "case {case:?}: a rejected write resolves the field to nothing, got {answered}"
            );
            // The row the server holds travels with the error, so the caller can retry from it.
            let carried = &answered["errors"][0]["data"];
            assert_eq!(
                (
                    carried[VERSION_ATTRIBUTE].as_i64(),
                    carried["title"].as_str(),
                ),
                (Some(FIRST_VERSION), Some("seeded")),
                "case {case:?}: the reported conflict carries the row the server holds, got \
                 {answered}"
            );
        } else {
            let field = &answered["data"]["retitle"];
            assert_eq!(
                (
                    reported(&answered),
                    field[VERSION_ATTRIBUTE].as_i64(),
                    field["title"].as_str(),
                ),
                (Vec::new(), Some(row.version), Some(row.held_title)),
                "case {case:?}: expected the write to answer version {} titled {:?}, got \
                 {answered}",
                row.version,
                row.held_title
            );
        }

        // Whatever was decided, what the TABLE holds is what says it: the row the write left
        // behind, read through DynamoDB's own api.
        let stored = held(&router, &id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the table must hold the row"));
        assert_eq!(
            (
                attribute_number(&stored, VERSION_ATTRIBUTE),
                attribute_text(&stored, "title"),
            ),
            (Some(row.version), Some(row.held_title)),
            "case {case:?}: expected the row to end at version {} titled {:?}, got {stored}",
            row.version,
            row.held_title
        );
        if row.reported_conflict {
            // Nothing of the rejected write reached the row: the attribute it would have
            // overwritten still carries what the seeding write put there.
            assert_eq!(
                (
                    attribute_text(&stored, DECIDED_BY_ATTRIBUTE),
                    stored.get(RESOLVED_BY_ATTRIBUTE),
                ),
                (Some(seeded_under.as_str()), None),
                "case {case:?}: a rejected write leaves every attribute of the row as the last \
                 accepted write left it, got {stored}"
            );
        } else {
            assert_ne!(
                attribute_text(&stored, DECIDED_BY_ATTRIBUTE),
                Some(seeded_under.as_str()),
                "case {case:?}: an accepted write writes the decision it was served under, got \
                 {stored}"
            );
        }
    }
}

/// RULE 2: the window a decision may be reused for is the one the authorizer stated for itself
/// where it stated one, else the one the API configured, and only a positive window is reused — so
/// a window of zero is one invocation per request even where every request is a conflicting write
/// the conflict handler has to decide.
#[tokio::test]
async fn an_authorizers_own_window_decides_how_many_invocations_serve_its_requests() {
    let router = build_router();
    create_table(&router).await;
    let authorizer = create_function(&router, "conflict-window-authorizer", AUTHORIZER_HANDLER)
        .await;
    let conflict_handler =
        create_function(&router, "conflict-window-decider", CONFLICT_HANDLER).await;

    /// One case: the window the API configures, the window the authorizer states for itself, and
    /// how many invocations of the authorizer the row's requests must come from.
    struct Window<'a> {
        case: &'a str,
        /// The configured window, or `None` for a configuration leaving the member out.
        configured: Option<i64>,
        /// The window the authorizer states, or `None` for a function that states none.
        stated: Option<i64>,
        invocations: usize,
    }
    // The rows span the configured window — each published bound of its range, one inside it, and
    // a configuration leaving it out so the documented default stands — against what the
    // authorizer states for itself.
    let rows: &[Window<'_>] = &[
        Window {
            case: "an authorizer declaring its decision uncacheable against the configured \
                   maximum",
            configured: Some(TTL_MAX),
            stated: Some(0),
            invocations: REQUESTS,
        },
        Window {
            case: "an authorizer stating no window against a configured window inside the range",
            configured: Some(TTL_INSIDE),
            stated: None,
            invocations: 1,
        },
        Window {
            case: "an authorizer stating a positive window against the configured minimum",
            configured: Some(TTL_MIN),
            stated: Some(TTL_INSIDE),
            invocations: 1,
        },
        Window {
            case: "an authorizer stating no window where the configuration states none either",
            configured: None,
            stated: None,
            invocations: REQUESTS,
        },
        Window {
            case: "an authorizer stating a window that is no window at all",
            configured: Some(TTL_INSIDE),
            stated: Some(-1),
            invocations: REQUESTS,
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let graphql_uri = create_api(
            &router,
            &format!("appsync-authorized-window-{index}"),
            &authorizer,
            &conflict_handler,
            row.configured,
        )
        .await;
        let token = match row.stated {
            Some(stated) => format!("window-caller-{index}-ttl-{stated}"),
            None => format!("window-caller-{index}"),
        };
        let id = format!("post-window-{index}");
        seed(case, &graphql_uri, &id).await;

        // Every request is a write stating a version no row reaches, so each one is a conflict the
        // handler resolves — and the row it resolves with carries the authorizer decision that
        // request was served under.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut last = String::new();
        for attempt in 0..REQUESTS {
            let title = format!("resolve-{attempt}");
            let answered = drive(
                case,
                &graphql_uri,
                &token,
                &retitle_document(&id, &title, STALE_VERSION),
            )
            .await;
            let field = &answered["data"]["retitle"];
            // Each resolved conflict is written as the item's next version, counting up from the
            // version the seeding write left.
            let version = FIRST_VERSION
                .checked_add(i64::try_from(attempt).expect("the attempt counts") + 1)
                .expect("the version counts");
            assert_eq!(
                (
                    reported(&answered),
                    field[VERSION_ATTRIBUTE].as_i64(),
                    field["title"].as_str(),
                ),
                (
                    Vec::new(),
                    Some(version),
                    Some(format!("resolved:{title}").as_str()),
                ),
                "case {case:?} request {attempt}: expected the resolved conflict to be written as \
                 version {version}, got {answered}"
            );
            let decision = field[DECIDED_BY_ATTRIBUTE]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?} request {attempt}: expected the row to carry the \
                         authorizer invocation the request was decided by, got {answered}"
                    )
                })
                .to_string();
            // One decision serves the whole request: the resolver read it, and the conflict
            // handler was told the same one when it resolved the write.
            assert_eq!(
                field[RESOLVED_BY_ATTRIBUTE].as_str(),
                Some(decision.as_str()),
                "case {case:?} request {attempt}: expected the conflict handler to be told the \
                 decision the request was admitted by, got {answered}"
            );
            seen.insert(decision.clone());
            last = decision;
        }
        assert_eq!(
            seen.len(),
            row.invocations,
            "case {case:?}: expected {REQUESTS} requests to be decided by {} authorizer \
             invocation(s), got the decisions {seen:?}",
            row.invocations
        );

        // What the TABLE holds is what says the last write happened at all, and which decision it
        // ran under.
        let stored = held(&router, &id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the table must hold the row"));
        assert_eq!(
            (
                attribute_text(&stored, DECIDED_BY_ATTRIBUTE),
                attribute_text(&stored, RESOLVED_BY_ATTRIBUTE),
            ),
            (Some(last.as_str()), Some(last.as_str())),
            "case {case:?}: expected the row to hold the decision its last write ran under, got \
             {stored}"
        );
    }
}
