// SPDX-License-Identifier: CC0-1.0

//! The MESSAGE ATTRIBUTES an `AWS`-type integration request template writes, and the two kinds
//! of client whose request they are computed from: a client that signs the invoke itself, and
//! an EventBridge rule whose target is that same method of that same deployed stage.
//!
//! RULE: an `AWS`-type integration request template renders the WHOLE body of the query-api
//! call the stage makes, so every field it writes travels as one parameter of that call —
//! `MessageBody` as the message's body, and each `MessageAttribute.{n}.Name`,
//! `.Value.DataType` and `.Value.StringValue` as one message attribute of it — and every
//! `$input`/`$context` reference in ANY of those fields resolves against the request the stage
//! received: `$input.body` the body it carried, `$input.params()` its path and header
//! parameters, `$context.resourcePath` the resource it matched. WHICH client made that request
//! does not enter into it: a request a client signed itself and a request an EventBridge target
//! made — its body rendered by the target's `InputTransformer`, the `*` of its ARN's path filled
//! from `HttpParameters.PathParameterValues` and its headers contributed by
//! `HttpParameters.HeaderParameters` — are the same request to the method, so they are mapped
//! the same way and each produces its own message.
//!
//! Only a cross-service suite can pin this: the queue and the attributes on the message it holds
//! are Amazon SQS's, the roles that decide the integration and the delivery are IAM's, the api,
//! the stage and the integration's templates are API Gateway's, and the rule and its target are
//! EventBridge's. The setup calls are the ones [`crate::events_rest_api_stage_target`] already
//! makes for that span, imported rather than written again; what is this suite's own is the
//! method whose template writes ATTRIBUTES rather than one document, and the signed client that
//! reaches it beside the rule.
//!
//! The rows vary what the rule generalises over: the CLIENT (a signed caller of the stage, and a
//! rule's target), the VALUE each reference resolves to (plain text, text carrying every
//! character that is significant in the form body the template renders, text outside ASCII), the
//! SOURCE a field reads (the path, a header, the matched resource, and a literal reading nothing
//! at all), and the SHAPE of the body the request carried (a flat document, one carrying a
//! nested array).

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::events_rest_api_stage_target::{
    apigw, attribute, emulator, events, messages, policy, queue, resource, role, sqs,
    APIGATEWAY_TRUST, DETAIL_TYPE, EVENTS_TRUST, JSON, REGION, SOURCE_HEADER, STAGE,
};

/// The long-term key every emulator publishes, which the signed invoke below carries. It
/// resolves to no session and so speaks for the account's own identity; the signature itself is
/// never checked, because this router runs with request authentication off as every suite in
/// this crate runs it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The `source` every event this suite publishes carries, and what its rule matches on.
const SOURCE: &str = "sim2real.attributes";

/// The body the method's integration response renders, which is what a client reads when the
/// queue call succeeded: a fixed document, so what a row asserts about the message is the
/// message's own.
const STAGE_BODY: &str = r#"{"stage_status": 200}"#;

/// The resource path of the method every row reaches, as the api declares it — and so what
/// `$context.resourcePath` resolves to, which the `route` attribute carries.
const RESOURCE_PATH: &str = "/orders/{id}";

/// The integration request template of that method: one Amazon SQS `SendMessage` in the
/// `application/x-www-form-urlencoded` form its query api carries, whose body is the request's
/// own and whose attributes are read one per source the parameter map has — the path, a header,
/// the matched resource — plus one written as a literal, which reads nothing of the request at
/// all. Each value is form-encoded as it is written, so the field carries it whole.
const ATTRIBUTES_TEMPLATE: &str = concat!(
    "Action=SendMessage",
    "&MessageBody=$util.urlEncode($input.body)",
    "&MessageAttribute.1.Name=pathId",
    "&MessageAttribute.1.Value.DataType=String",
    "&MessageAttribute.1.Value.StringValue=$util.urlEncode($input.params().path.get('id'))",
    "&MessageAttribute.2.Name=orderSrc",
    "&MessageAttribute.2.Value.DataType=String",
    // The header is spelled here as the clients below send it, which is `SOURCE_HEADER`.
    "&MessageAttribute.2.Value.StringValue=\
     $util.urlEncode($input.params().header.get('x-order-src'))",
    "&MessageAttribute.3.Name=route",
    "&MessageAttribute.3.Value.DataType=String",
    "&MessageAttribute.3.Value.StringValue=$util.urlEncode($context.resourcePath)",
    "&MessageAttribute.4.Name=via",
    "&MessageAttribute.4.Value.DataType=String",
    "&MessageAttribute.4.Value.StringValue=stage",
);

/// What the literal field of the template above carries, whichever request is mapped.
const VIA: &str = "stage";

