// SPDX-License-Identifier: CC0-1.0

//! The PARAMETER MAP of a method request, as an `AWS`-type integration's request template
//! reads it: the whole path from the stage invoke URL to the message the queue behind it
//! holds, across three services on one engine.
//!
//! RULE: `$input.params()` called with NO argument names the map of the method request's
//! parameters keyed by the source each came from — the path, the query string and the
//! headers, the same three sources `$input.params(name)` reads — so a reference that reads
//! into that map, by a source and then a name, resolves to that parameter's value, and a name
//! no parameter of the call has resolves to nothing. The map is one value of the call like
//! every other, so it is read the same way wherever a template names it: directly
//! (`$input.params().path.get('id')`, `$input.params().querystring.tier`) or bound by a
//! `#set` first, which is the form every documented example of this reference opens with.
//! A template this emulator cannot render is an integration it cannot perform, answered
//! through the api's `API_CONFIGURATION_ERROR` gateway response (HTTP 500,
//! `{"message":"Internal server error"}`) — so a template reading the parameter map decided
//! whether the stage answers the backend's response at all.
//!
//! Only a cross-service suite can pin this end to end: the queue and the message it holds are
//! Amazon SQS's, the role the integration assumes and its policies are IAM's, and the api,
//! the stage and the integration's templates are API Gateway's, so what the rendered template
//! carried is observable only where all three are mounted on one engine. The crate's own
//! tests pin the rendering of each reference on its own.
//!
//! The rows vary what the rule generalises over: WHICH source the parameter came from (a path
//! parameter, a query-string parameter, a header), HOW the template reaches the map (directly,
//! and through a `#set` binding of it), the CASE a header name is written in against the case
//! it was sent in, whether the parameter was sent at all, and a value carrying every character
//! that is significant in the form body the template renders.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for, and the Region the integration ARNs name.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is what the setup calls are signed
/// with. The signature itself is never checked: this router runs with request
/// authentication off, as every suite in this crate runs it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The queue every row below delivers to.
const QUEUE: &str = "parameter-map-orders";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The media type the method requests below declare, which is the key their integration
/// request template is stored under.
const JSON: &str = "application/json";

/// A trust policy that lets API Gateway assume the role it is attached to.
const APIGATEWAY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"apigateway.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The header whose value the templates below read out of the parameter map, named as the
/// templates spell it.
const SOURCE_HEADER: &str = "x-order-src";

/// The query-string parameter the templates below read out of the parameter map.
const TIER_PARAMETER: &str = "tier";

/// The body the integration response renders, which is what a client reads when the queue
/// call succeeded: a fixed document, so what the row asserts about the message is the
/// message's own.
const STAGE_BODY: &str = r#"{"stage_status": 200}"#;

/// The integration request template of the resource that reads the parameter map DIRECTLY:
/// one `SendMessage` whose body is a document built from the map, and one message attribute
/// read out of it, in the `application/x-www-form-urlencoded` form Amazon SQS's query api
/// carries. Each value is form-encoded as it is written, so the field carries it whole.
const DIRECT_TEMPLATE: &str = concat!(
    "Action=SendMessage&MessageBody={",
    r#""pathId":"$util.urlEncode($input.params().path.get('id'))","#,
    r#""pathMember":"$util.urlEncode($input.params().path.id)","#,
    r#""src":"$util.urlEncode($input.params().header.get('x-order-src'))","#,
    r#""tier":"$util.urlEncode($input.params().querystring.tier)","#,
    r#""absent":"$util.urlEncode($input.params().path.get('nothing-sent-this'))","#,
    r#""oneArgument":"$util.urlEncode($input.params('id'))","#,
    r#""route":"$util.urlEncode($context.resourcePath)""#,
    "}",
    "&MessageAttribute.1.Name=pathId",
    "&MessageAttribute.1.Value.DataType=String",
    "&MessageAttribute.1.Value.StringValue=$util.urlEncode($input.params().path.get('id'))",
);

