// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose fields are resolved by an `HTTP` data source pointed at a
//! DEPLOYED API Gateway stage — end to end over the real `MultiServiceRouter`, with AppSync,
//! API Gateway and IAM mounted on one engine.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > A resolver step whose data source is of type `HTTP` MAKES the request its request half
//! > described against the endpoint the data source configures: the `method` it states, the
//! > `resourcePath` appended to that endpoint's own path, and the `params` it carries — its
//! > `query`, its `headers` and its `body`. The endpoint's own answer is what the response half
//! > is handed, as `statusCode`, `headers` and `body`, WHATEVER that status is: a refusal is an
//! > answer the field reports, not a failure of the step. An endpoint whose
//! > `authorizationConfig` names `AWS_IAM` has that request SIGNED for the service and the
//! > Region its `awsIamConfig` names, so an endpoint that requires a signature admits the call
//! > and names the principal it authenticated; a data source that configures no such
//! > authorization sends the request unsigned, and the same endpoint refuses it. A request the
//! > data source cannot act on is never made at all: the step reports it as an error of the
//! > definition that produced it, locating the member at fault.
//!
//! The rows span what that rule generalises over: the authorization the data source configures
//! (`AWS_IAM` and none), the authorization the endpoint's method requires (`AWS_IAM` and
//! `NONE`), the verb the request states (`GET` and `POST`), what it carries (a query parameter,
//! a header, a body), the path it addresses (two resources of the stage, and one the deployment
//! holds nothing at), and the form the resolver's definition takes (VTL mapping templates and
//! APPSYNC_JS `code`).
//!
//! Two services have to agree for the rule to hold — API Gateway decides the invoke and renders
//! what it echoes back, AppSync builds the request, signs it and applies the answer — so this is
//! the suite that can pin it. The GraphQL request is POSTed to the address the API itself
//! PUBLISHED, as a client does.
//!
//! WHAT IS AND IS NOT PINNED HERE about the signature: this suite runs with the emulator's
//! request authentication OFF, as every other data-plane suite of this target does, so what a
//! row reads off the stage's echo is that the request ARRIVED signed and the CREDENTIAL SCOPE it
//! carries names the service and the Region the data source's `awsIamConfig` configures — which
//! is what routes and admits such a call. That a signature of that scope VERIFIES against this
//! emulator's own verifier, and what the stage answers one that does not, belongs to the suite
//! that owns the stage's own verification (`apigateway_iam_authenticated_invoke` and
//! `apigateway_iam_signature_mismatch_quotes_the_canonical_request`): the toggle cannot be moved
//! here, because the handle this plane's egress is sent through is the one the create was given
//! and not the router a test holds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is made in.
const REGION: &str = "us-west-2";

/// The long-term key every emulator publishes, which every setup call is signed with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The stage the REST API is deployed to.
const STAGE: &str = "live";

/// The signing name an invoke of a deployed stage is signed under: the `execute-api` service AWS
/// publishes the invoke endpoint and the `execute-api:Invoke` action under, which is NOT the
/// control plane's own name (`apigateway`).
const INVOKE_SIGNING_NAME: &str = "execute-api";

/// The two resources of the stage: one method behind `AWS_IAM`, one open to everyone, and one
/// `POST` method the request's own body reaches.
const GUARDED_PATH: &str = "guarded";
const OPEN_PATH: &str = "open";
const ORDERS_PATH: &str = "orders";

/// A path the deployment holds no resource for, which the stage answers its own refusal for.
const UNSERVED_PATH: &str = "nothing-here";

/// The two data sources every API of this suite holds: the IAM-authorized endpoint and the same
/// endpoint with no authorization configured at all.
const SIGNED_SOURCE: &str = "signed";
const PLAIN_SOURCE: &str = "plain";

/// The query parameter and the header a request carries, and the values this suite sends under
/// them — what the stage echoes back, so a case reads whether they travelled.
const QUERY_NAME: &str = "tenant";
const QUERY_VALUE: &str = "acme & co";
const HEADER_NAME: &str = "x-echo";
const HEADER_VALUE: &str = "from-appsync";