/// One stage this suite deploys, and everything the rows address it by.
struct Stage {
    api: String,
    account: String,
    sink_url: String,
    /// A role EventBridge can assume that may invoke every `POST` of this stage.
    invoker: String,
}

impl Stage {
    /// The invoke host of this api, which is the address a client of the stage dials.
    fn host(&self) -> String {
        format!("{}.execute-api.{REGION}.amazonaws.com", self.api)
    }

    /// The ARN a target names every `POST` under `/orders` of this stage by, whose one `*` the
    /// target's own path parameter fills.
    fn target_arn(&self) -> String {
        format!(
            "arn:aws:execute-api:{REGION}:{}:{}/{STAGE}/POST/orders/*",
            self.account, self.api
        )
    }
}

/// The api, its one method and the roles every row below uses: a `POST /orders/{id}` authorized
/// by IAM whose `AWS` integration sends [`ATTRIBUTES_TEMPLATE`] to the sink queue.
async fn deployed(router: &MultiServiceRouter) -> Stage {
    let (sink_url, sink_arn) = queue(router, "sink", "stage-attributes-sink").await;
    let account = sink_arn.split(':').nth(4).unwrap_or_default().to_string();
    let sender = role(
        router,
        "stage-attributes-sender",
        APIGATEWAY_TRUST,
        Some(&policy("sqs:SendMessage", &sink_arn)),
    )
    .await;

    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "stage-attributes" })),
        201,
    )
    .await;
    let api = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateRestApi: no id in {created}"))
        .to_string();
    let root = created["rootResourceId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateRestApi: no rootResourceId in {created}"))
        .to_string();

    let parent = resource(router, &api, &root, "orders").await;
    let child = resource(router, &api, &parent, "{id}").await;
    let method = format!("/restapis/{api}/resources/{child}/methods/POST");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({
            "authorizationType": "AWS_IAM",
            "requestParameters": { "method.request.path.id": true },
        })),
        201,
    )
    .await;
    let sink_uri = format!(
        "arn:aws:apigateway:{REGION}:sqs:path/{account}/{}",
        sink_url.rsplit('/').next().unwrap_or_default()
    );
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports for it.
            "httpMethod": "POST",
            "uri": sink_uri,
            "credentials": sender,
            "requestParameters": {
                "integration.request.header.Content-Type":
                    "'application/x-www-form-urlencoded'",
            },
            "requestTemplates": { JSON: ATTRIBUTES_TEMPLATE },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({
            "statusCode": "200",
            "responseTemplates": { JSON: STAGE_BODY },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    let invoker = role(
        router,
        "stage-attributes-invoker",
        EVENTS_TRUST,
        Some(&policy(
            "execute-api:Invoke",
            &format!("arn:aws:execute-api:{REGION}:{account}:{api}/{STAGE}/POST/*"),
        )),
    )
    .await;
    Stage {
        api,
        account,
        sink_url,
        invoker,
    }
}

/// One request to the stage's invoke endpoint, signed by the client itself, as its status and
/// body text.
async fn invoke(
    router: &MultiServiceRouter,
    stage: &Stage,
    id: &str,
    source: &str,
    body: &str,
) -> (u16, String) {
    let host = stage.host();
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/execute-api/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/{STAGE}/orders/{id}"))
        .header("host", &host)
        .header("authorization", authorization)
        .header("content-type", JSON)
        .header(SOURCE_HEADER, source)
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, text)
}

/// Every attribute of one message that this suite's template writes, so a row asserts the whole
/// set rather than one of them.
fn attributes(message: &Value) -> Value {
    json!({
        "pathId": attribute(message, "pathId"),
        "orderSrc": attribute(message, "orderSrc"),
        "route": attribute(message, "route"),
        "via": attribute(message, "via"),
    })
}

/// What the template owes a request carrying `id` in its path and `source` as its header.
fn expected_attributes(id: &str, source: &str) -> Value {
    json!({
        "pathId": id,
        "orderSrc": source,
        "route": RESOURCE_PATH,
        "via": VIA,
    })
}

/// Take the one message `url` holds for `case`, and leave the queue empty for the next row.
async fn one_message(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let held = messages(router, case, url, 1).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected exactly the one message this request made, got {held:?}"
    );
    sqs(
        router,
        case,
        "DeleteMessage",
        &json!({
            "QueueUrl": url,
            "ReceiptHandle": held[0]["ReceiptHandle"].as_str().unwrap_or_default(),
        }),
    )
    .await;
    held[0].clone()
}

/// One row of the signed-client table: what the client sent, which is what the template maps.
struct Signed {
    case: &'static str,
    id: &'static str,
    source: &'static str,
    body: &'static str,
}