/// The same integration request, with the parameter map BOUND by a `#set` first and read
/// through that binding — the form every documented example of this reference opens with.
/// The header is named exactly as the rows that reach this resource send it, because a
/// binding is read by the keys the map carries.
const BOUND_TEMPLATE: &str = concat!(
    "#set($all = $input.params())",
    "Action=SendMessage&MessageBody={",
    r#""pathId":"$util.urlEncode($all.path.id)","#,
    r#""pathMember":"$util.urlEncode($all.get('path').get('id'))","#,
    r#""src":"$util.urlEncode($all.header.get('x-order-src'))","#,
    r#""tier":"$util.urlEncode($all.querystring.get('tier'))","#,
    r#""absent":"$util.urlEncode($all.path.get('nothing-sent-this'))","#,
    r#""oneArgument":"$util.urlEncode($input.params('id'))","#,
    r#""route":"$util.urlEncode($context.resourcePath)""#,
    "}",
    "&MessageAttribute.1.Name=pathId",
    "&MessageAttribute.1.Value.DataType=String",
    "&MessageAttribute.1.Value.StringValue=$util.urlEncode($all.path.get('id'))",
);

/// What one call answered.
struct Answer {
    status: u16,
    body: Bytes,
}

impl Answer {
    /// The body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, `Null` when it is neither JSON nor present.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The three services this rule spans, over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// One API Gateway control-plane call that has to succeed with `expected`.
async fn apigw_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answer = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", JSON),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answer.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that has to succeed.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let answer = dispatch(
        router,
        "POST",
        &format!("sqs.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", &format!("AmazonSQS.{operation}")),
            ("authorization", &authorization("sqs")),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// One IAM awsQuery call that has to succeed, answering its body.
async fn iam_ok(router: &MultiServiceRouter, case: &str, body: String) -> String {
    let answer = dispatch(
        router,
        "POST",
        "iam.amazonaws.com",
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("iam")),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup {case:?}: expected HTTP 200 from IAM, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.text()
}

/// The member `name` of a JSON object, as text.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// The `{account-id}/{queue-name}` tail of a queue url, which is what the `path` form of an
/// integration `uri` names a queue by — read off the url Amazon SQS itself handed back, so no
/// account id is written here.
fn queue_path(url: &str) -> String {
    let mut segments = url.trim_end_matches('/').rsplit('/');
    let name = segments.next().unwrap_or_default();
    let account = segments.next().unwrap_or_default();
    format!("{account}/{name}")
}

/// One resource of the api under `parent`, answering its id.
async fn resource(router: &MultiServiceRouter, api: &str, parent: &str, part: &str) -> String {
    let created = apigw_ok(
        router,
        &format!("CreateResource({part})"),
        "POST",
        &format!("/restapis/{api}/resources/{parent}"),
        Some(json!({ "pathPart": part })),
        201,
    )
    .await;
    member(&format!("CreateResource({part})"), &created, "id")
}

/// The `POST /{part}/{{id}}` method of the api, whose `AWS` integration sends `template` to
/// the queue `uri` names under `credentials`, and whose integration response renders
/// [`STAGE_BODY`].
async fn parameter_map_method(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
    part: &str,
    uri: &str,
    credentials: &str,
    template: &str,
) {
    let parent = resource(router, api, root, part).await;
    let child = resource(router, api, &parent, "{id}").await;
    let case = format!("POST /{part}/{{id}}");
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{child}/methods/POST"),
        Some(json!({
            "authorizationType": "NONE",
            "requestParameters": { "method.request.path.id": true },
        })),
        201,
    )
    .await;
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{child}/methods/POST/integration"),
        Some(json!({
            "type": "AWS",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports for it.
            "httpMethod": "POST",
            "uri": uri,
            "credentials": credentials,
            "passthroughBehavior": "NEVER",
            "requestParameters": {
                "integration.request.header.Content-Type":
                    "'application/x-www-form-urlencoded'",
            },
            "requestTemplates": { JSON: template },
        })),
        201,
    )
    .await;
    // The default integration response, which maps the success, and one per class of refusal,
    // so a refusal is answered with the status the service gave rather than one status for
    // every outcome.
    for (status, pattern) in [("200", None), ("403", Some("403")), ("500", Some(r"5\d{2}"))] {
        apigw_ok(
            router,
            &case,
            "PUT",
            &format!("/restapis/{api}/resources/{child}/methods/POST/responses/{status}"),
            Some(json!({})),
            201,
        )
        .await;
        let mut response = json!({ "statusCode": status });
        if let Some(pattern) = pattern {
            response["selectionPattern"] = json!(pattern);
        } else {
            response["responseTemplates"] = json!({ JSON: STAGE_BODY });
        }
        apigw_ok(
            router,
            &case,
            "PUT",
            &format!(
                "/restapis/{api}/resources/{child}/methods/POST/integration/responses/{status}"
            ),
            Some(response),
            201,
        )
        .await;
    }
}

