// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose resolvers issue `BatchGetItem` and paginated `Query` requests
//! against an `AMAZON_DYNAMODB` data source, driven at the address the API publishes.
//!
//! Two rules, both of the resolver-invocation path:
//!
//! > A `BatchGetItem` request object is answered ONE SLOT PER KEY IT ASKED FOR: for every table
//! > the request names, `ctx.result.data[<table>]` is a list as long as that table's `keys` and
//! > in the request's own order, carrying the item answered for the key at each position and
//! > `null` at a position whose key the table answered no item for — a key that is not there is
//! > a hole, never a dropped element and never a failure of the whole request — and
//! > `ctx.result.unprocessedKeys[<table>]` is stated for every table named too, empty where the
//! > call got to all of that table's keys.
//!
//! > A pagination token the endpoint did not MINT is an argument of the request it cannot read:
//! > the field is refused inside a `200` envelope with `errorType` `IllegalArgument` and the one
//! > fixed sentence AWS states for such a token, while the same selection with no token, and
//! > with a token the endpoint itself minted, serves its page.
//!
//! The rows span what the rules generalise over: how many keys one batch asks for, which of
//! them the table holds (none, some, all), where the holes fall (first, last, interleaved), the
//! ORDER the keys are asked in (the table's own and the reverse of it), the shape of the table's
//! key (one attribute, and a composite of two), and — for the token — the forms a value that is
//! no token of this endpoint takes.
//!
//! Both services have to agree for either rule to hold: DynamoDB holds the tables and answers
//! only the items it found, and AppSync publishes the endpoint and runs the resolver, so what a
//! field reports is correlated against what DynamoDB's OWN reader holds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request of this suite is made in.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which every control-plane call is signed with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The two tables the rows read: one keyed by a single attribute, one by a composite key, so
/// the arity rule is driven over both shapes a batch's keys take.
const SINGLE_KEY_TABLE: &str = "batch-items";
const COMPOSITE_KEY_TABLE: &str = "batch-pages";

/// One data source per table the rows read: a batch names the tables it addresses in its own
/// request, but a selection is made against the table its data source was configured with.
const SINGLE_KEY_TABLE_SOURCE: &str = "ddb";
const COMPOSITE_KEY_TABLE_SOURCE: &str = "ddbPages";

/// The partition the `pk` of every item of the composite-key table carries, which is what the
/// paginated selection reads.
const PARTITION: &str = "p";

/// How long a poll loop waits for a state the clock owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches a
/// transition.
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The schema every API of this suite is created with: the item each table holds, the page the
/// selection answers, and one field per request object a row drives.
const SCHEMA: &str = "\
type Item {\n  id: String\n  value: String\n}\n\
\n\
type Page {\n  pk: String\n  sk: String\n  value: String\n}\n\
\n\
type PageList {\n  items: [Page]\n  nextToken: String\n}\n\
\n\
type Query {\n  \
  batchLookup(ids: [String]): [Item]\n  \
  batchUnprocessed(ids: [String]): Int\n  \
  batchComposite(sks: [String]): [Page]\n  \
  listItems(nextToken: String, limit: Int): PageList\n}\n";

/// The response half of a resolver whose result is the per-table slot list: a call the table
/// REFUSED is the field's own error, and every HOLE is reported as one appended error carrying
/// the position it stands at — which is the handling the AppSync JavaScript resolver reference
/// writes for a batch, and what a client correlating by position needs.
const SLOT_RESPONSE: &str = "\
export function response(ctx) {\n  \
  if (ctx.error) {\n    util.error(ctx.error.message, ctx.error.type);\n  }\n  \
  const slots = ctx.result.data['$TABLE'];\n  \
  for (let at = 0; at < slots.length; at++) {\n    \
    if (slots[at] === null) {\n      \
      util.appendError('No item was found for this key.', 'NotFound', null, {index: at});\n    \
    }\n  \
  }\n  \
  return slots;\n}\n";

/// The request half of the field that looks up single-attribute keys.
const BATCH_LOOKUP_REQUEST: &str = "\
export function request(ctx) {\n  \
  return {operation: 'BatchGetItem', tables: {'$TABLE': {keys: \
  ctx.args.ids.map(function (id) { return {id: util.dynamodb.toDynamoDB(id)}; })}}};\n}\n";

/// The request half of the field that looks up COMPOSITE keys of the other table.
const BATCH_COMPOSITE_REQUEST: &str = "\
export function request(ctx) {\n  \
  return {operation: 'BatchGetItem', tables: {'$TABLE': {keys: \
  ctx.args.sks.map(function (sk) { return {pk: util.dynamodb.toDynamoDB('$PARTITION'), \
  sk: util.dynamodb.toDynamoDB(sk)}; })}}};\n}\n";