/// The body a `POST` request carries, which the stage echoes back.
const SENT_BODY: &str = r#"{"id":"A-1"}"#;

/// The HTTP field a SigV4 signature travels in, which the stage echoes back so a case reads the
/// SCOPE the signature was computed for off the request that arrived.
const AUTHORIZATION_FIELD: &str = "Authorization";

/// What a SigV4 signature opens with, and the credential scope an `AWS_IAM` HTTP data source's
/// request must carry: the service and the Region its `awsIamConfig` names, which is what admits
/// the call at the endpoint that verifies it.
const SIGV4_SCHEME: &str = "AWS4-HMAC-SHA256";

/// The sentence API Gateway refuses a request it can place no credential for with.
const MISSING_TOKEN: &str = "Missing Authentication Token";

/// How long a poll loop waits for a state another service owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches a
/// transition.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The schema every API of this suite is created with: one field per row.
const SCHEMA: &str = "\
type Answer {\n  statusCode: Int\n  body: String\n}\n\
\n\
type Query {\n  \
  openPlain: Answer\n  \
  openSigned: Answer\n  \
  guardedSigned: Answer\n  \
  guardedPlain: Answer\n  \
  postedSigned: Answer\n  \
  unservedSigned: Answer\n  \
  jsGuardedSigned: Answer\n  \
  noMethod: Answer\n  \
  badHeader: Answer\n}\n";

/// The response half every VTL resolver of this suite carries: the step's own failure is
/// RE-RAISED where there is one — the `$util.error($ctx.error.message, $ctx.error.type)` idiom —
/// and otherwise the field takes the status and the body the endpoint answered.
const RESPONSE_TEMPLATE: &str = "#if($ctx.error)$util.error($ctx.error.message, \
                                 $ctx.error.type)#end\
                                 {\"statusCode\": $ctx.result.statusCode, \
                                 \"body\": $util.toJson($ctx.result.body)}";

/// The APPSYNC_JS form of one request half and its response half, which is the other form a
/// definition takes. Its path is the same constant every VTL row builds its own from, so the
/// row cannot be decoupled from the resource it means to reach.
fn js_code() -> String {
    format!(
        "export function request(ctx) {{\n  \
         return {{method: 'GET', resourcePath: '/{GUARDED_PATH}'}};\n}}\n\
         export function response(ctx) {{\n  \
         return {{statusCode: ctx.result.statusCode, body: ctx.result.body}};\n}}\n"
    )
}

/// The whole emulator over ONE engine, exactly as `core/router/src/lib.rs` assembles it for the
/// binary: AppSync publishes the endpoint and makes the call, API Gateway serves the stage, and
/// IAM holds the data source's service role.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The SETUP of every suite below is made with authentication off, so a control-plane call
    // needs no real signature — and it stays off for the data-plane rows, because the handle this
    // API's egress is sent through is the one its create was given rather than the router a test
    // holds: what a row reads is the signature the data source PRESENTED and its scope, with the
    // verification of one owned by the apigateway suites this module's header names.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential is the ambient key — the key a service reads
/// the caller's identity from. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One control-plane call of the service `signing_name` names (restJson1), as its status and
/// parsed body.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Vec::new, |body| {
        serde_json::to_vec(&body).expect("the test body serializes")
    });
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization(signing_name))
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

/// One AppSync control-plane call, asserting it was served.
async fn appsync_ok(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let (status, answered) = call(router, "appsync", method, path, body).await;
    assert_eq!(
        status, 200,
        "setup: appsync {method} {path} expected 200, got {status} {answered}"
    );
    answered
}

/// One API Gateway control-plane call, asserting it answered `expected`.
async fn apigateway_ok(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let (status, answered) = call(router, "apigateway", method, path, body).await;
    assert_eq!(
        status, expected,
        "setup: apigateway {method} {path} expected {expected}, got {status} {answered}"
    );
    answered
}

/// One IAM call (awsQuery), answering the response body.
async fn iam(router: &MultiServiceRouter, body: String) -> String {
    let host = format!("iam.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body.clone()))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(status, 200, "setup: iam {body} expected 200, got {text}");
    text
}

/// A required string member of a response body.
fn member(what: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what} must carry a non-empty {name:?}, got {body}"))
}

