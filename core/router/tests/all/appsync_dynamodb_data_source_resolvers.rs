// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose resolvers resolve against an `AMAZON_DYNAMODB` data source,
//! served at the address the API publishes.
//!
//! RULE: a resolver step whose data source is of the `AMAZON_DYNAMODB` type CALLS Amazon
//! DynamoDB with the request its request half returned — the `operation` the request names,
//! against the table the data source was configured with — and the value that service
//! answered, unmarshalled from the attribute values it holds, is what the step's response
//! half is handed. The permissions that admit the call are weighed FIRST and the call is not
//! made when they do not admit it: under the CALLER's own credentials where the data source
//! uses them (`useCallerCredentials`), and under the role the data source names otherwise; a
//! refusal — one the permissions withheld, and one the table itself answered — is reported as
//! a GraphQL error of the field carrying the `DynamoDB:<error class>` type, with nothing read
//! and nothing written.
//!
//! Five services have to agree for that to hold — Cognito Identity vends the session, IAM
//! holds the roles and their policies, STS records the session and names its principal,
//! DynamoDB holds the table, and AppSync publishes the endpoint, runs the resolver and takes
//! the authorization decision — so this is the suite that can pin it. Every write a case makes
//! through the endpoint is read back through DynamoDB's OWN api, so what a field reports is
//! what that service holds.
//!
//! The rows span what the rule generalises over: the operation the request object names
//! (`GetItem`, `PutItem`, `UpdateItem`, `DeleteItem`, `Query` and `Scan`), the form the
//! resolver's definition takes (code importing the utilities module and code using the names
//! the runtime binds), whose credentials admit the call (the data source's role and the
//! caller's own), what the policy in force on the caller says about the DynamoDB action (a
//! grant naming it, a grant naming another action only, an explicit deny over a wildcard
//! grant, and no grant at all), and the material the call presents (a session an identity pool
//! vended, and material of another provider, which names no principal to act as).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The region every request here signs for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which every SETUP call is signed with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The identity-provider principal an identity pool's exchange is made under.
const COGNITO_IDENTITY_PRINCIPAL: &str = "cognito-identity.amazonaws.com";

/// The service principal a data source's role is assumed by.
const APPSYNC_PRINCIPAL: &str = "appsync.amazonaws.com";

/// The table every resolver of this suite resolves against, and its one key attribute.
const TABLE: &str = "appsync-items";
const KEY_ATTRIBUTE: &str = "id";

/// The data sources an API of this suite holds: one reached under the data source's own role,
/// one reached under the credentials of whoever made the GraphQL call.
const ROLE_DATA_SOURCE: &str = "ddb";
const CALLER_DATA_SOURCE: &str = "ddbCaller";

/// The path a GraphQL API's `GRAPHQL` endpoint serves.
const GRAPHQL_PATH: &str = "/graphql";

/// How long a poll loop waits for a state the clock owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches
/// a transition.
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a case waits for one answer from the API's own socket.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

/// The schema every API of this suite is created with: the item the table holds, the page a
/// selection answers, and one field per operation a row drives.
const SCHEMA: &str = "\
type Item {\n  id: String\n  title: String\n  owner: String\n  count: Int\n}\n\
\n\
type ItemPage {\n  items: [Item]\n  nextToken: String\n  scannedCount: Int\n}\n\
\n\
type Query {\n  \
  item(id: String): Item\n  \
  callerItem(id: String): Item\n  \
  itemsWithId(id: String): ItemPage\n  \
  allItems: ItemPage\n}\n\
\n\
type Mutation {\n  \
  putItem(id: String, title: String, owner: String, count: Int): Item\n  \
  updateItem(id: String, title: String): Item\n  \
  deleteItem(id: String): Item\n  \
  insertItem(id: String, title: String): Item\n  \
  callerUpdateItem(id: String, title: String): Item\n}\n";

/// The five services this rule spans, over ONE engine.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Request authentication is off in this target: a signed row would otherwise need a real
    // signature. What a graded call pins is what the endpoint ANSWERS for the identity the
    // access key id resolves to, never signature verification.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cognitoidentity_awscognitoidentityservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential is `access_key_id` — the key a service
/// reads the caller's identity from.
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

/// One Amazon DynamoDB call over its own `awsJson1_0` wire, signed with the ambient key.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
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
    (status, parsed)
}

