// SPDX-License-Identifier: CC0-1.0

//! The canonical authenticated GraphQL app: an AppSync API in the
//! `AMAZON_COGNITO_USER_POOLS` mode whose resolvers are VTL MAPPING TEMPLATES over an
//! `AMAZON_DYNAMODB` data source, marshalling the call's arguments and the caller's own
//! identity with the `$util.dynamodb` helpers.
//!
//! RULE: a mapping template that MARSHALS a value with a `$util.dynamodb` helper renders that
//! value as the attribute value DynamoDB holds it as — a `…Json` helper as that same
//! representation already serialized, so the template can inline it into the request object it
//! writes — and the request object the template rendered is the call AppSync makes against the
//! table. So such a resolver is served: the answer is HTTP 200 carrying the field's own value
//! and NO `errors` member at all, the item the template wrote is in the table under the
//! attribute types the helpers produced, and a later read of the same key — through the API
//! and through Amazon DynamoDB itself — reports it. A call the API's authorization mode
//! refuses reaches no template, so it writes nothing.
//!
//! Three services have to agree for that to hold — Amazon Cognito user pools mints and signs
//! the token the call presents, Amazon DynamoDB holds the table the template's request object
//! addresses, and AppSync publishes the endpoint, renders the template and makes the call — so
//! this is the suite that can pin it. Every write a case makes through the endpoint is read
//! back through DynamoDB's OWN api, so what a field reports is what that service holds.
//!
//! The rows span what the rule generalises over: WHICH helper form the template marshals with
//! (the general converter and the typed `toStringJson` / `toNumberJson` / `toListJson` /
//! `toBooleanJson` / `toMapJson` siblings), the KIND of value marshalled (text, text
//! carrying the characters JSON escapes, the empty string, an integer, a negative number, a
//! list, a boolean and a map), where the value comes from (an argument of the call, a literal
//! of the template, and the identity the admitted token claims), and — for the refused rows —
//! what the call presents instead of a token the pool signed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The Region every call of this suite is made in.
const REGION: &str = "us-west-2";

/// The long-term key every SETUP call is signed with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The table the resolvers of this suite read and write, and its one key attribute.
const TABLE: &str = "notes";
const KEY_ATTRIBUTE: &str = "id";

/// The data source the resolvers resolve against.
const DATA_SOURCE: &str = "notesTable";

/// The `version` every request object of a VTL mapping template states, as the documented
/// DynamoDB resolver templates do.
const REQUEST_VERSION: &str = "2017-02-28";

/// The user this suite signs in, and a password satisfying a default pool's policy. Written
/// for this test and used nowhere else: the pool it is set in lives only in this test's own
/// in-memory engine.
const USERNAME: &str = "alice";
const PASSWORD: &str = "Passw0rd!2024";

/// The label the `addNote` template marshals from a LITERAL of its own, which no argument of
/// the call carries.
const LABEL: &str = "note";

/// The refusal an unauthorized call on this data plane earns, and what a caller presenting a
/// value the user-pool mode cannot READ as a token is told instead. The service's own copies
/// are `service::event_api::UNAUTHORIZED_ERROR_TYPE` / `…_MESSAGE` and
/// `service::graphql_api::user_pool::UNREADABLE_TOKEN_MESSAGE`.
const UNAUTHORIZED_STATUS: u16 = 401;
const UNAUTHORIZED_ERROR_TYPE: &str = "UnauthorizedException";
const UNAUTHORIZED_MESSAGE: &str = "You are not authorized to make this call.";
const UNREADABLE_TOKEN_MESSAGE: &str = "Unable to parse JWT token.";

/// How long a poll loop waits for a state the clock owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches
/// a transition.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a case waits for one answer from the API's own socket. Generous for the same
/// reason: a loaded host answers later, and nothing here is about how fast it answers.
const ANSWER_DEADLINE: Duration = Duration::from_secs(120);