/// Create the role the IAM-authorized data source names: trusted by AppSync and allowed to
/// invoke a deployed stage, which is what an `AWS_IAM`-authorized HTTP endpoint's call performs.
///
/// The documents are written compactly and free of the characters an
/// `application/x-www-form-urlencoded` body treats specially, so they need no encoding.
async fn create_role(router: &MultiServiceRouter, name: &str) -> String {
    let trust = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"appsync.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"}]}";
    let created = iam(
        router,
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}\
             &AssumeRolePolicyDocument={trust}"
        ),
    )
    .await;
    let role_arn = crate::query_wire::opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole reports no <Arn>: {created}"));
    let document = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                    \"Action\":\"execute-api:Invoke\",\"Resource\":\"*\"}]}";
    iam(
        router,
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={name}\
             &PolicyName=invoke-the-stage&PolicyDocument={document}"
        ),
    )
    .await;
    role_arn
}

/// The integration response template a `GET` method of the stage echoes its own method request
/// with: the verb and the resource it reached, the query parameter and the header the caller
/// sent, and the CREDENTIAL SCOPE the request presented — the `Authorization` field a SigV4
/// signature travels in, which names the service and the Region the signature was computed for
/// and nothing a caller could sign another request with. A request that arrived unsigned
/// presents no such field, so the echo is empty for one.
///
/// A method whose `authorizationType` is `AWS_IAM` also echoes the ACCOUNT its authorization
/// authenticated for the call — `$context.identity.accountId` is "the AWS account ID associated
/// with the request", which API Gateway resolves off the credential the request SIGNED with. A
/// method that authenticates nobody carries no such value at all, and a template REFERENCING one
/// the call has no value for is a template the api cannot render — which earns the api's own
/// configuration error rather than an empty echo — so the open method's template leaves the
/// member out and the signature's own scope above is what says whether that call arrived signed.
fn echo_template(account: bool) -> String {
    let authenticated = match account {
        true => "\n\"account\":\"$context.identity.accountId\",",
        false => "",
    };
    format!(
        r#"{{"method":"$context.httpMethod","path":"$context.resourcePath",{authenticated}
"scope":"$input.params('{AUTHORIZATION_FIELD}')",
"{QUERY_NAME}":"$input.params('{QUERY_NAME}')","{HEADER_NAME}":"$input.params('{HEADER_NAME}')"}}"#
    )
}

/// The same echo for the `POST` method, which also carries the BODY the caller sent: a `MOCK`
/// integration answers its own rendered request, so the request template carries the body through
/// and the response template reads it back off that answer.
fn posted_templates() -> (String, String) {
    (
        r#"{"statusCode":200,"sent":$input.json('$')}"#.to_string(),
        r#"{"method":"$context.httpMethod","path":"$context.resourcePath",
"account":"$context.identity.accountId","sent":$input.json('$.sent')}"#
            .to_string(),
    )
}

/// One `MOCK` method of the stage: the resource it sits on, the verb it serves, the
/// authorization in front of it, and the two templates either side of its integration.
struct MockMethod<'a> {
    path_part: &'a str,
    verb: &'a str,
    authorization_type: &'a str,
    request_template: &'a str,
    response_template: &'a str,
}

/// Put one `MOCK` method on its own resource under `root`, answering its own
/// `response_template` for a request its `request_template` selects.
async fn mock_method(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
    method: &MockMethod<'_>,
) {
    let resource = apigateway_ok(
        router,
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": method.path_part })),
        201,
    )
    .await;
    let resource_id = member("CreateResource", &resource, "id");
    let base = format!(
        "/restapis/{api}/resources/{resource_id}/methods/{}",
        method.verb
    );
    apigateway_ok(
        router,
        "PUT",
        &base,
        Some(json!({ "authorizationType": method.authorization_type })),
        201,
    )
    .await;
    apigateway_ok(
        router,
        "PUT",
        &format!("{base}/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": method.request_template },
        })),
        201,
    )
    .await;
    apigateway_ok(
        router,
        "PUT",
        &format!("{base}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigateway_ok(
        router,
        "PUT",
        &format!("{base}/integration/responses/200"),
        Some(json!({
            "statusCode": "200",
            "responseTemplates": { "application/json": method.response_template },
        })),
        201,
    )
    .await;
}