/// The same, asserting the call was served.
async fn dynamodb_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, answered) = dynamodb(router, operation, body).await;
    assert_eq!(
        status, 200,
        "setup: DynamoDB {operation} expected 200, got {status} {answered}"
    );
    answered
}

/// One Amazon Cognito Identity call (awsJson1_1), signed with the ambient key.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cognito-identity.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSCognitoIdentityService.{operation}"))
        .header(
            "authorization",
            authorization(AMBIENT_KEY, "cognito-identity"),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the test body serializes"),
        ))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "setup: cognito-identity {operation} expected 200, got {status} {parsed}"
    );
    parsed
}

/// One IAM call (awsQuery), signed with the ambient key, answering the response body.
async fn iam(router: &MultiServiceRouter, body: String) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(AMBIENT_KEY, "iam"))
        .body(Bytes::from(body.clone()))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: iam {body} expected 200, got {status} {text}"
    );
    text
}

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// The table every row of this suite reads or writes, created through DynamoDB's own API and
/// waited for until that service first reports it `ACTIVE` — a table that is not is one no
/// call lands on.
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
        let described =
            dynamodb_ok(router, "DescribeTable", json!({"TableName": TABLE})).await;
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

/// Write one item into the table through DynamoDB's own API, as a row's starting point.
async fn put_held(router: &MultiServiceRouter, id: &str, title: &str, count: i64) {
    dynamodb_ok(
        router,
        "PutItem",
        json!({
            "TableName": TABLE,
            "Item": {
                KEY_ATTRIBUTE: {"S": id},
                "title": {"S": title},
                "owner": {"S": "ada"},
                "count": {"N": count.to_string()},
            },
        }),
    )
    .await;
}

/// What the table holds for `id`, read through DynamoDB's OWN api — so it says what that
/// service holds rather than what the endpoint answered. `None` for a key holding no item.
async fn held(router: &MultiServiceRouter, id: &str) -> Option<Value> {
    let read = dynamodb_ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {KEY_ATTRIBUTE: {"S": id}}}),
    )
    .await;
    read.get("Item").filter(|item| !item.is_null()).cloned()
}

/// One created GraphQL API: what it is addressed by, and where it is served.
struct Api {
    api_id: String,
    arn: String,
    /// The URL the API publishes for its `GRAPHQL` endpoint, which is what a client POSTs to.
    graphql_uri: String,
}

/// Create the API, install [`SCHEMA`], and attach the two DynamoDB data sources — the one
/// reached under `role_arn` and the one reached under the caller's own credentials.
async fn create_api(router: &MultiServiceRouter, name: &str, role_arn: &str) -> Api {
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": name, "authenticationType": "AWS_IAM"})),
    )
    .await;
    let api = &created["graphqlApi"];
    let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
    let arn = api["arn"].as_str().unwrap_or_default().to_string();
    let graphql_uri = api["uris"]["GRAPHQL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        graphql_uri.ends_with(GRAPHQL_PATH) && graphql_uri.contains("127.0.0.1:"),
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
    for (name, caller_credentials) in [(ROLE_DATA_SOURCE, false), (CALLER_DATA_SOURCE, true)] {
        appsync_ok(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/datasources"),
            Some(json!({
                "name": name,
                "type": "AMAZON_DYNAMODB",
                "serviceRoleArn": role_arn,
                "dynamodbConfig": {
                    "tableName": TABLE,
                    "awsRegion": REGION,
                    "useCallerCredentials": caller_credentials,
                },
            })),
        )
        .await;
    }
    Api {
        api_id,
        arn,
        graphql_uri,
    }
}