/// The schema every API of this suite is created with: the note the table holds, the mutation
/// that writes one and the query that reads it back.
const SCHEMA: &str = "\
type Note {\n  id: ID!\n  text: String\n  owner: String\n  label: String\n  \
count: Int\n  tags: [String]\n  active: Boolean\n  meta: AWSJSON\n}\n\
\n\
type Query {\n  getNote(id: ID!): Note\n}\n\
\n\
type Mutation {\n  \
addNote(id: ID!, text: String!): Note\n  \
addDetailedNote(id: ID!, text: String!, count: Int!, tags: [String]!, active: Boolean!): Note\n\
}\n";

/// The three services this rule spans, over ONE engine.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Request authentication is off in this target: a signed setup call would otherwise need a
    // real signature. The GraphQL calls under test present a token the pool really minted, and
    // that is what the API's own mode decides them by.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice
            ::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential is `access_key_id`.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One AppSync control-plane call (restJson1), signed with the ambient key.
async fn appsync(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Vec::new, |body| {
        serde_json::to_vec(&body).expect("the test body serializes")
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://appsync.{REGION}.amazonaws.com{path}"))
        .header("host", format!("appsync.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(AMBIENT_KEY, "appsync"))
        .body(Bytes::from(payload))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The same, asserting the call was served.
async fn appsync_ok(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let (status, answered) = appsync(router, method, path, body).await;
    assert_eq!(
        status, 200,
        "setup: {method} {path} expected 200, got {status} {answered}"
    );
    answered
}

/// One Amazon DynamoDB call over its own `awsJson1_0` wire, asserting it was served.
async fn dynamodb_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization(AMBIENT_KEY, "dynamodb"))
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

/// One Amazon Cognito user pools call (awsJson1_1), asserting it was served.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://cognito-idp.{REGION}.amazonaws.com/"))
        .header("host", format!("cognito-idp.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        )
        .header("authorization", authorization(AMBIENT_KEY, "cognito-idp"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "setup: cognito-idp {operation} expected 200, got {status} {parsed}"
    );
    parsed
}

/// The table this suite's resolvers resolve against, created through DynamoDB's own API and
/// waited for until that service first reports it `ACTIVE`.
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

/// What the table holds for `id`, read through DynamoDB's OWN api — so it says what that
/// service holds rather than what the endpoint answered. `None` for a key holding no item.
async fn held(router: &MultiServiceRouter, id: &str) -> Option<Value> {
    let read = dynamodb_ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {KEY_ATTRIBUTE: {"S": id}}, "ConsistentRead": true}),
    )
    .await;
    read.get("Item").filter(|item| !item.is_null()).cloned()
}

/// One pool with one app client and one confirmed user, signed in: the pool's id and the
/// identity token of that sign-in.
async fn pool_with_signed_in_user(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let pool = cognito(router, "CreateUserPool", json!({"PoolName": name})).await;
    let pool_id = pool["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateUserPool must mint an id, got {pool}"))
        .to_string();
    let client = cognito(
        router,
        "CreateUserPoolClient",
        json!({
            "UserPoolId": pool_id,
            "ClientName": format!("{name}-client"),
            // The flow the sign-in below uses, plus the refresh flow AWS requires beside any
            // other `ALLOW_` flow.
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateUserPoolClient must mint an id, got {client}"))
        .to_string();
    cognito(
        router,
        "AdminCreateUser",
        json!({"UserPoolId": pool_id, "Username": USERNAME}),
    )
    .await;
    cognito(
        router,
        "AdminSetUserPassword",
        json!({
            "UserPoolId": pool_id,
            "Username": USERNAME,
            "Password": PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    let signed_in = cognito(
        router,
        "AdminInitiateAuth",
        json!({
            "UserPoolId": pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": {"USERNAME": USERNAME, "PASSWORD": PASSWORD},
        }),
    )
    .await;
    let id_token = signed_in["AuthenticationResult"]["IdToken"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the sign-in must mint an IdToken, got {signed_in}"))
        .to_string();
    (pool_id, id_token)
}

/// One created GraphQL API: where it is served.
struct Api {
    /// The URL the API publishes for its `GRAPHQL` endpoint, which is what a client POSTs to.
    graphql_uri: String,
}

/// Create the API in the user-pool mode over `user_pool_id`, install [`SCHEMA`], attach the
/// DynamoDB data source and the three VTL resolvers of this suite.
async fn create_api(router: &MultiServiceRouter, name: &str, user_pool_id: &str) -> Api {
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({
            "name": name,
            "authenticationType": "AMAZON_COGNITO_USER_POOLS",
            "userPoolConfig": {
                "userPoolId": user_pool_id,
                "awsRegion": REGION,
                "defaultAction": "ALLOW",
            },
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
        "setup: uris.GRAPHQL must be a URL this emulator serves, got {graphql_uri:?}"
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
                "arn:aws:iam::{}:role/appsync-notes-datasource",
                engine::EMULATOR_ACCOUNT_ID
            ),
            "dynamodbConfig": {"tableName": TABLE, "awsRegion": REGION},
        })),
    )
    .await;
    for (type_name, field, request) in [
        ("Mutation", "addNote", add_note_template()),
        ("Mutation", "addDetailedNote", add_detailed_note_template()),
        ("Query", "getNote", get_note_template()),
    ] {
        appsync_ok(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
            Some(json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": DATA_SOURCE,
                "requestMappingTemplate": request,
                "responseMappingTemplate": "$util.toJson($ctx.result)",
            })),
        )
        .await;
    }
    Api { graphql_uri }
}

/// The `PutItem` template of `addNote`: the key and the text come from the call's arguments,
/// the owner from the IDENTITY the admitted token claims, and the label from a literal of the
/// template itself — each marshalled with the general converter's serialized sibling, which is
/// what lets it be inlined into the request object.
fn add_note_template() -> String {
    format!(
        "{{\n  \"version\": \"{REQUEST_VERSION}\",\n  \"operation\": \"PutItem\",\n  \
         \"key\": {{\"id\": $util.dynamodb.toDynamoDBJson($ctx.args.id)}},\n  \
         \"attributeValues\": {{\n    \
         \"text\": $util.dynamodb.toDynamoDBJson($ctx.args.text),\n    \
         \"owner\": $util.dynamodb.toDynamoDBJson($ctx.identity.username),\n    \
         \"label\": $util.dynamodb.toStringJson(\"{LABEL}\")\n  }}\n}}"
    )
}

/// The `PutItem` template of `addDetailedNote`: one TYPED helper per attribute type, and the
/// map helper over a map the template composed itself.
fn add_detailed_note_template() -> String {
    format!(
        "#set($meta = {{\"by\": $ctx.identity.username, \"label\": \"{LABEL}\"}})\n\
         {{\n  \"version\": \"{REQUEST_VERSION}\",\n  \"operation\": \"PutItem\",\n  \
         \"key\": {{\"id\": $util.dynamodb.toStringJson($ctx.args.id)}},\n  \
         \"attributeValues\": {{\n    \
         \"text\": $util.dynamodb.toStringJson($ctx.args.text),\n    \
         \"owner\": $util.dynamodb.toStringJson($ctx.identity.username),\n    \
         \"label\": $util.dynamodb.toStringJson(\"{LABEL}\"),\n    \
         \"count\": $util.dynamodb.toNumberJson($ctx.args.count),\n    \
         \"tags\": $util.dynamodb.toListJson($ctx.args.tags),\n    \
         \"active\": $util.dynamodb.toBooleanJson($ctx.args.active),\n    \
         \"meta\": $util.dynamodb.toMapJson($meta)\n  }}\n}}"
    )
}

/// The `GetItem` template of `getNote`, which marshals the key the call names.
fn get_note_template() -> String {
    format!(
        "{{\n  \"version\": \"{REQUEST_VERSION}\",\n  \"operation\": \"GetItem\",\n  \
         \"key\": {{\"id\": $util.dynamodb.toDynamoDBJson($ctx.args.id)}}\n}}"
    )
}

/// POST one GraphQL document to the URL the API PUBLISHED, as a client does: the host, the
/// port and the path all come from `uris.GRAPHQL` rather than from anything this suite knows.
async fn post_published(
    case: &str,
    api: &Api,
    body: &Value,
    fields: &[(&str, &str)],
) -> (u16, Value) {
    let (authority, path) = published_parts(case, &api.graphql_uri);
    let port: u16 = authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: {authority:?} must name a port"));
    let mut stream = tokio::time::timeout(ANSWER_DEADLINE, TcpStream::connect(("127.0.0.1", port)))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: {authority:?} must accept a connection"))
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {authority:?} must accept a connection, got {error}")
        });
    let payload = body.to_string();
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nhost: {authority}\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n",
        payload.len()
    );
    for (field, value) in fields {
        request.push_str(&format!("{field}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&payload);
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request must be written: {error}"));
    let mut answered = Vec::new();
    tokio::time::timeout(ANSWER_DEADLINE, stream.read_to_end(&mut answered))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the endpoint must answer"))
        .unwrap_or_else(|error| panic!("case {case:?}: the endpoint must answer: {error}"));
    let text = String::from_utf8_lossy(&answered).to_string();
    let (head, payload) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("case {case:?}: expected an HTTP response, got {text:?}"));
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a status line, got {head:?}"));
    let document = serde_json::from_str(payload).unwrap_or(Value::Null);
    (status, document)
}

/// The authority and the path of a published URL — what a client dials and what it asks for.
fn published_parts(case: &str, uri: &str) -> (String, String) {
    let rest = uri
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("case {case:?}: {uri:?} must be an absolute URL"));
    match rest.split_once('/') {
        Some((authority, path)) => (authority.to_string(), format!("/{path}")),
        None => (rest.to_string(), "/".to_string()),
    }
}

#[tokio::test]
async fn a_vtl_template_marshalling_with_the_dynamodb_helpers_writes_the_item_it_renders() {
    let router = build_router();
    create_table(&router).await;
    let (pool_id, id_token) = pool_with_signed_in_user(&router, "notes-pool").await;
    let api = create_api(&router, "notes-app", &pool_id).await;

    /// One row: the note the mutation writes, and the attribute values the helpers have to
    /// render its text as.
    struct Row<'a> {
        case: &'a str,
        id: &'a str,
        text: &'a str,
    }
    // The rows differ along the KIND of text marshalled, which is what the general converter
    // picks the attribute type from: plain text, text carrying the characters JSON escapes,
    // the empty string, text that READS as a number, and text carrying characters outside
    // ASCII.
    let rows: &[Row] = &[
        Row {
            case: "a plain note",
            id: "note-1",
            text: "buy milk",
        },
        Row {
            case: "text carrying the characters JSON escapes",
            id: "note-2",
            text: "she said \"hi\" \\ then left\nnext line\ttabbed",
        },
        Row {
            case: "the empty string",
            id: "note-3",
            text: "",
        },
        Row {
            case: "text that reads as a number",
            id: "note-4",
            text: "42",
        },
        Row {
            case: "text outside ASCII",
            id: "note-5",
            text: "caffè — ☕",
        },
    ];

    for row in rows {
        let case = row.case;
        let (status, answered) = post_published(
            case,
            &api,
            &json!({
                "query": "mutation Add($id: ID!, $text: String!) { \
                          addNote(id: $id, text: $text) { id text owner label } }",
                "variables": {"id": row.id, "text": row.text},
            }),
            &[("authorization", &id_token)],
        )
        .await;

        // The answer is the field's own value and carries NO errors member at all: a template
        // whose helpers resolved reports nothing to the client.
        assert_eq!(
            (status, answered.get("errors")),
            (200, None),
            "case {case:?}: a resolved template answers 200 with no errors, got {status} and \
             {answered}"
        );
        assert_eq!(
            answered["data"]["addNote"],
            json!({
                "id": row.id,
                "text": row.text,
                // The owner is the identity the ADMITTED token claims, which the template
                // marshalled: no argument of the call carries it.
                "owner": USERNAME,
                "label": LABEL,
            }),
            "case {case:?}: expected the note the template wrote, got {answered}"
        );

        // And the item is in the table, under the attribute types the helpers rendered.
        let item = held(&router, row.id).await;
        assert_eq!(
            item,
            Some(json!({
                KEY_ATTRIBUTE: {"S": row.id},
                "text": {"S": row.text},
                "owner": {"S": USERNAME},
                "label": {"S": LABEL},
            })),
            "case {case:?}: expected DynamoDB to hold the marshalled item, got {item:?}"
        );

        // Read back through the API's own query, whose template marshals the same key.
        let (status, read) = post_published(
            case,
            &api,
            &json!({
                "query": "query Get($id: ID!) { getNote(id: $id) { id text owner label } }",
                "variables": {"id": row.id},
            }),
            &[("authorization", &id_token)],
        )
        .await;
        assert_eq!(
            (status, read.get("errors")),
            (200, None),
            "case {case:?}: the read is served with no errors, got {status} and {read}"
        );
        assert_eq!(
            read["data"]["getNote"], answered["data"]["addNote"],
            "case {case:?}: the note read back must be the one written, got {read}"
        );
    }
}

