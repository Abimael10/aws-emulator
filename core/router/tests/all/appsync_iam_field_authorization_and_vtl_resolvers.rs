// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API served at the address it PUBLISHES, resolved by VTL mapping
//! templates, with the `AWS_IAM` authorization mode deciding each field a call selects.
//!
//! RULE: a GraphQL API publishes, as `GraphqlApi$uris`'s `GRAPHQL` entry, the URL of the
//! socket that endpoint is served on, so a client that POSTs a GraphQL document to the
//! published value verbatim reaches the API it just created. There:
//!
//! * a resolver defined by MAPPING TEMPLATES is run — the request template renders the
//!   request its data source is handed (`"payload"` IS the result of a `NONE` data source)
//!   and the response template renders the value the field resolves to — so a template's
//!   references, its `#set` / `#if` / `#foreach` directives and its `$util` helpers all take
//!   effect, and `$util.error` reports the field as one that did not resolve;
//! * `$context.identity` carries who the call signed as: the account and the user principal,
//!   and — for a call signed with credentials an identity pool vended — the pool, the
//!   identity and the authentication type that exchange was made under;
//! * with `AWS_IAM`, every field the call selects is authorized for `appsync:GraphQL` on
//!   `<the API's ARN>/types/<typeName>/fields/<fieldName>` before any resolver runs: a grant
//!   covering the field serves it, an explicit `Deny` over one field of a wildcard-allowed
//!   API refuses it, and a caller no statement allows is refused too — a call that may reach
//!   no field it selects is answered `401` with `UnauthorizedException` / "Permission
//!   denied", named in the answer's own error-type field as well as in the error itself, and
//!   one that may reach some is served in part, the refused fields resolving to nothing and
//!   reported as `Unauthorized` errors of their own path, with no resolver of them run;
//! * and whatever a template rendered is written out as the field's DECLARED type writes it:
//!   a field of the `AWSJSON` scalar — "a JSON string. Any valid JSON construct" — answers
//!   the JSON TEXT of the construct the response template produced, so a client reads the
//!   echoed identity back by parsing that one string.
//!
//! Four services have to agree for that to hold — Cognito Identity vends the session, IAM
//! holds the role and its policy, STS records the session and names its principal, and
//! AppSync publishes the endpoint, runs the templates and takes the decision — so this is the
//! suite that can pin it.
//!
//! The rows span what the rule generalises over: the template construct a resolver is
//! defined by (a literal payload, a `#set` with an interpolated reference, a `#foreach` with
//! `$foreach.hasNext`, an `#if` over an argument, a stash written in the request template and
//! read in the response template, `$util` helpers over a map and a string, a `$util.error`,
//! a helper that does not exist, and a `PIPELINE` resolver whose function is a template
//! pair); the exchange a session comes from (an authenticated developer identity and an
//! unauthenticated guest); the policy in force on the caller (the field itself, the
//! API-wide wildcard, the wildcard with an explicit deny over the field, a grant over
//! another field only, and no grant at all); the DECLARED type of the field a template
//! resolves (an object type, a scalar, and the `AWSJSON` scalar); and how many fields one
//! call selects (one, two).

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

/// The developer provider the authenticated identity of this suite logs in through, and the
/// user of that provider's own directory the login names.
const DEVELOPER_PROVIDER: &str = "login.fidelity.test";
const DEVELOPER_USER: &str = "developer-user-1";

/// The path a GraphQL API's `GRAPHQL` endpoint serves.
const GRAPHQL_PATH: &str = "/graphql";

/// The `errorType` and the sentence AppSync refuses a call its IAM policies do not permit
/// with.
const UNAUTHORIZED_ERROR_TYPE: &str = "UnauthorizedException";
const PERMISSION_DENIED: &str = "Permission denied";

/// How long a poll loop waits for a state the clock owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host
/// reaches a transition.
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a case waits for one answer from the API's own socket.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

/// The data source every resolver here resolves against: `NONE` is "There is no data
/// source", so the request template's own `payload` is the result the response template is
/// handed.
const DATA_SOURCE: &str = "local";