/// Stand the REST API up — the guarded `GET`, the open `GET` and the guarded `POST`, each behind
/// a `MOCK` integration that echoes the method request — deploy it, and answer the URL its stage
/// is invoked at, which is the endpoint the HTTP data sources are configured with.
async fn deployed_stage(router: &MultiServiceRouter) -> String {
    let created = apigateway_ok(
        router,
        "POST",
        "/restapis",
        Some(json!({ "name": "appsync-http-data-source" })),
        201,
    )
    .await;
    let api_id = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let echo = echo_template(true);
    let open_echo = echo_template(false);
    let (posted_request, posted_response) = posted_templates();
    let methods = [
        MockMethod {
            path_part: GUARDED_PATH,
            verb: "GET",
            authorization_type: "AWS_IAM",
            request_template: r#"{"statusCode":200}"#,
            response_template: &echo,
        },
        MockMethod {
            path_part: OPEN_PATH,
            verb: "GET",
            authorization_type: "NONE",
            request_template: r#"{"statusCode":200}"#,
            response_template: &open_echo,
        },
        MockMethod {
            path_part: ORDERS_PATH,
            verb: "POST",
            authorization_type: "AWS_IAM",
            request_template: &posted_request,
            response_template: &posted_response,
        },
    ];
    for method in &methods {
        mock_method(router, &api_id, &root, method).await;
    }
    apigateway_ok(
        router,
        "POST",
        &format!("/restapis/{api_id}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    format!("https://{api_id}.execute-api.{REGION}.amazonaws.com/{STAGE}")
}

/// The request half of one row, as the HTTP data source's own request object: the verb, the path
/// under the endpoint, and the parameters it carries.
fn request_template(verb: &str, resource_path: &str, carries: Carries) -> String {
    let params = match carries {
        Carries::Nothing => String::new(),
        Carries::QueryAndHeader => format!(
            r#", "params": {{"query": {{"{QUERY_NAME}": "{QUERY_VALUE}"}},
"headers": {{"{HEADER_NAME}": "{HEADER_VALUE}"}}}}"#
        ),
        Carries::Body => format!(
            r#", "params": {{"headers": {{"content-type": "application/json"}},
"body": {}}}"#,
            json!(SENT_BODY)
        ),
    };
    format!(
        r#"{{"version": "2018-05-29", "method": "{verb}", "resourcePath": "{resource_path}"{params}}}"#
    )
}

/// What one row's request carries beside its verb and its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carries {
    /// Nothing of its own.
    Nothing,
    /// One query parameter and one header.
    QueryAndHeader,
    /// A body, declared as the media type the endpoint reads it as.
    Body,
}