#[tokio::test]
async fn each_typed_helper_renders_the_attribute_type_it_names() {
    let router = build_router();
    create_table(&router).await;
    let (pool_id, id_token) = pool_with_signed_in_user(&router, "typed-pool").await;
    let api = create_api(&router, "typed-notes", &pool_id).await;

    /// One row: the values the typed helpers marshal, and the number attribute the number
    /// helper has to render.
    struct Row<'a> {
        case: &'a str,
        id: &'a str,
        count: i64,
        tags: Vec<&'a str>,
        active: bool,
    }
    // The rows differ along the values the typed helpers are handed: a positive and a negative
    // count, a list of several elements and an empty one, and both booleans.
    let rows: &[Row] = &[
        Row {
            case: "a positive count, two tags and a true flag",
            id: "typed-1",
            count: 7,
            tags: vec!["home", "errand"],
            active: true,
        },
        Row {
            case: "a negative count, no tag at all and a false flag",
            id: "typed-2",
            count: -3,
            tags: Vec::new(),
            active: false,
        },
        Row {
            case: "a zero count and one tag",
            id: "typed-3",
            count: 0,
            tags: vec!["single"],
            active: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let (status, answered) = post_published(
            case,
            &api,
            &json!({
                "query": "mutation Add($id: ID!, $text: String!, $count: Int!, \
                          $tags: [String]!, $active: Boolean!) { \
                          addDetailedNote(id: $id, text: $text, count: $count, tags: $tags, \
                          active: $active) { id text count tags active } }",
                "variables": {
                    "id": row.id,
                    "text": "detailed",
                    "count": row.count,
                    "tags": row.tags,
                    "active": row.active,
                },
            }),
            &[("authorization", &id_token)],
        )
        .await;
        assert_eq!(
            (status, answered.get("errors")),
            (200, None),
            "case {case:?}: a resolved template answers 200 with no errors, got {status} and \
             {answered}"
        );
        assert_eq!(
            answered["data"]["addDetailedNote"],
            json!({
                "id": row.id,
                "text": "detailed",
                "count": row.count,
                "tags": row.tags,
                "active": row.active,
            }),
            "case {case:?}: expected the note the typed helpers wrote, got {answered}"
        );

        // Each helper rendered the attribute DynamoDB holds that kind of value as: the number
        // under `N`, the list under `L` with every element marshalled through the general
        // converter, the boolean under `BOOL` and the composed map under `M`.
        let item = held(&router, row.id).await;
        assert_eq!(
            item,
            Some(json!({
                KEY_ATTRIBUTE: {"S": row.id},
                "text": {"S": "detailed"},
                "owner": {"S": USERNAME},
                "label": {"S": LABEL},
                "count": {"N": row.count.to_string()},
                "tags": {"L": row.tags.iter().map(|tag| json!({"S": tag})).collect::<Vec<Value>>()},
                "active": {"BOOL": row.active},
                "meta": {"M": {"by": {"S": USERNAME}, "label": {"S": LABEL}}},
            })),
            "case {case:?}: expected DynamoDB to hold one attribute per typed helper, got \
             {item:?}"
        );
    }
}