/// The schema every API of this suite is created with: one field per template row, the
/// identity echo, and the two fields the authorization rows select.
const SCHEMA: &str = "\
type Who {\n  accountId: String\n  userArn: String\n  username: String\n  \
  cognitoIdentityId: String\n  cognitoIdentityPoolId: String\n  \
  cognitoIdentityAuthType: String\n  cognitoIdentityAuthProvider: String\n}\n\
\n\
type Query {\n  \
  allowed: String\n  other: String\n  whoAmI: Who\n  \
  identityJson: AWSJSON\n  otherJson: AWSJSON\n  \
  vtlLiteral: String\n  vtlEcho(text: String): String\n  vtlList: [Int]\n  \
  vtlBranch(flag: Boolean): String\n  vtlStash: String\n  vtlHelpers: String\n  \
  vtlRefused: String\n  vtlUnknownHelper: String\n  vtlPiped: String\n}\n";

/// The two halves of the resolver the identity rows are resolved by, as the documented VTL
/// form of one writes them: the request half renders the caller's identity as the payload the
/// `NONE` data source answers with, and the response half renders that result as the value
/// the field takes. `$context` is the long spelling of the reference `$ctx` abbreviates.
const IDENTITY_REQUEST_TEMPLATE: &str =
    r#"{"version": "2018-05-29", "payload": $util.toJson($context.identity)}"#;
const IDENTITY_RESPONSE_TEMPLATE: &str = "$util.toJson($context.result)";

/// The field of the answer AppSync names the class of a data-plane error in, beside the error
/// itself; the service's own copy is `service::graphql_api::query`'s `ERROR_TYPE_FIELD`.
const ERROR_TYPE_FIELD: &str = "x-amzn-errortype";

/// The four services this rule spans, over ONE engine.
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
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
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

/// One created GraphQL API: what it is addressed by, and where it is served.
struct Api {
    api_id: String,
    arn: String,
    /// The URL the API publishes for its `GRAPHQL` endpoint, which is what a client POSTs to.
    graphql_uri: String,
}

/// Create one API with `authentication_type`, install [`SCHEMA`] and the `NONE` data source,
/// and read the endpoint it publishes.
async fn create_api(router: &MultiServiceRouter, name: &str, authentication_type: &str) -> Api {
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": name, "authenticationType": authentication_type})),
    )
    .await;
    let api = &created["graphqlApi"];
    let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
    let arn = api["arn"].as_str().unwrap_or_default().to_string();
    let graphql_uri = api["uris"]["GRAPHQL"].as_str().unwrap_or_default().to_string();
    assert!(
        graphql_uri.ends_with(GRAPHQL_PATH) && graphql_uri.contains("127.0.0.1:"),
        "setup: uris.GRAPHQL must be a URL this emulator serves, got {graphql_uri:?}"
    );
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/schemacreation"),
        Some(json!({
            "definition": ::aws_smithy_types::base64::encode(SCHEMA),
        })),
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
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": DATA_SOURCE, "type": "NONE"})),
    )
    .await;
    Api {
        api_id,
        arn,
        graphql_uri,
    }
}

/// Attach a UNIT resolver defined by mapping templates to `field` of `Query`.
async fn create_template_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    field: &str,
    request_template: &str,
    response_template: &str,
) {
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": field,
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "requestMappingTemplate": request_template,
            "responseMappingTemplate": response_template,
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
    let (status, _, document) = post_published_answer(case, api, body, fields).await;
    (status, document)
}

/// The same, answering the response FIELDS as well — the lowercase name of each, with its
/// value — for a case that reads the class off the answer rather than out of its body.
async fn post_published_answer(
    case: &str,
    api: &Api,
    body: &Value,
    fields: &[(&str, String)],
) -> (u16, Vec<(String, String)>, Value) {
    let (authority, path) = published_parts(case, &api.graphql_uri);
    let port: u16 = authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: {authority:?} must name a port"));
    let mut stream = tokio::time::timeout(
        ANSWER_DEADLINE,
        TcpStream::connect(("127.0.0.1", port)),
    )
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
    let answered_fields: Vec<(String, String)> = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let document = serde_json::from_str(payload).unwrap_or(Value::Null);
    (status, answered_fields, document)
}