/// The response half of the field that reports how many keys the call did not get to: reading
/// `length` off the member is itself the assertion that a fully served batch states an empty
/// LIST for the table it was asked about rather than nothing at all.
const UNPROCESSED_RESPONSE: &str = "\
export function response(ctx) {\n  \
  if (ctx.error) {\n    util.error(ctx.error.message, ctx.error.type);\n  }\n  \
  return ctx.result.unprocessedKeys['$TABLE'].length;\n}\n";

/// The paginated selection: the token the field was given is carried only where the caller gave
/// one, so the same resolver serves the first page and a continuation.
const LIST_ITEMS_REQUEST: &str = "\
export function request(ctx) {\n  \
  const requested = {operation: 'Query', limit: ctx.args.limit, \
  query: {expression: '#p = :p', expressionNames: {'#p': 'pk'}, \
  expressionValues: {':p': util.dynamodb.toDynamoDB('$PARTITION')}}};\n  \
  if (ctx.args.nextToken) {\n    requested.nextToken = ctx.args.nextToken;\n  }\n  \
  return requested;\n}\n";

/// The response half of the paginated selection: the page the call answered.
const PAGE_RESPONSE: &str = "\
export function response(ctx) {\n  \
  if (ctx.error) {\n    util.error(ctx.error.message, ctx.error.type);\n  }\n  \
  return ctx.result;\n}\n";

/// The `errorType` AppSync refuses a pagination token it cannot read with, and the fixed
/// sentence it states; the service's own spelling of the sentence is
/// `state::INVALID_PAGINATION_TOKEN_MESSAGE`.
const ILLEGAL_ARGUMENT: &str = "IllegalArgument";
const INVALID_TOKEN_MESSAGE: &str = "Invalid pagination token given.";

/// The two services this suite's rules span, over ONE engine.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Request authentication is off in this target: what a case pins is what the endpoint
    // ANSWERS, never signature verification.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential is `AMBIENT_KEY`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One AppSync control-plane call (restJson1), asserting it was served.