#[tokio::test]
async fn a_signed_request_to_the_stage_is_mapped_into_the_queues_message_attributes() {
    let router = emulator();
    let stage = deployed(&router).await;

    let rows: &[Signed] = &[
        Signed {
            case: "plain values in the path, the header and the body",
            id: "w-1",
            source: "runner",
            body: r#"{"marker":"warm-1"}"#,
        },
        Signed {
            case: "a header value carrying every character a form body treats specially",
            id: "ORD.9_x",
            source: "a&b=c+d%e f",
            body: r#"{"marker":"warm-2","note":"a&b=c+d%e f"}"#,
        },
        Signed {
            case: "a body outside ASCII, carrying a nested array",
            id: "ord-abc123",
            source: "checkout-web",
            body: r#"{"marker":"café ☕","lines":[{"sku":"A-1","qty":2}]}"#,
        },
    ];

    for row in rows {
        let (status, text) = invoke(&router, &stage, row.id, row.source, row.body).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the integration's 200, got {status} with {text:?}",
            row.case
        );
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
            serde_json::from_str::<Value>(STAGE_BODY).unwrap_or(Value::Null),
            "case {:?}: expected the method response's own body, got {text:?}",
            row.case
        );
        let message = one_message(&router, row.case, &stage.sink_url).await;
        // `MessageBody` is the request's own body, whole: every character of it survives the
        // form field that carried it.
        assert_eq!(
            message["Body"].as_str(),
            Some(row.body),
            "case {:?}: expected the request's own body as the message's, got {message}",
            row.case
        );
        assert_eq!(
            attributes(&message),
            expected_attributes(row.id, row.source),
            "case {:?}: expected the attributes the template maps, got {message}",
            row.case
        );
    }
}

/// One row of the delivery table: the detail the event carries, which is what the target's
/// transformer renders and what its parameters are read out of.
struct Delivered {
    case: &'static str,
    detail: Value,
    /// The `id` member of that detail, which fills the `*` of the target's ARN.
    id: &'static str,
    /// Its `src` member, which the target contributes as [`SOURCE_HEADER`].
    source: &'static str,
}

#[tokio::test]
async fn a_rules_delivery_to_the_stage_is_mapped_the_same_way_a_signed_request_is() {
    let router = emulator();
    let stage = deployed(&router).await;
    events(
        &router,
        "the delivering rule",
        "PutRule",
        &json!({
            "Name": "stage-attributes-rule",
            "EventPattern": json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
            })
            .to_string(),
        }),
    )
    .await;
    let wired = events(
        &router,
        "the delivering rule",
        "PutTargets",
        &json!({
            "Rule": "stage-attributes-rule",
            "Targets": [{
                "Id": "stage",
                "Arn": stage.target_arn(),
                "RoleArn": stage.invoker,
                "HttpParameters": {
                    // Each value names a place in the EVENT, so one rule addresses a
                    // different resource and sends a different header per event.
                    "PathParameterValues": ["$.detail.id"],
                    "HeaderParameters": { SOURCE_HEADER: "$.detail.src" },
                },
                "InputTransformer": {
                    "InputPathsMap": { "d": "$.detail", "t": "$.detail-type" },
                    "InputTemplate": r#"{"wrapped": <d>, "kind": <t>}"#,
                },
            }],
        }),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "setup PutTargets: expected the target to be accepted, got {wired}"
    );

    let rows: &[Delivered] = &[
        Delivered {
            case: "a detail naming the path and the header, carrying a nested array",
            detail: json!({
                "id": "ord-abc123",
                "src": "checkout-web",
                "amount": 42,
                "lines": [{ "sku": "A-1", "qty": 2 }],
            }),
            id: "ord-abc123",
            source: "checkout-web",
        },
        Delivered {
            case: "another detail, naming another path and header",
            detail: json!({ "id": "w-1", "src": "runner" }),
            id: "w-1",
            source: "runner",
        },
        Delivered {
            case: "a detail whose header value carries characters a form body treats \
                   specially",
            detail: json!({ "id": "ORD.9_x", "src": "a&b=c+d%e f" }),
            id: "ORD.9_x",
            source: "a&b=c+d%e f",
        },
    ];

    for row in rows {
        let published = events(
            &router,
            row.case,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": row.detail.to_string(),
                }],
            }),
        )
        .await;
        assert_eq!(
            published["FailedEntryCount"],
            json!(0),
            "case {:?}: expected the entry to be accepted, got {published}",
            row.case
        );
        let message = one_message(&router, row.case, &stage.sink_url).await;
        // The body the target asked to be delivered: the transformer's own rendering, carrying
        // exactly the two members its template names and nothing of the envelope.
        assert_eq!(
            message["Body"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok()),
            Some(json!({ "wrapped": row.detail, "kind": DETAIL_TYPE })),
            "case {:?}: expected the transformed event as the message's body, got {message}",
            row.case
        );
        assert_eq!(
            attributes(&message),
            expected_attributes(row.id, row.source),
            "case {:?}: expected the attributes the template maps, got {message}",
            row.case
        );
    }
}