/// The value one field of an answer carries, or `None` when the answer states none.
fn answered_field(fields: &[(String, String)], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value.clone())
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
async fn a_resolver_defined_by_mapping_templates_is_executed_at_the_published_endpoint() {
    let router = build_router();
    let api = create_api(&router, "templates", "API_KEY").await;
    let key = appsync_ok(
        &router,
        "POST",
        &format!("/v1/apis/{}/apikeys", api.api_id),
        Some(json!({"description": "the suite's key"})),
    )
    .await;
    let api_key = key["apiKey"]["id"].as_str().unwrap_or_default().to_string();

    /// One row: the field, the template pair that defines it, the document that selects it,
    /// and what the endpoint must answer.
    struct Row {
        case: &'static str,
        field: &'static str,
        request_template: &'static str,
        response_template: &'static str,
        query: &'static str,
        data: Value,
        /// The message the one reported error must state, for a row whose field does not
        /// resolve.
        error: Option<&'static str>,
    }
    let rows: Vec<Row> = vec![
        Row {
            case: "a literal payload rendered by the request template",
            field: "vtlLiteral",
            request_template: r#"{"version": "2018-05-29", "payload": "rendered-literal"}"#,
            response_template: "$util.toJson($ctx.result)",
            query: "query { vtlLiteral }",
            data: json!({"vtlLiteral": "rendered-literal"}),
            error: None,
        },
        Row {
            case: "a #set whose value interpolates an argument",
            field: "vtlEcho",
            request_template: "#set($spoken = \"echo:${ctx.args.text}\")\n\
                               {\"version\": \"2018-05-29\", \"payload\": {\"said\": \"$spoken\"}}",
            response_template: "$util.toJson($ctx.result.said)",
            query: "query { vtlEcho(text: \"spoken\") }",
            data: json!({"vtlEcho": "echo:spoken"}),
            error: None,
        },
        Row {
            case: "a #foreach over a list literal, separated by $foreach.hasNext",
            field: "vtlList",
            request_template: "{\"version\": \"2018-05-29\", \"payload\": [\
                               #foreach($n in [2, 3, 5])$n#if($foreach.hasNext), #end#end]}",
            response_template: "$util.toJson($ctx.result)",
            query: "query { vtlList }",
            data: json!({"vtlList": [2, 3, 5]}),
            error: None,
        },
        Row {
            case: "an #if / #else over an argument",
            field: "vtlBranch",
            request_template: "{\"version\": \"2018-05-29\", \"payload\": \
                               #if($ctx.args.flag)\"on\"#{else}\"off\"#end}",
            response_template: "$util.toJson($ctx.result)",
            query: "query { vtlBranch(flag: true) }",
            data: json!({"vtlBranch": "on"}),
            error: None,
        },
        Row {
            case: "a stash written in the request template and read in the response one",
            field: "vtlStash",
            request_template: "#set($ctx.stash.seen = \"stashed\")\
                               {\"version\": \"2018-05-29\", \"payload\": {}}",
            response_template: "$util.toJson($ctx.stash.seen)",
            query: "query { vtlStash }",
            data: json!({"vtlStash": "stashed"}),
            error: None,
        },
        Row {
            case: "$util helpers and the methods of a map and a string",
            field: "vtlHelpers",
            request_template: "#set($built = {})\
                               $util.qr($built.put(\"name\", $util.base64Encode(\"ab\")))\
                               #set($name = $built.get(\"name\"))\
                               {\"version\": \"2018-05-29\", \"payload\": \
                               {\"name\": \"$name.toLowerCase()\", \
                                \"size\": $built.size(), \
                                \"blank\": $util.isNullOrEmpty($ctx.args.missing)}}",
            response_template: "$util.toJson(\"$ctx.result.name/$ctx.result.size/$ctx.result.blank\")",
            query: "query { vtlHelpers }",
            data: json!({"vtlHelpers": "ywi=/1/true"}),
            error: None,
        },
        Row {
            case: "a request template that raises with $util.error",
            field: "vtlRefused",
            request_template: "$util.error(\"this template refuses the field\", \"Refused\")",
            response_template: "$util.toJson($ctx.result)",
            query: "query { vtlRefused }",
            data: json!({"vtlRefused": Value::Null}),
            error: Some("this template refuses the field"),
        },
        Row {
            case: "a template naming a helper this endpoint does not publish",
            field: "vtlUnknownHelper",
            request_template: "{\"version\": \"2018-05-29\", \"payload\": \
                               \"$util.notAHelperAnyonePublishes()\"}",
            response_template: "$util.toJson($ctx.result)",
            query: "query { vtlUnknownHelper }",
            data: json!({"vtlUnknownHelper": Value::Null}),
            error: Some("No such helper: $util.notAHelperAnyonePublishes"),
        },
    ];

    for row in &rows {
        create_template_resolver(
            &router,
            &api.api_id,
            row.field,
            row.request_template,
            row.response_template,
        )
        .await;
    }
    // One PIPELINE resolver whose FUNCTION is a template pair too: the resolver's before
    // template stashes a value, the function's templates read it, and the after template
    // answers what the function returned.
    let function = appsync_ok(
        &router,
        "POST",
        &format!("/v1/apis/{}/functions", api.api_id),
        Some(json!({
            "name": "stashReader",
            "dataSourceName": DATA_SOURCE,
            "functionVersion": "2018-05-29",
            "requestMappingTemplate":
                "{\"version\": \"2018-05-29\", \"payload\": \"piped:${ctx.stash.seen}\"}",
            "responseMappingTemplate": "$util.toJson($ctx.result)",
        })),
    )
    .await;
    let function_id = function["functionConfiguration"]["functionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    appsync_ok(
        &router,
        "POST",
        &format!("/v1/apis/{}/types/Query/resolvers", api.api_id),
        Some(json!({
            "fieldName": "vtlPiped",
            "kind": "PIPELINE",
            "pipelineConfig": {"functions": [function_id]},
            "requestMappingTemplate":
                "#set($ctx.stash.seen = \"stashed\"){\"version\": \"2018-05-29\", \"payload\": {}}",
            "responseMappingTemplate": "$util.toJson($ctx.prev.result)",
        })),
    )
    .await;

    for row in &rows {
        let (status, answered) = post_published(
            row.case,
            &api,
            &json!({"query": row.query}),
            &[("x-api-key", api_key.clone())],
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
            Some(message) => assert_eq!(
                answered["errors"][0]["message"].as_str(),
                Some(message),
                "case {:?}: expected the error {message:?}, got {answered}",
                row.case
            ),
        }
    }

    let (status, piped) = post_published(
        "a PIPELINE resolver whose function is a template pair",
        &api,
        &json!({"query": "query { vtlPiped }"}),
        &[("x-api-key", api_key.clone())],
    )
    .await;
    assert_eq!(
        (status, piped["data"]["vtlPiped"].as_str()),
        (200, Some("piped:stashed")),
        "case a PIPELINE resolver whose function is a template pair: expected the function's \
         rendered payload, got {status} and {piped}"
    );
}