async fn appsync_ok(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let payload = body.map_or_else(Vec::new, |body| {
        serde_json::to_vec(&body).expect("the test body serializes")
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://appsync.{REGION}.amazonaws.com{path}"))
        .header("host", format!("appsync.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("appsync"))
        .body(Bytes::from(payload))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "setup: {method} {path} expected 200, got {status} {parsed}"
    );
    parsed
}

/// One Amazon DynamoDB call over its own `awsJson1_0` wire, asserting it was served.
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

/// Create one table through DynamoDB's own API and wait until that service first reports it
/// `ACTIVE` — a table that is not is one no call lands on.
async fn create_table(router: &MultiServiceRouter, name: &str, key: &[&str]) {
    let schema: Vec<Value> = key
        .iter()
        .enumerate()
        .map(|(at, attribute)| {
            let kind = if at == 0 { "HASH" } else { "RANGE" };
            json!({"AttributeName": attribute, "KeyType": kind})
        })
        .collect();
    let definitions: Vec<Value> = key
        .iter()
        .map(|attribute| json!({"AttributeName": attribute, "AttributeType": "S"}))
        .collect();
    dynamodb_ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": schema,
            "AttributeDefinitions": definitions,
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = dynamodb_ok(router, "DescribeTable", json!({"TableName": name})).await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the table {name} must become ACTIVE, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One created GraphQL API: what it is addressed by, where it is served, and the key a client
/// presents.
struct Api {
    graphql_uri: String,
    api_key: String,
}

/// Create the API, install [`SCHEMA`], attach the DynamoDB data source and one resolver per
/// field, and answer what a client needs to drive it.
async fn standing(router: &MultiServiceRouter, name: &str) -> Api {
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": name, "authenticationType": "API_KEY"})),
    )
    .await;
    let api_id = created["graphqlApi"]["apiId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let graphql_uri = created["graphqlApi"]["uris"]["GRAPHQL"]
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
        let settled = appsync_ok(
            router,
            "GET",
            &format!("/v1/apis/{api_id}/schemacreation"),
            None,
        )
        .await;
        if settled["status"].as_str() == Some("SUCCESS") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the schema creation must be performed, last read {settled}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    // The data sources name a role, as every calling data source does; no IAM is mounted here,
    // so nothing is known against it and the permissions withhold nothing — what this suite
    // pins is the RESULT the call answers, which `appsync_dynamodb_data_source_resolvers`
    // pins the permission weighing of. One data source per table: a batch names the tables it
    // addresses itself, but a selection is made against the table its data source carries.
    for (name, table) in [
        (SINGLE_KEY_TABLE_SOURCE, SINGLE_KEY_TABLE),
        (COMPOSITE_KEY_TABLE_SOURCE, COMPOSITE_KEY_TABLE),
    ] {
        appsync_ok(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/datasources"),
            Some(json!({
                "name": name,
                "type": "AMAZON_DYNAMODB",
                "serviceRoleArn": format!(
                    "arn:aws:iam::{}:role/appsync-batch-get",
                    engine::EMULATOR_ACCOUNT_ID
                ),
                "dynamodbConfig": {
                    "tableName": table,
                    "awsRegion": REGION,
                },
            })),
        )
        .await;
    }
    for (field, request, response, table) in [
        (
            "batchLookup",
            BATCH_LOOKUP_REQUEST,
            SLOT_RESPONSE,
            SINGLE_KEY_TABLE,
        ),
        (
            "batchUnprocessed",
            BATCH_LOOKUP_REQUEST,
            UNPROCESSED_RESPONSE,
            SINGLE_KEY_TABLE,
        ),
        (
            "batchComposite",
            BATCH_COMPOSITE_REQUEST,
            SLOT_RESPONSE,
            COMPOSITE_KEY_TABLE,
        ),
        (
            "listItems",
            LIST_ITEMS_REQUEST,
            PAGE_RESPONSE,
            COMPOSITE_KEY_TABLE,
        ),
    ] {
        let data_source = if table == SINGLE_KEY_TABLE {
            SINGLE_KEY_TABLE_SOURCE
        } else {
            COMPOSITE_KEY_TABLE_SOURCE
        };
        let code = format!(
            "import {{ util }} from '@aws-appsync/utils';\n{}\n{}",
            request
                .replace("$TABLE", table)
                .replace("$PARTITION", PARTITION),
            response.replace("$TABLE", table)
        );
        appsync_ok(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers"),
            Some(json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": data_source,
                "runtime": {"name": "APPSYNC_JS", "runtimeVersion": "1.0.0"},
                "code": code,
            })),
        )
        .await;
    }
    Api {
        graphql_uri,
        api_key: key["apiKey"]["id"].as_str().unwrap_or_default().to_string(),
    }
}

/// POST one GraphQL document to the URL the API PUBLISHED, as a client does, presenting the
/// API key this suite's APIs admit.
///
/// The socket exchange itself is this target's ONE reading of how a client reaches a published
/// GraphQL endpoint ([`crate::appsync_lambda_data_source_and_authorizer::post_published_uri`]),
/// which takes the header fields a case presents — so there is one copy of it, not one per
/// suite.
async fn post_published(case: &str, api: &Api, body: &Value) -> (u16, Value) {
    crate::appsync_lambda_data_source_and_authorizer::post_published_uri(
        case,
        &api.graphql_uri,
        body,
        &[("x-api-key", api.api_key.clone())],
    )
    .await
}

/// The GraphQL document that selects `field` over the list of ids a row asks for.
fn batch_document(field: &str, argument: &str, keys: &[&str], selection: &str) -> Value {
    let listed: Vec<String> = keys.iter().map(|key| format!("\"{key}\"")).collect();
    json!({
        "query": format!(
            "query Batch {{ {field}({argument}: [{}]) {selection} }}",
            listed.join(", ")
        )
    })
}

/// The positions of the slots that must be `null`, read off a row's expectation.
fn holes(expected: &[Option<&str>]) -> Vec<usize> {
    expected
        .iter()
        .enumerate()
        .filter_map(|(at, held)| held.is_none().then_some(at))
        .collect()
}

/// Assert `answered` carries one slot per requested key, the value a row expects at each
/// position, and exactly one appended error per hole — naming the position it stands at and the
/// field it is of.
fn assert_slots(
    case: &str,
    field: &str,
    key_attribute: &str,
    keys: &[&str],
    expected: &[Option<&str>],
    answered: &Value,
) {
    let slots = answered["data"][field]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        slots.len(),
        keys.len(),
        "case {case:?}: expected one slot per requested key ({} keys), got {} in {answered}",
        keys.len(),
        slots.len()
    );
    for (at, held) in expected.iter().enumerate() {
        match held {
            None => assert_eq!(
                slots[at],
                Value::Null,
                "case {case:?}: expected null at {at}, got {}",
                slots[at]
            ),
            Some(value) => assert_eq!(
                (slots[at][key_attribute].as_str(), slots[at]["value"].as_str()),
                (Some(keys[at]), Some(*value)),
                "case {case:?}: expected the item of {:?} at {at}, got {}",
                keys[at],
                slots[at]
            ),
        }
    }
    let reported = answered["errors"].as_array().cloned().unwrap_or_default();
    let expected_holes = holes(expected);
    assert_eq!(
        reported.len(),
        expected_holes.len(),
        "case {case:?}: expected one appended error per hole ({:?}), got {answered}",
        expected_holes
    );
    let mut positions: Vec<i64> = reported
        .iter()
        .map(|error| {
            assert_eq!(
                error["path"],
                json!([field]),
                "case {case:?}: expected the error to name the field's path, got {error}"
            );
            error["errorInfo"]["index"].as_i64().unwrap_or(-1)
        })
        .collect();
    positions.sort_unstable();
    let as_expected: Vec<i64> = expected_holes.iter().map(|at| *at as i64).collect();
    assert_eq!(
        positions, as_expected,
        "case {case:?}: expected one error per hole position, got {answered}"
    );
}

#[tokio::test]
async fn a_batch_get_item_resolver_answers_one_slot_per_requested_key() {
    let router = build_router();
    create_table(&router, SINGLE_KEY_TABLE, &["id"]).await;
    create_table(&router, COMPOSITE_KEY_TABLE, &["pk", "sk"]).await;
    // Every second key of the single-attribute table is seeded, so a row asking for a run of
    // them asks across holes; the composite-key table holds two of three.
    for at in [0usize, 2, 4] {
        dynamodb_ok(
            &router,
            "PutItem",
            json!({
                "TableName": SINGLE_KEY_TABLE,
                "Item": {"id": {"S": format!("k{at}")}, "value": {"S": format!("seeded-{at}")}},
            }),
        )
        .await;
    }
    for sk in ["a", "c"] {
        dynamodb_ok(
            &router,
            "PutItem",
            json!({
                "TableName": COMPOSITE_KEY_TABLE,
                "Item": {
                    "pk": {"S": PARTITION},
                    "sk": {"S": sk},
                    "value": {"S": format!("page-{sk}")},
                },
            }),
        )
        .await;
    }
    let api = standing(&router, "batch-get-arity").await;

    /// One row: the keys it asks for and what the slot at each position must hold.
    struct Row<'a> {
        case: &'a str,
        keys: &'a [&'a str],
        expected: &'a [Option<&'a str>],
    }
    let rows: &[Row] = &[
        Row {
            case: "five keys of which three are held",
            keys: &["k0", "k1", "k2", "k3", "k4"],
            expected: &[Some("seeded-0"), None, Some("seeded-2"), None, Some("seeded-4")],
        },
        Row {
            case: "the same five keys in the reverse order",
            keys: &["k4", "k3", "k2", "k1", "k0"],
            expected: &[Some("seeded-4"), None, Some("seeded-2"), None, Some("seeded-0")],
        },
        Row {
            case: "three keys the table holds an item for each of",
            keys: &["k0", "k2", "k4"],
            expected: &[Some("seeded-0"), Some("seeded-2"), Some("seeded-4")],
        },
        Row {
            case: "three keys the table holds none of",
            keys: &["k1", "k3", "k9"],
            expected: &[None, None, None],
        },
        Row {
            case: "one key the table holds",
            keys: &["k2"],
            expected: &[Some("seeded-2")],
        },
        Row {
            case: "one key the table does not hold",
            keys: &["k1"],
            expected: &[None],
        },
        Row {
            case: "four keys whose holes are the first and the last",
            keys: &["k1", "k2", "k0", "k3"],
            expected: &[None, Some("seeded-2"), Some("seeded-0"), None],
        },
    ];
    for row in rows {
        let (status, answered) = post_published(
            row.case,
            &api,
            &batch_document("batchLookup", "ids", row.keys, "{ id value }"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the published endpoint must answer 200, got {status} and {answered}",
            row.case
        );
        assert_slots(
            row.case,
            "batchLookup",
            "id",
            row.keys,
            row.expected,
            &answered,
        );
    }

    // The same rule over a COMPOSITE key: a slot stands for the key at its position, which is
    // matched on every attribute that key names.
    let composite_rows: &[Row] = &[
        Row {
            case: "composite keys of which the middle one is absent",
            keys: &["a", "b", "c"],
            expected: &[Some("page-a"), None, Some("page-c")],
        },
        Row {
            case: "composite keys the table holds both of",
            keys: &["c", "a"],
            expected: &[Some("page-c"), Some("page-a")],
        },
    ];
    for row in composite_rows {
        let (status, answered) = post_published(
            row.case,
            &api,
            &batch_document("batchComposite", "sks", row.keys, "{ sk value }"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the published endpoint must answer 200, got {status} and {answered}",
            row.case
        );
        assert_slots(
            row.case,
            "batchComposite",
            "sk",
            row.keys,
            row.expected,
            &answered,
        );
    }

    // And a batch the call fully served states an empty list of unprocessed keys for the table
    // it was asked about: reading `length` off that member is what the resolver above does.
    for keys in [["k0"].as_slice(), ["k0", "k1", "k2", "k3", "k4"].as_slice()] {
        let case = format!("the unprocessed keys of a batch of {}", keys.len());
        let (status, answered) =
            post_published(&case, &api, &batch_document("batchUnprocessed", "ids", keys, "")).await;
        assert_eq!(
            (status, answered["data"]["batchUnprocessed"].as_i64()),
            (200, Some(0)),
            "case {case:?}: expected an empty list of unprocessed keys, got {answered}"
        );
        assert!(
            answered.get("errors").is_none(),
            "case {case:?}: expected no errors, got {answered}"
        );
    }
}

#[tokio::test]
async fn a_pagination_token_the_endpoint_did_not_mint_is_refused_as_an_illegal_argument() {
    let router = build_router();
    create_table(&router, SINGLE_KEY_TABLE, &["id"]).await;
    create_table(&router, COMPOSITE_KEY_TABLE, &["pk", "sk"]).await;
    for sk in ["a", "b"] {
        dynamodb_ok(
            &router,
            "PutItem",
            json!({
                "TableName": COMPOSITE_KEY_TABLE,
                "Item": {
                    "pk": {"S": PARTITION},
                    "sk": {"S": sk},
                    "value": {"S": format!("page-{sk}")},
                },
            }),
        )
        .await;
    }
    let api = standing(&router, "pagination-refusal").await;

    // The rows differ along the FORM a value that is no token of this endpoint takes: text of
    // no payload at all, a payload that decodes to nothing readable, and one that decodes to a
    // document that is no key.
    let encoded = |text: &str| ::aws_smithy_types::base64::encode(text.as_bytes());
    let rows: Vec<(String, String)> = vec![
        ("text of no payload at all".to_string(), "not-a-real-token".to_string()),
        ("a word of the base64 alphabet".to_string(), "abcd".to_string()),
        (
            "a payload that decodes to no document".to_string(),
            encoded("{not json"),
        ),
        (
            "a payload that decodes to a document that is no key".to_string(),
            encoded("[1, 2]"),
        ),
    ];
    for (case, token) in &rows {
        let (status, answered) = post_published(
            case,
            &api,
            &json!({
                "query": "query Listed($token: String) \
                          { listItems(nextToken: $token, limit: 1) { items { sk } nextToken } }",
                "variables": {"token": token},
            }),
        )
        .await;
        assert_eq!(
            (status, &answered["data"]["listItems"]),
            (200, &Value::Null),
            "case {case:?}: a refused selection resolves to nothing inside a 200 envelope, got \
             {status} and {answered}"
        );
        assert_eq!(
            (
                answered["errors"][0]["errorType"].as_str(),
                answered["errors"][0]["message"].as_str(),
            ),
            (Some(ILLEGAL_ARGUMENT), Some(INVALID_TOKEN_MESSAGE)),
            "case {case:?}: expected the illegal-argument refusal, got {answered}"
        );
    }

    // The identical selection with NO token serves its page, and the token that page reports —
    // one this endpoint minted — continues from where it left off: the refusal above is about
    // the value, not about the member.
    let case = "the same selection with no token";
    let (status, first) = post_published(
        case,
        &api,
        &json!({"query": "query { listItems(limit: 1) { items { sk } nextToken } }"}),
    )
    .await;
    assert_eq!(
        (
            status,
            first["data"]["listItems"]["items"][0]["sk"].as_str(),
            first.get("errors"),
        ),
        (200, Some("a"), None),
        "case {case:?}: expected the first page of the selection, got {status} and {first}"
    );
    let minted = first["data"]["listItems"]["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a token for the next page, got {first}"))
        .to_string();
    let case = "the token the first page minted";
    let (status, second) = post_published(
        case,
        &api,
        &json!({
            "query": "query Listed($token: String) \
                      { listItems(nextToken: $token, limit: 1) { items { sk } } }",
            "variables": {"token": minted},
        }),
    )
    .await;
    assert_eq!(
        (
            status,
            second["data"]["listItems"]["items"][0]["sk"].as_str(),
            second.get("errors"),
        ),
        (200, Some("b"), None),
        "case {case:?}: expected the page after the first, got {status} and {second}"
    );
}