/// Create the GraphQL API, install [`SCHEMA`], attach the two HTTP data sources at `endpoint`
/// and one UNIT resolver per field, and answer `(apiId, uris.GRAPHQL, apiKey)`.
async fn create_api(
    router: &MultiServiceRouter,
    endpoint: &str,
    role_arn: &str,
) -> (String, String, String) {
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": "http-data-source", "authenticationType": "API_KEY"})),
    )
    .await;
    let api = &created["graphqlApi"];
    let api_id = member("CreateGraphqlApi", api, "apiId");
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
        Some(json!({ "definition": ::aws_smithy_types::base64::encode(SCHEMA) })),
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
    // The two data sources: the endpoint AppSync signs for, and the same endpoint with no
    // authorization configured. "The system assumes this role when accessing the data source",
    // so the IAM-authorized one names the role; the plain one needs none.
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({
            "name": SIGNED_SOURCE,
            "type": "HTTP",
            "serviceRoleArn": role_arn,
            "httpConfig": {
                "endpoint": endpoint,
                "authorizationConfig": {
                    "authorizationType": "AWS_IAM",
                    "awsIamConfig": {
                        "signingRegion": REGION,
                        "signingServiceName": INVOKE_SIGNING_NAME,
                    },
                },
            },
        })),
    )
    .await;
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({
            "name": PLAIN_SOURCE,
            "type": "HTTP",
            "httpConfig": { "endpoint": endpoint },
        })),
    )
    .await;
    // One UNIT resolver per field: the VTL pair for every row but the APPSYNC_JS one, whose
    // definition is `code` instead.
    let templated: &[(&str, &str, String)] = &[
        (
            "openPlain",
            PLAIN_SOURCE,
            request_template("GET", &format!("/{OPEN_PATH}"), Carries::QueryAndHeader),
        ),
        (
            "openSigned",
            SIGNED_SOURCE,
            request_template("GET", &format!("/{OPEN_PATH}"), Carries::QueryAndHeader),
        ),
        (
            "guardedSigned",
            SIGNED_SOURCE,
            request_template("GET", &format!("/{GUARDED_PATH}"), Carries::QueryAndHeader),
        ),
        (
            "guardedPlain",
            PLAIN_SOURCE,
            request_template("GET", &format!("/{GUARDED_PATH}"), Carries::QueryAndHeader),
        ),
        (
            "postedSigned",
            SIGNED_SOURCE,
            request_template("POST", &format!("/{ORDERS_PATH}"), Carries::Body),
        ),
        (
            "unservedSigned",
            SIGNED_SOURCE,
            request_template("GET", &format!("/{UNSERVED_PATH}"), Carries::Nothing),
        ),
        (
            "noMethod",
            SIGNED_SOURCE,
            format!(r#"{{"version": "2018-05-29", "resourcePath": "/{OPEN_PATH}"}}"#),
        ),
        (
            "badHeader",
            SIGNED_SOURCE,
            format!(
                r#"{{"version": "2018-05-29", "method": "GET", "resourcePath": "/{OPEN_PATH}",
"params": {{"headers": {{"{HEADER_NAME}": {{"id": "A-1"}}}}}}}}"#
            ),
        ),
    ];
    for (field, data_source, request) in templated {
        appsync_ok(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers"),
            Some(json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": data_source,
                "requestMappingTemplate": request,
                "responseMappingTemplate": RESPONSE_TEMPLATE,
            })),
        )
        .await;
    }
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": "jsGuardedSigned",
            "kind": "UNIT",
            "dataSourceName": SIGNED_SOURCE,
            "code": js_code(),
            "runtime": {"name": "APPSYNC_JS", "runtimeVersion": "1.0.0"},
        })),
    )
    .await;
    let key = appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/apikeys"),
        Some(json!({"description": "the suite's key"})),
    )
    .await;
    let api_key = member("CreateApiKey", &key["apiKey"], "id");
    (api_id, graphql_uri, api_key)
}

/// Everything a row is driven against: the published GraphQL endpoint and the key a call on an
/// `API_KEY` API presents.
struct Served {
    graphql_uri: String,
    api_key: String,
}

/// Stand the whole estate up: the deployed stage, the role, and the GraphQL API whose resolvers
/// call that stage.
async fn standing(router: &MultiServiceRouter, role: &str) -> Served {
    let endpoint = deployed_stage(router).await;
    let role_arn = create_role(router, role).await;
    let (_, graphql_uri, api_key) = create_api(router, &endpoint, &role_arn).await;
    Served {
        graphql_uri,
        api_key,
    }
}