/// One identity pool with a role every exchange of it vends a session of.
struct Pool {
    pool_id: String,
    role_name: String,
}

/// Create the pool and its role: the trust policy admits the Cognito Identity principal for
/// THIS pool, as the Amazon Cognito documentation writes one, and both role types resolve to
/// it so an authenticated and a guest exchange are both served.
async fn create_pool(router: &MultiServiceRouter, name: &str) -> Pool {
    let pool = cognito(
        router,
        "CreateIdentityPool",
        &json!({
            "IdentityPoolName": name,
            "AllowUnauthenticatedIdentities": true,
            "DeveloperProviderName": DEVELOPER_PROVIDER,
        }),
    )
    .await;
    let pool_id = pool["IdentityPoolId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let role_name = format!("{name}-role");
    let trust = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Federated\":\"{COGNITO_IDENTITY_PRINCIPAL}\"}},\
         \"Action\":\"sts:AssumeRoleWithWebIdentity\",\"Condition\":{{\"StringEquals\":\
         {{\"{COGNITO_IDENTITY_PRINCIPAL}:aud\":\"{pool_id}\"}}}}}}]}}"
    );
    let created = iam(
        router,
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={role_name}\
             &AssumeRolePolicyDocument={trust}"
        ),
    )
    .await;
    let role_arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole reports no <Arn>: {created}"));
    cognito(
        router,
        "SetIdentityPoolRoles",
        &json!({
            "IdentityPoolId": pool_id,
            "Roles": {"authenticated": role_arn, "unauthenticated": role_arn},
        }),
    )
    .await;
    Pool { pool_id, role_name }
}

/// Put `document` on the pool's role as its one inline policy, replacing whatever the last
/// row put there.
async fn put_role_policy(router: &MultiServiceRouter, pool: &Pool, document: &str) {
    iam(
        router,
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={}\
             &PolicyName=graphql-field-access&PolicyDocument={document}",
            pool.role_name
        ),
    )
    .await;
}