/// One row: how the request is addressed, and so what the integration request must carry.
struct Row {
    case: &'static str,
    /// The resource the request is sent to, which decides HOW its template reaches the
    /// parameter map: `orders` reads it directly, `bound` through a `#set` binding of it.
    resource: &'static str,
    /// The path parameter the resource binds.
    id: &'static str,
    /// The header the request sends, by the name it is sent under and its value — `None` for
    /// a request that sends none at all.
    header: Option<(&'static str, &'static str)>,
    /// The query-string parameter the request sends, or `None` for one that sends none.
    tier: Option<&'static str>,
}

#[tokio::test]
async fn the_parameter_map_carries_each_sent_parameter_into_the_integration_request() {
    let router = emulator();
    let queue_url = member(
        "CreateQueue",
        &sqs_ok(
            &router,
            "CreateQueue",
            "CreateQueue",
            &json!({ "QueueName": QUEUE }),
        )
        .await,
        "QueueUrl",
    );
    let queue_arn = member(
        "GetQueueAttributes",
        sqs_ok(
            &router,
            "CreateQueue",
            "GetQueueAttributes",
            &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
        )
        .await
        .get("Attributes")
        .unwrap_or(&Value::Null),
        "QueueArn",
    );

    // One role this api can assume that may send to this queue, written by IAM's own
    // `CreateRole`/`PutRolePolicy`.
    let created = iam_ok(
        &router,
        "CreateRole",
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName=parameter-map-sender\
             &AssumeRolePolicyDocument={APIGATEWAY_TRUST}"
        ),
    )
    .await;
    let role = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup CreateRole: no <Arn> in {created}"));
    iam_ok(
        &router,
        "PutRolePolicy",
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName=parameter-map-sender\
             &PolicyName=queue-integration&PolicyDocument={}",
            format_args!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["sqs:SendMessage"],"Resource":"{queue_arn}"}}]}}"#
            )
        ),
    )
    .await;

    let api_body = apigw_ok(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "parameter-map" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api_body, "id");
    let root = member("CreateRestApi", &api_body, "rootResourceId");
    let uri = format!(
        "arn:aws:apigateway:{REGION}:sqs:path/{}",
        queue_path(&queue_url)
    );
    parameter_map_method(&router, &api, &root, "orders", &uri, &role, DIRECT_TEMPLATE).await;
    parameter_map_method(&router, &api, &root, "bound", &uri, &role, BOUND_TEMPLATE).await;
    apigw_ok(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    let rows: &[Row] = &[
        Row {
            case: "every parameter sent, the header named as the template spells it",
            resource: "orders",
            id: "w-1",
            header: Some((SOURCE_HEADER, "runner")),
            tier: Some("gold"),
        },
        Row {
            case: "the header sent in another case, which a header name is matched in",
            resource: "orders",
            id: "ord-abc123",
            header: Some(("X-ORDER-SRC", "checkout-web")),
            tier: Some("silver"),
        },
        Row {
            case: "no header at all, which is an absent value",
            resource: "orders",
            id: "ord-no-header",
            header: None,
            tier: Some("bronze"),
        },
        Row {
            case: "no query-string parameter at all",
            resource: "orders",
            id: "ord-no-query",
            header: Some((SOURCE_HEADER, "runner")),
            tier: None,
        },
        Row {
            case: "a value carrying every character significant in a form body",
            resource: "orders",
            id: "ord-form-chars",
            header: Some((SOURCE_HEADER, "a&b=c+d%e")),
            tier: Some("gold"),
        },
        Row {
            case: "the same map reached through a #set binding of it",
            resource: "bound",
            id: "w-2",
            header: Some((SOURCE_HEADER, "runner")),
            tier: Some("gold"),
        },
        Row {
            case: "through the binding with neither optional parameter sent",
            resource: "bound",
            id: "w-3",
            header: None,
            tier: None,
        },
    ];

    for row in rows {
        let query = row
            .tier
            .map(|tier| format!("?{TIER_PARAMETER}={tier}"))
            .unwrap_or_default();
        let headers: Vec<(&str, &str)> = [("content-type", JSON)]
            .into_iter()
            .chain(row.header)
            .collect();
        let answered = dispatch(
            &router,
            "POST",
            &format!("{api}.execute-api.{REGION}.amazonaws.com"),
            &format!("/{STAGE}/{}/{}{query}", row.resource, row.id),
            &headers,
            Bytes::from_static(b"{}"),
        )
        .await;
        let text = answered.text();
        assert_eq!(
            answered.status, 200,
            "case {:?}: expected the stage to answer the queue call it made, got {} with \
             {text:?}",
            row.case, answered.status
        );
        assert_eq!(
            answered.json(),
            serde_json::from_str::<Value>(STAGE_BODY).expect("a JSON response template"),
            "case {:?}: expected the integration response's own body, got {text:?}",
            row.case
        );

        // What the queue HOLDS, read through Amazon SQS itself: the document the template
        // rendered out of the parameter map.
        let received = sqs_ok(
            &router,
            row.case,
            "ReceiveMessage",
            &json!({
                "QueueUrl": queue_url,
                "MaxNumberOfMessages": 10,
                "MessageAttributeNames": ["All"],
            }),
        )
        .await;
        let messages = received
            .get("Messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            messages.len(),
            1,
            "case {:?}: expected exactly the one message this row sent, got {received}",
            row.case
        );
        let body: Value = serde_json::from_str(&member(row.case, &messages[0], "Body"))
            .unwrap_or_else(|_| {
                panic!(
                    "case {:?}: expected the rendered document the template wrote, got {}",
                    row.case, messages[0]
                )
            });
        let expected = json!({
            // The path parameter, by `get(name)` and by the bare member spelling.
            "pathId": row.id,
            "pathMember": row.id,
            // The header, matched however its name was written.
            "src": row.header.map(|(_, value)| value).unwrap_or_default(),
            // The query-string parameter.
            "tier": row.tier.unwrap_or_default(),
            // A name no parameter of this call has, which resolves to nothing.
            "absent": "",
            // The same path parameter through the one-argument form of the same reference.
            "oneArgument": row.id,
            // The resource path the deployment matched, which names the parameter.
            "route": format!("/{}/{{id}}", row.resource),
        });
        assert_eq!(
            body, expected,
            "case {:?}: expected the parameters this request sent, got {body}",
            row.case
        );
        assert_eq!(
            messages[0]
                .get("MessageAttributes")
                .and_then(|attributes| attributes.get("pathId"))
                .and_then(|attribute| attribute.get("StringValue"))
                .and_then(Value::as_str),
            Some(row.id),
            "case {:?}: expected the path parameter as the message's own attribute, got {}",
            row.case,
            messages[0]
        );
        sqs_ok(
            &router,
            row.case,
            "DeleteMessage",
            &json!({
                "QueueUrl": queue_url,
                "ReceiptHandle": member(row.case, &messages[0], "ReceiptHandle"),
            }),
        )
        .await;
    }
}