/// Attach a UNIT resolver defined by APPSYNC_JS `code` to `field` of `type_name`.
async fn create_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    type_name: &str,
    field: &str,
    data_source: &str,
    code: &str,
) {
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
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

/// POST one GraphQL document to the URL the API PUBLISHED, as a client does: the host, the
/// port and the path all come from `uris.GRAPHQL` rather than from anything this suite knows.
async fn post_published(
    case: &str,
    api: &Api,
    body: &Value,
    fields: &[(&str, String)],
) -> (u16, Value) {
    let (authority, path) = published_parts(case, &api.graphql_uri);
    let port: u16 = authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: {authority:?} must name a port"));
    let mut stream =
        tokio::time::timeout(ANSWER_DEADLINE, TcpStream::connect(("127.0.0.1", port)))
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

/// The trust policy a role assumed by `principal` carries, written compactly and free of the
/// characters an `application/x-www-form-urlencoded` body treats specially (`&`, `=`, `+`,
/// `%`, space), so it needs no encoding.
fn trust(principal: &str, federated: bool, pool_id: Option<&str>) -> String {
    let (element, action) = if federated {
        ("Federated", "sts:AssumeRoleWithWebIdentity")
    } else {
        ("Service", "sts:AssumeRole")
    };
    let condition = pool_id.map_or_else(String::new, |pool| {
        format!(
            ",\"Condition\":{{\"StringEquals\":{{\"{COGNITO_IDENTITY_PRINCIPAL}:aud\":\
             \"{pool}\"}}}}"
        )
    });
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"{element}\":\"{principal}\"}},\"Action\":\"{action}\"{condition}}}]}}"
    )
}

/// One identity-based policy: an `Allow` of each `(actions, resource)` pair and a `Deny` of
/// each, written with none of the characters a form body treats specially.
fn policy(allowed: &[(&str, String)], denied: &[(&str, String)]) -> String {
    let statements: Vec<String> = allowed
        .iter()
        .map(|(actions, resource)| (r#""Allow""#, actions, resource))
        .chain(
            denied
                .iter()
                .map(|(actions, resource)| (r#""Deny""#, actions, resource)),
        )
        .map(|(effect, actions, resource)| {
            format!(
                "{{\"Effect\":{effect},\"Action\":[{actions}],\"Resource\":\"{resource}\"}}"
            )
        })
        .collect();
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{}]}}",
        statements.join(",")
    )
}

/// Create the role `name` with `trust_policy` and `document`, answering its ARN.
async fn create_role(
    router: &MultiServiceRouter,
    name: &str,
    trust_policy: &str,
    document: &str,
) -> String {
    let created = iam(
        router,
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}\
             &AssumeRolePolicyDocument={trust_policy}"
        ),
    )
    .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({name}) reports no <Arn>: {created}"));
    put_role_policy(router, name, document).await;
    arn
}

/// Put `document` on `role_name` as its one inline policy, replacing whatever the last row
/// put there.
async fn put_role_policy(router: &MultiServiceRouter, role_name: &str, document: &str) {
    iam(
        router,
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={role_name}\
             &PolicyName=data-source-access&PolicyDocument={document}"
        ),
    )
    .await;
}