/// Vend a session for a GUEST identity of the pool, answering the access key id a call signs
/// with and the identity id the exchange was made for.
async fn vend_guest_session(router: &MultiServiceRouter, pool: &Pool) -> (String, String) {
    let identity = cognito(router, "GetId", &json!({"IdentityPoolId": pool.pool_id})).await;
    let identity_id = identity["IdentityId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let vended = cognito(
        router,
        "GetCredentialsForIdentity",
        &json!({"IdentityId": identity_id}),
    )
    .await;
    (
        vended["Credentials"]["AccessKeyId"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        identity_id,
    )
}

/// The same for an AUTHENTICATED identity: a developer login is exchanged for an OpenID token
/// and that token for the session.
async fn vend_authenticated_session(router: &MultiServiceRouter, pool: &Pool) -> (String, String) {
    let token = cognito(
        router,
        "GetOpenIdTokenForDeveloperIdentity",
        &json!({
            "IdentityPoolId": pool.pool_id,
            "Logins": {DEVELOPER_PROVIDER: DEVELOPER_USER},
        }),
    )
    .await;
    let identity_id = token["IdentityId"].as_str().unwrap_or_default().to_string();
    let vended = cognito(
        router,
        "GetCredentialsForIdentity",
        &json!({
            "IdentityId": identity_id,
            "Logins": {
                COGNITO_IDENTITY_PRINCIPAL: token["Token"].as_str().unwrap_or_default(),
            },
        }),
    )
    .await;
    (
        vended["Credentials"]["AccessKeyId"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        identity_id,
    )
}

/// A policy allowing `appsync:GraphQL` on `resources`, with `denied` explicitly denied.
///
/// Written compactly and free of the characters an `application/x-www-form-urlencoded` body
/// treats specially (`&`, `=`, `+`, `%`, space), so it needs no encoding.
fn field_policy(allowed: &[String], denied: &[String]) -> String {
    let quoted = |arns: &[String]| {
        arns.iter()
            .map(|arn| format!("\"{arn}\""))
            .collect::<Vec<String>>()
            .join(",")
    };
    let mut statements = vec![format!(
        "{{\"Effect\":\"Allow\",\"Action\":\"appsync:GraphQL\",\"Resource\":[{}]}}",
        quoted(allowed)
    )];
    if !denied.is_empty() {
        statements.push(format!(
            "{{\"Effect\":\"Deny\",\"Action\":\"appsync:GraphQL\",\"Resource\":[{}]}}",
            quoted(denied)
        ));
    }
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{}]}}",
        statements.join(",")
    )
}

/// How one field of one type of an API is addressed by a policy.
fn field_arn(api: &Api, field: &str) -> String {
    format!("{}/types/Query/fields/{field}", api.arn)
}

#[tokio::test]
async fn the_identity_of_a_cognito_vended_session_reaches_the_resolver() {
    let router = build_router();
    let api = create_api(&router, "identity-echo", "AWS_IAM").await;
    // The echo itself is a mapping template: the request template renders the identity as the
    // payload of the local data source, and the response template renders it back.
    create_template_resolver(
        &router,
        &api.api_id,
        "whoAmI",
        r#"{"version": "2018-05-29", "payload": $util.toJson($context.identity)}"#,
        "$util.toJson($ctx.result)",
    )
    .await;
    let pool = create_pool(&router, "identity-echo-pool").await;
    put_role_policy(
        &router,
        &pool,
        &field_policy(&[format!("{}/*", api.arn)], &[]),
    )
    .await;

    /// One row: which exchange the session came from, and the authentication type that
    /// exchange publishes.
    struct Row {
        case: &'static str,
        authenticated: bool,
        auth_type: &'static str,
    }
    let rows: &[Row] = &[
        Row {
            case: "a session vended to an authenticated developer identity",
            authenticated: true,
            auth_type: "authenticated",
        },
        Row {
            case: "a session vended to an unauthenticated guest identity",
            authenticated: false,
            auth_type: "unauthenticated",
        },
    ];
    for row in rows {
        let (access_key_id, identity_id) = if row.authenticated {
            vend_authenticated_session(&router, &pool).await
        } else {
            vend_guest_session(&router, &pool).await
        };
        let (status, answered) = post_published(
            row.case,
            &api,
            &json!({
                "query": "query { whoAmI { accountId userArn username cognitoIdentityId \
                          cognitoIdentityPoolId cognitoIdentityAuthType \
                          cognitoIdentityAuthProvider } }"
            }),
            &[(
                "authorization",
                authorization(&access_key_id, "appsync"),
            )],
        )
        .await;
        assert_eq!(
            (status, answered.get("errors")),
            (200, None),
            "case {:?}: the granted field must be served cleanly, got {status} and {answered}",
            row.case
        );
        let echoed = &answered["data"]["whoAmI"];
        assert_eq!(
            echoed["cognitoIdentityId"].as_str(),
            Some(identity_id.as_str()),
            "case {:?}: expected the identity the session was vended to, got {answered}",
            row.case
        );
        assert_eq!(
            echoed["cognitoIdentityPoolId"].as_str(),
            Some(pool.pool_id.as_str()),
            "case {:?}: expected the pool the session was vended for, got {answered}",
            row.case
        );
        assert_eq!(
            echoed["cognitoIdentityAuthType"].as_str(),
            Some(row.auth_type),
            "case {:?}: expected the exchange's authentication type, got {answered}",
            row.case
        );
        // The principal the call signed as is the session's own, which is the role the pool
        // vends sessions of.
        let user_arn = echoed["userArn"].as_str().unwrap_or_default();
        assert!(
            user_arn.contains(&format!("assumed-role/{}/", pool.role_name)),
            "case {:?}: expected the session's assumed-role principal, got {answered}",
            row.case
        );
        assert_eq!(
            echoed["accountId"].as_str(),
            Some(engine::EMULATOR_ACCOUNT_ID),
            "case {:?}: expected the account the call was made in, got {answered}",
            row.case
        );
        assert!(
            !echoed["username"].as_str().unwrap_or_default().is_empty(),
            "case {:?}: expected the AWS user principal as username, got {answered}",
            row.case
        );
        // The providers that authenticated the identity, as the exchange's own
        // authentication methods name them: each quoted, in one comma-separated value. A
        // guest exchange names no provider at all, so the member carries nothing.
        let expected_provider = row.authenticated.then(|| {
            format!(
                "\"{DEVELOPER_PROVIDER}\",\"{DEVELOPER_PROVIDER}:{}:{DEVELOPER_USER}\"",
                pool.pool_id
            )
        });
        assert_eq!(
            echoed["cognitoIdentityAuthProvider"].as_str(),
            expected_provider.as_deref(),
            "case {:?}: expected the providers the exchange was made under, got {answered}",
            row.case
        );
    }
}