/// Select `field` through the API's own published endpoint and answer what the result reports for
/// it: the field's value and the errors the result carries.
async fn selected(case: &str, served: &Served, field: &str) -> (Value, Vec<(String, String)>) {
    let document = json!({ "query": format!("{{ {field} {{ statusCode body }} }}") });
    let (status, answered) = crate::appsync_lambda_data_source_and_authorizer::post_published_uri(
        case,
        &served.graphql_uri,
        &document,
        &[("x-api-key", served.api_key.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the GraphQL endpoint must answer 200, got {status} {answered}"
    );
    let errors = answered["errors"]
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
        .unwrap_or_default();
    (answered["data"][field].clone(), errors)
}

/// The document the stage echoed, parsed off the `body` the field reports.
fn echoed(case: &str, value: &Value) -> Value {
    let body = value["body"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the field must report a body, got {value}"));
    serde_json::from_str(body)
        .unwrap_or_else(|_| panic!("case {case:?}: expected the endpoint's document, got {body:?}"))
}

/// RULE, the whole of it bar the refusals: every row's field is resolved from what the stage
/// ANSWERED the request its definition described — and only a data source whose
/// `authorizationConfig` names `AWS_IAM` has that request signed, which is what the guarded
/// method admits and the unsigned call is refused for.
#[tokio::test]
async fn an_http_data_source_calls_its_endpoint_and_signs_only_where_its_configuration_says_so() {
    let router = build_router();
    let served = standing(&router, "appsync-http-signed").await;

    /// (case, the field, the status the stage answers, whether the request arrived SIGNED,
    /// whether the stage echoes the ACCOUNT it authenticated for the call — which only an
    /// `AWS_IAM` method has to echo at all)
    type Row<'a> = (&'a str, &'a str, u64, bool, bool);
    let rows: &[Row<'_>] = &[
        (
            "an unsigned call on a method that authenticates nobody",
            "openPlain",
            200,
            false,
            false,
        ),
        (
            "a signed call on that same open method",
            "openSigned",
            200,
            true,
            false,
        ),
        (
            "a signed call on an AWS_IAM-guarded method",
            "guardedSigned",
            200,
            true,
            true,
        ),
        (
            "a signed call whose definition is written in APPSYNC_JS code",
            "jsGuardedSigned",
            200,
            true,
            true,
        ),
    ];
    for (case, field, status, signed, authenticated) in rows {
        let (value, errors) = selected(case, &served, field).await;
        assert!(
            errors.is_empty(),
            "case {case:?}: expected the field to resolve, got errors {errors:?}"
        );
        assert_eq!(
            value["statusCode"].as_u64(),
            Some(*status),
            "case {case:?}: expected the status the endpoint answered, got {value}"
        );
        let echo = echoed(case, &value);
        assert_eq!(
            echo["method"].as_str(),
            Some("GET"),
            "case {case:?}: expected the verb the request stated, got {echo}"
        );
        // WHETHER the request arrived signed, and FOR WHAT: the scope a signature carries names
        // the service and the Region the data source's `awsIamConfig` configures, so a call
        // signed for anything else is a different value here and not merely a present one.
        let scope = echo["scope"].as_str().unwrap_or_default().to_string();
        let scoped = format!("/{REGION}/{INVOKE_SIGNING_NAME}/aws4_request");
        assert_eq!(
            scope.starts_with(SIGV4_SCHEME) && scope.contains(&scoped),
            *signed,
            "case {case:?}: expected a request signed={signed} for {scoped}, got a scope of \
             {} characters in {echo}",
            scope.len()
        );
        let account = echo["account"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            !account.is_empty(),
            *authenticated,
            "case {case:?}: expected an echoed account={authenticated}, got account \
             {account:?} in {echo}"
        );
    }

    // The two halves of the same guarded method, which is the difference the configuration
    // buys: the signed call is served and the unsigned one is refused by the endpoint itself,
    // reported as the status the field resolves to rather than as an error.
    let (refused, errors) = selected("an unsigned call on a guarded method", &served, "guardedPlain")
        .await;
    assert!(
        errors.is_empty(),
        "case \"an unsigned call on a guarded method\": expected the field to resolve, got \
         errors {errors:?}"
    );
    assert_eq!(
        refused["statusCode"].as_u64(),
        Some(403),
        "case \"an unsigned call on a guarded method\": expected the endpoint's own refusal, \
         got {refused}"
    );
    let stated = echoed("an unsigned call on a guarded method", &refused);
    assert_eq!(
        stated["message"].as_str(),
        Some(MISSING_TOKEN),
        "case \"an unsigned call on a guarded method\": expected the endpoint's sentence, got \
         {stated}"
    );
}

/// RULE, what the request CARRIES: the query parameter, the header and the body the request half
/// described reach the endpoint, under the verb and the path it stated.
#[tokio::test]
async fn what_the_request_half_described_is_what_the_endpoint_received() {
    let router = build_router();
    let served = standing(&router, "appsync-http-carried").await;

    let case = "a GET carrying a query parameter and a header";
    let (value, errors) = selected(case, &served, "guardedSigned").await;
    assert!(
        errors.is_empty(),
        "case {case:?}: expected the field to resolve, got errors {errors:?}"
    );
    let echo = echoed(case, &value);
    let guarded_path = format!("/{GUARDED_PATH}");
    for (member, expected) in [
        ("method", "GET"),
        ("path", guarded_path.as_str()),
        (QUERY_NAME, QUERY_VALUE),
        (HEADER_NAME, HEADER_VALUE),
    ] {
        assert_eq!(
            echo[member].as_str(),
            Some(expected),
            "case {case:?}: expected {member} to reach the endpoint as {expected:?}, got {echo}"
        );
    }

    let case = "a POST carrying a body";
    let (value, errors) = selected(case, &served, "postedSigned").await;
    assert!(
        errors.is_empty(),
        "case {case:?}: expected the field to resolve, got errors {errors:?}"
    );
    assert_eq!(
        value["statusCode"].as_u64(),
        Some(200),
        "case {case:?}: expected the status the endpoint answered, got {value}"
    );
    let echo = echoed(case, &value);
    assert_eq!(
        echo["method"].as_str(),
        Some("POST"),
        "case {case:?}: expected the verb the request stated, got {echo}"
    );
    assert_eq!(
        echo["path"].as_str(),
        Some(format!("/{ORDERS_PATH}").as_str()),
        "case {case:?}: expected the path the request addressed, got {echo}"
    );
    assert_eq!(
        echo["sent"],
        serde_json::from_str::<Value>(SENT_BODY).expect("the sent body is a document"),
        "case {case:?}: expected the body the request carried, got {echo}"
    );

    // A path the deployment holds no resource for is answered by the ENDPOINT itself — the
    // refusal API Gateway states for a request it can place no route for — and the field
    // resolves to that answer rather than failing.
    let case = "a path the deployment holds nothing at";
    let (value, errors) = selected(case, &served, "unservedSigned").await;
    assert!(
        errors.is_empty(),
        "case {case:?}: expected the field to resolve, got errors {errors:?}"
    );
    assert_eq!(
        value["statusCode"].as_u64(),
        Some(403),
        "case {case:?}: expected the endpoint's own refusal of an unserved path, got {value}"
    );
    let stated = echoed(case, &value);
    assert_eq!(
        stated["message"].as_str(),
        Some(MISSING_TOKEN),
        "case {case:?}: expected the endpoint's sentence for an unplaceable request, got {stated}"
    );
}

/// RULE, the refusals: a request the HTTP data source cannot act on is never MADE — the step
/// reports it as an error of the definition that produced it, locating the member at fault.
#[tokio::test]
async fn a_request_this_data_source_cannot_act_on_is_refused_and_located() {
    let router = build_router();
    let served = standing(&router, "appsync-http-refused").await;

    /// (case, the field, the member the refusal locates)
    type Row<'a> = (&'a str, &'a str, String);
    let rows: &[Row<'_>] = &[
        (
            "a request stating no method",
            "noMethod",
            "[method]".to_string(),
        ),
        (
            "a header carrying no single value",
            "badHeader",
            format!("[params][headers][{HEADER_NAME}]"),
        ),
    ];
    for (case, field, located) in rows {
        let (value, errors) = selected(case, &served, field).await;
        assert_eq!(
            value,
            Value::Null,
            "case {case:?}: expected the field not to resolve, got {value}"
        );
        assert_eq!(
            errors.len(),
            1,
            "case {case:?}: expected exactly one error of the field, got {errors:?}"
        );
        let (error_type, message) = &errors[0];
        assert!(
            message.contains(located),
            "case {case:?}: expected the refusal to locate {located}, got {message:?}"
        );
        assert_eq!(
            error_type, "Code",
            "case {case:?}: expected the error of the definition's own code, got {errors:?}"
        );
    }
}