#[tokio::test]
async fn a_call_the_api_refuses_reaches_no_template_and_writes_nothing() {
    let router = build_router();
    create_table(&router).await;
    let (pool_id, id_token) = pool_with_signed_in_user(&router, "refused-pool").await;
    // A second pool, whose tokens this API accepts none of.
    let (_other_pool_id, other_token) = pool_with_signed_in_user(&router, "stranger-pool").await;
    let api = create_api(&router, "refused-notes", &pool_id).await;

    // The rows differ along WHAT the call presents instead of a token this API's pool signed:
    // nothing at all, a value no reader can take for a token, and a token another pool signed.
    // The sentence each is refused with says how far the decision got.
    let rows: &[(&str, Option<&str>, &str)] = &[
        ("nothing presented at all", None, UNAUTHORIZED_MESSAGE),
        (
            "a value that is no token at all",
            Some("not-a-token-at-all"),
            UNREADABLE_TOKEN_MESSAGE,
        ),
        (
            "a readable token another user pool signed",
            Some(other_token.as_str()),
            UNAUTHORIZED_MESSAGE,
        ),
    ];
    for (case, presented, message) in rows {
        let fields: Vec<(&str, &str)> = presented
            .map(|value| vec![("authorization", value)])
            .unwrap_or_default();
        let (status, answered) = post_published(
            case,
            &api,
            &json!({
                "query": "mutation Add($id: ID!, $text: String!) { \
                          addNote(id: $id, text: $text) { id } }",
                "variables": {"id": "refused-1", "text": "never written"},
            }),
            &fields,
        )
        .await;
        assert_eq!(
            (
                status,
                answered["errors"][0]["errorType"].as_str(),
                answered["errors"][0]["message"].as_str()
            ),
            (
                UNAUTHORIZED_STATUS,
                Some(UNAUTHORIZED_ERROR_TYPE),
                Some(*message)
            ),
            "case {case:?}: expected ({UNAUTHORIZED_STATUS}, {UNAUTHORIZED_ERROR_TYPE:?}, \
             {message:?}), got {status} and {answered}"
        );
        let item = held(&router, "refused-1").await;
        assert_eq!(
            item, None,
            "case {case:?}: a refused call writes nothing, got {item:?}"
        );
    }

    // And the same mutation under the pool's own token writes the item, so the rows above
    // failed on what they presented rather than on the template.
    let (status, answered) = post_published(
        "the pool's own identity token",
        &api,
        &json!({
            "query": "mutation Add($id: ID!, $text: String!) { \
                      addNote(id: $id, text: $text) { id } }",
            "variables": {"id": "refused-1", "text": "written"},
        }),
        &[("authorization", &id_token)],
    )
    .await;
    assert_eq!(
        (status, answered.get("errors")),
        (200, None),
        "the admitted call must be served, got {status} and {answered}"
    );
    assert!(
        held(&router, "refused-1").await.is_some(),
        "the admitted call must write the item the template rendered"
    );
}