#[tokio::test]
async fn iam_decides_every_field_an_aws_iam_call_selects() {
    let router = build_router();
    let api = create_api(&router, "field-authorization", "AWS_IAM").await;
    for field in ["allowed", "other"] {
        create_template_resolver(
            &router,
            &api.api_id,
            field,
            &format!(r#"{{"version": "2018-05-29", "payload": "served:{field}"}}"#),
            "$util.toJson($ctx.result)",
        )
        .await;
    }
    let pool = create_pool(&router, "field-authorization-pool").await;
    let (access_key_id, _) = vend_guest_session(&router, &pool).await;

    /// One row: the policy in force on the caller, the field the call selects, and what the
    /// endpoint must answer.
    struct Row {
        case: &'static str,
        allowed: Vec<String>,
        denied: Vec<String>,
        field: &'static str,
        served: bool,
    }
    let rows: Vec<Row> = vec![
        Row {
            case: "a grant naming the field itself",
            allowed: vec![field_arn(&api, "allowed")],
            denied: Vec::new(),
            field: "allowed",
            served: true,
        },
        Row {
            case: "a grant over the whole API, by wildcard",
            allowed: vec![format!("{}/*", api.arn)],
            denied: Vec::new(),
            field: "allowed",
            served: true,
        },
        Row {
            case: "an explicit deny over one field of a wildcard-allowed API",
            allowed: vec![format!("{}/*", api.arn)],
            denied: vec![field_arn(&api, "other")],
            field: "other",
            served: false,
        },
        Row {
            case: "the same wildcard-allowed API, on the field nothing denies",
            allowed: vec![format!("{}/*", api.arn)],
            denied: vec![field_arn(&api, "other")],
            field: "allowed",
            served: true,
        },
        Row {
            case: "a grant naming another field only",
            allowed: vec![field_arn(&api, "allowed")],
            denied: Vec::new(),
            field: "other",
            served: false,
        },
        Row {
            case: "a policy granting nothing of this service",
            allowed: vec!["arn:aws:s3:::no-such-bucket/*".to_string()],
            denied: Vec::new(),
            field: "allowed",
            served: false,
        },
    ];
    for row in &rows {
        put_role_policy(&router, &pool, &field_policy(&row.allowed, &row.denied)).await;
        let (status, answered) = post_published(
            row.case,
            &api,
            &json!({"query": format!("query {{ {} }}", row.field)}),
            &[(
                "authorization",
                authorization(&access_key_id, "appsync"),
            )],
        )
        .await;
        if row.served {
            assert_eq!(
                (status, answered["data"][row.field].as_str()),
                (200, Some(format!("served:{}", row.field).as_str())),
                "case {:?}: the field must be served, got {status} and {answered}",
                row.case
            );
            assert!(
                answered.get("errors").is_none(),
                "case {:?}: expected no errors, got {answered}",
                row.case
            );
            continue;
        }
        assert_eq!(
            status, 401,
            "case {:?}: an unauthorized field must answer 401, got {status} and {answered}",
            row.case
        );
        assert_eq!(
            (
                answered["errors"][0]["errorType"].as_str(),
                answered["errors"][0]["message"].as_str(),
            ),
            (Some(UNAUTHORIZED_ERROR_TYPE), Some(PERMISSION_DENIED)),
            "case {:?}: expected the authorization refusal, got {answered}",
            row.case
        );
        assert!(
            answered.get("data").is_none(),
            "case {:?}: a refused call must reach no resolver, got {answered}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_call_selecting_a_permitted_and_a_refused_field_is_served_in_part() {
    let router = build_router();
    let api = create_api(&router, "partial-authorization", "AWS_IAM").await;
    for field in ["allowed", "other"] {
        create_template_resolver(
            &router,
            &api.api_id,
            field,
            &format!(r#"{{"version": "2018-05-29", "payload": "served:{field}"}}"#),
            "$util.toJson($ctx.result)",
        )
        .await;
    }
    let pool = create_pool(&router, "partial-authorization-pool").await;
    let (access_key_id, _) = vend_guest_session(&router, &pool).await;
    // The whole API allowed, one field of it denied: the call below selects both.
    put_role_policy(
        &router,
        &pool,
        &field_policy(&[format!("{}/*", api.arn)], &[field_arn(&api, "other")]),
    )
    .await;

    let case = "a call selecting one permitted and one refused field";
    let (status, answered) = post_published(
        case,
        &api,
        &json!({"query": "query { allowed other }"}),
        &[("authorization", authorization(&access_key_id, "appsync"))],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a call the caller may reach a field of must be served, got {status} \
         and {answered}"
    );
    assert_eq!(
        answered["data"],
        json!({"allowed": "served:allowed", "other": Value::Null}),
        "case {case:?}: expected the permitted field served and the refused one null, got \
         {answered}"
    );
    assert_eq!(
        answered["errors"],
        json!([{
            "path": ["other"],
            "errorType": "Unauthorized",
            "message": "Not Authorized to access other on type Query",
        }]),
        "case {case:?}: expected one error naming the refused field, got {answered}"
    );
}

#[tokio::test]
async fn a_json_scalar_field_resolved_by_templates_answers_the_identity_its_request_half_echoed()
{
    let router = build_router();
    let api = create_api(&router, "identity-json", "AWS_IAM").await;
    for field in ["identityJson", "otherJson"] {
        create_template_resolver(
            &router,
            &api.api_id,
            field,
            IDENTITY_REQUEST_TEMPLATE,
            IDENTITY_RESPONSE_TEMPLATE,
        )
        .await;
    }
    let pool = create_pool(&router, "identity-json-pool").await;
    put_role_policy(
        &router,
        &pool,
        &field_policy(&[format!("{}/*", api.arn)], &[]),
    )
    .await;

    /// One row: which exchange the session came from, the authentication type that exchange
    /// publishes, and the fields the call selects — each of the `AWSJSON` scalar, so each
    /// answers the JSON text of the identity the templates rendered.
    struct Row {
        case: &'static str,
        authenticated: bool,
        auth_type: &'static str,
        query: &'static str,
        selected: &'static [&'static str],
    }
    let rows: &[Row] = &[
        Row {
            case: "one json field, for a session vended to an authenticated identity",
            authenticated: true,
            auth_type: "authenticated",
            query: "query { identityJson }",
            selected: &["identityJson"],
        },
        Row {
            case: "one json field, for a session vended to an unauthenticated guest",
            authenticated: false,
            auth_type: "unauthenticated",
            query: "query { identityJson }",
            selected: &["identityJson"],
        },
        Row {
            case: "two json fields in one call, for an authenticated identity",
            authenticated: true,
            auth_type: "authenticated",
            query: "query { identityJson otherJson }",
            selected: &["identityJson", "otherJson"],
        },
        Row {
            case: "two json fields in one call, for an unauthenticated guest",
            authenticated: false,
            auth_type: "unauthenticated",
            query: "query { identityJson otherJson }",
            selected: &["identityJson", "otherJson"],
        },
    ];
    for row in rows {
        let case = row.case;
        let (access_key_id, identity_id) = if row.authenticated {
            vend_authenticated_session(&router, &pool).await
        } else {
            vend_guest_session(&router, &pool).await
        };
        let (status, answered) = post_published(
            case,
            &api,
            &json!({"query": row.query}),
            &[("authorization", authorization(&access_key_id, "appsync"))],
        )
        .await;
        assert_eq!(
            (status, answered.get("errors")),
            (200, None),
            "case {case:?}: a field a template resolves must be served cleanly, got {status} \
             and {answered}"
        );
        for field in row.selected {
            // The field is declared `AWSJSON`, so what it answers is the JSON TEXT of the
            // construct the response template produced — one string a client parses — and
            // never the construct itself.
            let text = answered["data"][field].as_str().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the {field} field must answer the json text of what the \
                     template rendered, got {answered}"
                )
            });
            let echoed: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: the {field} field's value must parse as json, got \
                     {text:?} ({error})"
                )
            });
            assert_eq!(
                (
                    echoed["cognitoIdentityId"].as_str(),
                    echoed["cognitoIdentityPoolId"].as_str(),
                    echoed["cognitoIdentityAuthType"].as_str(),
                    echoed["accountId"].as_str(),
                ),
                (
                    Some(identity_id.as_str()),
                    Some(pool.pool_id.as_str()),
                    Some(row.auth_type),
                    Some(engine::EMULATOR_ACCOUNT_ID),
                ),
                "case {case:?}: expected the {field} field to echo the identity the session \
                 was vended to, got {echoed}"
            );
            assert!(
                echoed["userArn"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(&format!("assumed-role/{}/", pool.role_name)),
                "case {case:?}: expected the {field} field to echo the session's \
                 assumed-role principal, got {echoed}"
            );
        }
    }
}