/// Vend a session for a GUEST identity of `pool_id`, answering the access key id a call signs
/// with.
async fn vend_guest_session(router: &MultiServiceRouter, pool_id: &str) -> String {
    let identity = cognito(router, "GetId", &json!({"IdentityPoolId": pool_id})).await;
    let identity_id = identity["IdentityId"].as_str().unwrap_or_default();
    let vended = cognito(
        router,
        "GetCredentialsForIdentity",
        &json!({"IdentityId": identity_id}),
    )
    .await;
    vended["Credentials"]["AccessKeyId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The ARN of the table every policy of this suite is written about.
fn table_arn() -> String {
    format!(
        "arn:aws:dynamodb:{REGION}:{}:table/{TABLE}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The material a row that signs with no AWS credential presents: a token of another
/// authorization provider, which the API admits and which names no principal to act as.
const OTHER_PROVIDERS_TOKEN: &str = "a-token-of-another-provider";

/// The response half every resolver of this suite carries: a call the table REFUSED is
/// reported as the field's own error, carrying the class the refusal is of, and anything else
/// is the value the call answered. This is the handling the AppSync JavaScript resolver
/// reference writes.
const RESPONSE_FUNCTION: &str = "export function response(ctx) {\n  \
                                 if (ctx.error) {\n    \
                                 util.error(ctx.error.message, ctx.error.type);\n  }\n  \
                                 return ctx.result;\n}\n";

/// One resolver's definition: the request half, and the shared response half. The utilities
/// module is IMPORTED, which is one of the two ways a resolver reaches `util`.
fn imported_code(request: &str) -> String {
    format!("import {{ util }} from '@aws-appsync/utils';\n{request}\n{RESPONSE_FUNCTION}")
}

/// The same with no import at all, which is the other way: the runtime binds `util` for the
/// code it runs.
fn bare_code(request: &str) -> String {
    format!("{request}\n{RESPONSE_FUNCTION}")
}

#[tokio::test]
async fn a_dynamodb_data_source_resolver_calls_the_table_for_each_operation() {
    let router = build_router();
    create_table(&router).await;
    let role = create_role(
        &router,
        "appsync-ddb-role",
        &trust(APPSYNC_PRINCIPAL, false, None),
        &policy(&[(r#""dynamodb:*""#, table_arn())], &[]),
    )
    .await;
    let api = create_api(&router, "ddb-operations", &role).await;
    // The item a read, an update, a delete and a selection start from.
    put_held(&router, "read-me", "held", 7).await;
    put_held(&router, "update-me", "before", 1).await;
    put_held(&router, "delete-me", "doomed", 1).await;
    put_held(&router, "taken", "taken", 1).await;

    for (type_name, field, request) in [
        (
            "Query",
            "item",
            "export function request(ctx) {\n  \
             return {operation: 'GetItem', \
             key: {id: util.dynamodb.toDynamoDB(ctx.args.id)}};\n}",
        ),
        (
            "Query",
            "itemsWithId",
            "export function request(ctx) {\n  \
             return {operation: 'Query', limit: 10, \
             query: {expression: '#k = :k', \
             expressionNames: {'#k': 'id'}, \
             expressionValues: {':k': util.dynamodb.toDynamoDB(ctx.args.id)}}};\n}",
        ),
        (
            "Query",
            "allItems",
            "export function request(ctx) {\n  return {operation: 'Scan'};\n}",
        ),
        (
            "Mutation",
            "putItem",
            "export function request(ctx) {\n  \
             return {operation: 'PutItem', \
             key: {id: util.dynamodb.toDynamoDB(ctx.args.id)}, \
             attributeValues: util.dynamodb.toMapValues(\
             {title: ctx.args.title, owner: ctx.args.owner, count: ctx.args.count})};\n}",
        ),
        (
            "Mutation",
            "updateItem",
            "export function request(ctx) {\n  \
             return {operation: 'UpdateItem', \
             key: {id: util.dynamodb.toDynamoDB(ctx.args.id)}, \
             update: {expression: 'SET #t = :t', \
             expressionNames: {'#t': 'title'}, \
             expressionValues: {':t': util.dynamodb.toDynamoDB(ctx.args.title)}}};\n}",
        ),
        (
            "Mutation",
            "deleteItem",
            "export function request(ctx) {\n  \
             return {operation: 'DeleteItem', \
             key: {id: util.dynamodb.toDynamoDB(ctx.args.id)}};\n}",
        ),
        (
            "Mutation",
            "insertItem",
            "export function request(ctx) {\n  \
             return {operation: 'PutItem', \
             key: {id: util.dynamodb.toDynamoDB(ctx.args.id)}, \
             attributeValues: util.dynamodb.toMapValues({title: ctx.args.title}), \
             condition: {expression: 'attribute_not_exists(id)'}};\n}",
        ),
    ] {
        // The `Scan` row is the one written WITHOUT the import, so both ways of reaching the
        // runtime's helpers are exercised.
        let code = if field == "allItems" {
            bare_code(request)
        } else {
            imported_code(request)
        };
        create_resolver(&router, &api.api_id, type_name, field, ROLE_DATA_SOURCE, &code).await;
    }

    /// One row: the document it posts, the data the endpoint must answer, and what the table
    /// must hold for a key afterwards.
    struct Row {
        case: &'static str,
        query: &'static str,
        data: Value,
        /// The error the one reported entry must carry, for a row whose call the table
        /// refused.
        error: Option<(&'static str, &'static str)>,
        /// What DynamoDB's own reader must report for a key after the row ran: `Some(None)`
        /// for a key that must hold no item at all.
        held: Option<(&'static str, Option<Value>)>,
    }
    let rows: Vec<Row> = vec![
        Row {
            case: "a GetItem of an item the table holds",
            query: "query { item(id: \"read-me\") { id title count } }",
            data: json!({"item": {"id": "read-me", "title": "held", "count": 7}}),
            error: None,
            held: None,
        },
        Row {
            case: "a GetItem of a key the table holds no item for",
            query: "query { item(id: \"absent\") { id title } }",
            data: json!({"item": Value::Null}),
            error: None,
            held: Some(("absent", None)),
        },
        Row {
            case: "a PutItem of a new item",
            query: "mutation { putItem(id: \"written\", title: \"first\", owner: \"ada\", \
                    count: 3) { id title owner count } }",
            data: json!({
                "putItem": {"id": "written", "title": "first", "owner": "ada", "count": 3},
            }),
            error: None,
            held: Some((
                "written",
                Some(json!({
                    "id": {"S": "written"},
                    "title": {"S": "first"},
                    "owner": {"S": "ada"},
                    "count": {"N": "3"},
                })),
            )),
        },
        Row {
            case: "an UpdateItem of an item the table holds",
            query: "mutation { updateItem(id: \"update-me\", title: \"after\") \
                    { id title count } }",
            data: json!({"updateItem": {"id": "update-me", "title": "after", "count": 1}}),
            error: None,
            held: Some((
                "update-me",
                Some(json!({
                    "id": {"S": "update-me"},
                    "title": {"S": "after"},
                    "owner": {"S": "ada"},
                    "count": {"N": "1"},
                })),
            )),
        },
        Row {
            case: "a DeleteItem of an item the table holds",
            query: "mutation { deleteItem(id: \"delete-me\") { id title } }",
            data: json!({"deleteItem": {"id": "delete-me", "title": "doomed"}}),
            error: None,
            held: Some(("delete-me", None)),
        },
        Row {
            case: "a Query selecting by the table's key",
            query: "query { itemsWithId(id: \"read-me\") { items { id title } scannedCount } }",
            data: json!({
                "itemsWithId": {
                    "items": [{"id": "read-me", "title": "held"}],
                    "scannedCount": 1,
                },
            }),
            error: None,
            held: None,
        },
        Row {
            case: "a PutItem whose condition the table refuses",
            query: "mutation { insertItem(id: \"taken\", title: \"again\") { id title } }",
            data: json!({"insertItem": Value::Null}),
            error: Some((
                "DynamoDB:ConditionalCheckFailedException",
                "The conditional request failed",
            )),
            // The refused write left the item exactly as it was.
            held: Some((
                "taken",
                Some(json!({
                    "id": {"S": "taken"},
                    "title": {"S": "taken"},
                    "owner": {"S": "ada"},
                    "count": {"N": "1"},
                })),
            )),
        },
    ];

    for row in &rows {
        let (status, answered) = post_published(
            row.case,
            &api,
            &json!({"query": row.query}),
            &[("authorization", authorization(AMBIENT_KEY, "appsync"))],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the published endpoint must answer 200, got {status} and {answered}",
            row.case
        );
        assert_eq!(
            answered["data"], row.data,
            "case {:?}: expected data {}, got {answered}",
            row.case, row.data
        );
        match row.error {
            None => assert!(
                answered.get("errors").is_none(),
                "case {:?}: expected no errors, got {answered}",
                row.case
            ),
            Some((error_type, message)) => assert_eq!(
                (
                    answered["errors"][0]["errorType"].as_str(),
                    answered["errors"][0]["message"].as_str(),
                ),
                (Some(error_type), Some(message)),
                "case {:?}: expected the error the table answered, got {answered}",
                row.case
            ),
        }
        if let Some((id, expected)) = &row.held {
            let stored = held(&router, id).await;
            assert_eq!(
                &stored, expected,
                "case {:?}: expected the table to hold {expected:?} for {id:?}, got {stored:?}",
                row.case
            );
        }
    }

    // The Scan reads every item the table holds at this point, which is what the rows above
    // left there: one selection over the whole table rather than one key.
    let case = "a Scan of the whole table";
    let (status, scanned) = post_published(
        case,
        &api,
        &json!({"query": "query { allItems { items { id } scannedCount } }"}),
        &[("authorization", authorization(AMBIENT_KEY, "appsync"))],
    )
    .await;
    let mut ids: Vec<String> = scanned["data"]["allItems"]["items"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect();
    ids.sort();
    assert_eq!(
        (status, ids.as_slice()),
        (
            200,
            [
                "read-me".to_string(),
                "taken".to_string(),
                "update-me".to_string(),
                "written".to_string(),
            ]
            .as_slice()
        ),
        "case {case:?}: expected every item the table holds, got {scanned}"
    );
    assert_eq!(
        scanned["data"]["allItems"]["scannedCount"],
        json!(4),
        "case {case:?}: expected the count of the values it examined, got {scanned}"
    );
}

#[tokio::test]
async fn caller_credentials_decide_the_call_and_a_refused_one_changes_nothing() {
    let router = build_router();
    create_table(&router).await;
    let role = create_role(
        &router,
        "appsync-caller-ddb-role",
        &trust(APPSYNC_PRINCIPAL, false, None),
        &policy(&[(r#""dynamodb:*""#, table_arn())], &[]),
    )
    .await;
    let api = create_api(&router, "ddb-caller-credentials", &role).await;
    create_resolver(
        &router,
        &api.api_id,
        "Query",
        "callerItem",
        CALLER_DATA_SOURCE,
        &imported_code(
            "export function request(ctx) {\n  \
             return {operation: 'GetItem', \
             key: {id: util.dynamodb.toDynamoDB(ctx.args.id)}};\n}",
        ),
    )
    .await;
    create_resolver(
        &router,
        &api.api_id,
        "Mutation",
        "callerUpdateItem",
        CALLER_DATA_SOURCE,
        &imported_code(
            "export function request(ctx) {\n  \
             return {operation: 'UpdateItem', \
             key: {id: util.dynamodb.toDynamoDB(ctx.args.id)}, \
             update: {expression: 'SET #t = :t', \
             expressionNames: {'#t': 'title'}, \
             expressionValues: {':t': util.dynamodb.toDynamoDB(ctx.args.title)}}};\n}",
        ),
    )
    .await;
    put_held(&router, "caller-read", "held", 2).await;

    // The pool whose guest sessions the rows sign with, and the role every one of its
    // exchanges vends a session of.
    let pool = cognito(
        &router,
        "CreateIdentityPool",
        &json!({
            "IdentityPoolName": "caller-credentials-pool",
            "AllowUnauthenticatedIdentities": true,
        }),
    )
    .await;
    let pool_id = pool["IdentityPoolId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let guest_role_name = "caller-credentials-guest";
    let guest_role = create_role(
        &router,
        guest_role_name,
        &trust(COGNITO_IDENTITY_PRINCIPAL, true, Some(&pool_id)),
        &policy(&[(r#""appsync:GraphQL""#, format!("{}/*", api.arn))], &[]),
    )
    .await;
    cognito(
        &router,
        "SetIdentityPoolRoles",
        &json!({
            "IdentityPoolId": pool_id,
            "Roles": {"authenticated": guest_role, "unauthenticated": guest_role},
        }),
    )
    .await;
    let access_key_id = vend_guest_session(&router, &pool_id).await;
    let graphql = (r#""appsync:GraphQL""#, format!("{}/*", api.arn));

    /// One row: what the policy in force on the caller says about the DynamoDB action, the
    /// document the caller posts, and whether the call it asks for is made.
    struct Row {
        case: &'static str,
        allowed: Vec<(&'static str, String)>,
        denied: Vec<(&'static str, String)>,
        query: &'static str,
        field: &'static str,
        /// Whether the row presents a session the pool vended: material of ANOTHER provider
        /// names no principal at all, which such a data source has nothing to act as.
        signed: bool,
        /// What the refusal must state, for a row whose call was not made: the action the
        /// permissions withheld, or that no credentials of a caller were presented.
        refused: Option<&'static str>,
    }
    let rows: Vec<Row> = vec![
        Row {
            case: "a grant naming the DynamoDB action the resolver performs",
            allowed: vec![graphql.clone(), (r#""dynamodb:GetItem""#, table_arn())],
            denied: Vec::new(),
            query: "query { callerItem(id: \"caller-read\") { id title count } }",
            field: "callerItem",
            signed: true,
            refused: None,
        },
        Row {
            case: "a grant naming another DynamoDB action only",
            allowed: vec![graphql.clone(), (r#""dynamodb:PutItem""#, table_arn())],
            denied: Vec::new(),
            query: "query { callerItem(id: \"caller-read\") { id title } }",
            field: "callerItem",
            signed: true,
            refused: Some("dynamodb:GetItem"),
        },
        Row {
            case: "no grant of this service at all",
            allowed: vec![graphql.clone()],
            denied: Vec::new(),
            query: "query { callerItem(id: \"caller-read\") { id title } }",
            field: "callerItem",
            signed: true,
            refused: Some("dynamodb:GetItem"),
        },
        Row {
            case: "an explicit deny over a wildcard grant",
            allowed: vec![graphql.clone(), (r#""dynamodb:*""#, table_arn())],
            denied: vec![(r#""dynamodb:GetItem""#, table_arn())],
            query: "query { callerItem(id: \"caller-read\") { id title } }",
            field: "callerItem",
            signed: true,
            refused: Some("dynamodb:GetItem"),
        },
        Row {
            case: "a write the caller may not make",
            allowed: vec![graphql.clone(), (r#""dynamodb:GetItem""#, table_arn())],
            denied: Vec::new(),
            query: "mutation { callerUpdateItem(id: \"caller-read\", title: \"rewritten\") \
                    { id title } }",
            field: "callerUpdateItem",
            signed: true,
            refused: Some("dynamodb:UpdateItem"),
        },
        Row {
            case: "a write the caller may make",
            allowed: vec![graphql.clone(), (r#""dynamodb:UpdateItem""#, table_arn())],
            denied: Vec::new(),
            query: "mutation { callerUpdateItem(id: \"caller-read\", title: \"rewritten\") \
                    { id title } }",
            field: "callerUpdateItem",
            signed: true,
            refused: None,
        },
        Row {
            case: "material of another provider, which names no principal to act as",
            allowed: vec![graphql.clone(), (r#""dynamodb:*""#, table_arn())],
            denied: Vec::new(),
            query: "query { callerItem(id: \"caller-read\") { id title } }",
            field: "callerItem",
            signed: false,
            refused: Some("presented none"),
        },
    ];

    for row in &rows {
        put_role_policy(
            &router,
            guest_role_name,
            &policy(&row.allowed, &row.denied),
        )
        .await;
        let before = held(&router, "caller-read").await;
        let presented = if row.signed {
            authorization(&access_key_id, "appsync")
        } else {
            OTHER_PROVIDERS_TOKEN.to_string()
        };
        let (status, answered) = post_published(
            row.case,
            &api,
            &json!({"query": row.query}),
            &[("authorization", presented)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the published endpoint must answer 200, got {status} and {answered}",
            row.case
        );
        let Some(stated) = row.refused else {
            assert!(
                answered.get("errors").is_none(),
                "case {:?}: a call the caller may make must be served cleanly, got {answered}",
                row.case
            );
            assert_eq!(
                answered["data"][row.field]["id"].as_str(),
                Some("caller-read"),
                "case {:?}: expected the item the call answered, got {answered}",
                row.case
            );
            // A write the caller MAY make reaches the table: what the field answered is what
            // DynamoDB's own reader now holds, which is the mirror of the refused rows below.
            if let Some(title) = answered["data"][row.field]["title"].as_str() {
                let stored = held(&router, "caller-read").await;
                assert_eq!(
                    stored
                        .as_ref()
                        .and_then(|item| item["title"]["S"].as_str()),
                    Some(title),
                    "case {:?}: expected the table to hold the title the call answered, got \
                     {stored:?}",
                    row.case
                );
            }
            continue;
        };
        assert_eq!(
            answered["data"][row.field],
            Value::Null,
            "case {:?}: a field whose call was refused resolves to nothing, got {answered}",
            row.case
        );
        assert_eq!(
            answered["errors"][0]["errorType"].as_str(),
            // DynamoDB's model declares no error for an authorization refusal, so a client
            // raises it as that service's base exception — unlike the conditional-check
            // failure the other suite asserts, which the model does declare.
            Some("DynamoDB:DynamoDbException"),
            "case {:?}: expected the class of an unauthorized call, got {answered}",
            row.case
        );
        let message = answered["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // A refusal the permissions withheld names the SESSION the call was made as — the
        // caller's own credentials, not the data source's role — the action it asked for and
        // the table it addressed; a request that presented no credentials of a caller at all
        // is refused for that.
        let expected: Vec<String> = if row.signed {
            vec![
                format!("is not authorized to perform: {stated}"),
                format!("on resource: {}", table_arn()),
                format!("assumed-role/{guest_role_name}/"),
            ]
        } else {
            vec![stated.to_string()]
        };
        for expected in &expected {
            assert!(
                message.contains(expected.as_str()),
                "case {:?}: expected a refusal stating {expected:?}, got {message:?}",
                row.case
            );
        }
        // And the table is untouched: a call the permissions withheld reads and writes
        // nothing.
        let after = held(&router, "caller-read").await;
        assert_eq!(
            after, before,
            "case {:?}: expected the item to be unchanged, got {after:?} where it held \
             {before:?}",
            row.case
        );
    }
}