#[tokio::test]
async fn the_class_of_an_iam_refusal_is_named_in_the_answer_as_well_as_in_its_error() {
    let router = build_router();
    let api = create_api(&router, "refusal-class", "AWS_IAM").await;
    for field in ["identityJson", "otherJson"] {
        create_template_resolver(
            &router,
            &api.api_id,
            field,
            IDENTITY_REQUEST_TEMPLATE,
            IDENTITY_RESPONSE_TEMPLATE,
        )
        .await;
    }
    let pool = create_pool(&router, "refusal-class-pool").await;
    let (access_key_id, _) = vend_guest_session(&router, &pool).await;

    /// One row: the policy in force on the caller, and whether the call it makes is one the
    /// caller may reach a field of at all.
    struct Row {
        case: &'static str,
        allowed: Vec<String>,
        denied: Vec<String>,
        served: bool,
    }
    let rows: Vec<Row> = vec![
        Row {
            case: "a grant naming another field only",
            allowed: vec![field_arn(&api, "otherJson")],
            denied: Vec::new(),
            served: false,
        },
        Row {
            case: "an explicit deny over the selected field of a wildcard-allowed API",
            allowed: vec![format!("{}/*", api.arn)],
            denied: vec![field_arn(&api, "identityJson")],
            served: false,
        },
        Row {
            case: "a grant naming the selected field itself",
            allowed: vec![field_arn(&api, "identityJson")],
            denied: Vec::new(),
            served: true,
        },
    ];
    for row in &rows {
        let case = row.case;
        put_role_policy(&router, &pool, &field_policy(&row.allowed, &row.denied)).await;
        let (status, fields, answered) = post_published_answer(
            case,
            &api,
            &json!({"query": "query { identityJson }"}),
            &[("authorization", authorization(&access_key_id, "appsync"))],
        )
        .await;
        let named = answered_field(&fields, ERROR_TYPE_FIELD);
        if row.served {
            assert_eq!(
                (status, answered.get("errors"), named.as_deref()),
                (200, None, None),
                "case {case:?}: a permitted field is served with no error class to name, got \
                 {status}, {named:?} and {answered}"
            );
            assert!(
                answered["data"]["identityJson"].is_string(),
                "case {case:?}: expected the field's own value, got {answered}"
            );
            continue;
        }
        assert_eq!(
            (status, named.as_deref()),
            (401, Some(UNAUTHORIZED_ERROR_TYPE)),
            "case {case:?}: a call the policies permit no field of is refused 401 naming its \
             class, got {status}, {named:?} and {answered}"
        );
        assert_eq!(
            (
                answered["errors"][0]["errorType"].as_str(),
                answered["errors"][0]["message"].as_str(),
                answered.get("data"),
            ),
            (Some(UNAUTHORIZED_ERROR_TYPE), Some(PERMISSION_DENIED), None),
            "case {case:?}: expected the authorization refusal and no data, got {answered}"
        );
    }
}
