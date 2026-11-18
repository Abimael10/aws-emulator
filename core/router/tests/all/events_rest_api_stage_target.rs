// SPDX-License-Identifier: CC0-1.0

//! An EventBridge rule whose TARGET is a deployed API Gateway REST API stage: the whole path
//! an event takes from `PutEvents` to the request the stage serves, across four services on
//! one engine.
//!
//! RULE: a target whose ARN is of the `execute-api` service names the invoke endpoint of one
//! method of one deployed stage
//! (`arn:aws:execute-api:{region}:{account}:{api-id}/{stage}/{METHOD}/{resource-path}`, the
//! form `execute-api:Invoke` is granted on), and a rule whose `EventPattern` matched hands
//! the event to it BY MAKING THAT REQUEST: the ARN's own verb at that stage's invoke URL,
//! carrying what the target receives as the body — the whole envelope, or the rendering of
//! the target's `InputTransformer`. The target's `HttpParameters` are what the caller
//! contributes to that request ("the parameters needed for you to contribute to the HTTP
//! request"): each `PathParameterValues` entry fills one `*` of the ARN's resource path in
//! order, each `HeaderParameters` entry is sent as that request header, and a value of either
//! that names a place in the event stands for what the event carries there. EventBridge
//! invokes such a target AS THE ROLE it names — "for EC2 instances, Kinesis data streams,
//! Step Functions state machines and API Gateway APIs, EventBridge relies on IAM roles that
//! you specify in the `RoleARN` argument in `PutTargets`" — so a role whose policies do not
//! allow `execute-api:Invoke` on that ARN is a delivery EventBridge withholds, and the event
//! goes to the target's dead-letter queue carrying `NO_PERMISSIONS`. A stage that ANSWERS and
//! does not take the event is the other class: a 5xx is retried within the target's
//! `RetryPolicy` and then dead-lettered with the status the stage answered, while a matched
//! event the stage served is delivered and dead-letters nothing.
//!
//! Only a cross-service suite can pin this: the rule, its target and the dead letter are
//! EventBridge's, the stage and its integration are API Gateway's, the role that decides the
//! delivery is IAM's, and the sink the stage writes into — which is what shows WHAT reached
//! the stage, rather than what EventBridge said about it — is Amazon SQS's.
//!
//! The rows vary what the rule generalises over: the DETAIL the event carries (so the path
//! and header the delivery contributes are the event's, not one recorded value), whether the
//! event matches the rule at all (a `detail-type` differing only in letter case is not the
//! rule's), and how the delivery ends — served, withheld for want of the invoke grant, and
//! answered 5xx by the stage and retried.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and the Region the ARNs name.
const REGION: &str = "us-west-2";

/// The long-term key every emulator publishes, which the setup calls are signed with. The
/// signature itself is never checked: this router runs with request authentication off, as
/// every suite in this crate runs it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The `source` every event this suite publishes carries, and what the rule matches on.
const SOURCE: &str = "sim2real.orders";

/// The `detail-type` the rule matches, exactly as it is written: EventBridge compares it
/// case-sensitively, which the near-miss row below is the differential for.
const DETAIL_TYPE: &str = "OrderPlaced";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The media type the method request declares, which keys its integration request template.
const JSON: &str = "application/json";

/// The header the target contributes to the request, which the stage records.
const SOURCE_HEADER: &str = "x-order-src";

/// The principal EventBridge acts as when it writes a dead letter, which the dead-letter
/// queue's own policy has to admit.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// A trust policy that lets EventBridge assume the role it is attached to.
const EVENTS_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"events.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// A trust policy that lets API Gateway assume the role it is attached to, which is what the
/// stage's own integration is performed under.
const APIGATEWAY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"apigateway.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The condition EventBridge names on a dead letter for a delivery it WITHHELD because no
/// document admits the invocation, spelled here rather than imported from the service so a
/// wrong value in the service would fail this suite.
const NO_PERMISSIONS: &str = "NO_PERMISSIONS";

/// The condition it names for a delivery to an endpoint that did not answer the request
/// successfully — here the stage's own 5xx answer, which the `ERROR_MESSAGE` beside it
/// carries. Spelled here for the same reason.
const CONNECTION_FAILURE: &str = "CONNECTION_FAILURE";

/// The dead-letter attribute that says how many retries a delivery made, spelled here for the
/// same reason.
const RETRY_ATTEMPTS: &str = "RETRY_ATTEMPTS";

/// How many retries the 5xx target allows, which is what its dead letter has to report.
const RETRIES_ALLOWED: i64 = 2;

/// The integration request template of the method the rule delivers to: one SQS `SendMessage`
/// whose body is a document built from the request the stage received, so what the sink holds
/// says what the delivery carried. Each value is form-encoded as it is written, so the field
/// carries it whole.
const SINK_TEMPLATE: &str = concat!(
    "Action=SendMessage&MessageBody={",
    // The delivered body goes inside a string of the document this template writes, so it is
    // escaped for one before it is form-encoded for the field that carries it.
    r#""delivered":"$util.urlEncode($util.escapeJavaScript($input.body))","#,
    r#""pathId":"$util.urlEncode($input.params().path.get('id'))","#,
    r#""orderSrc":"$util.urlEncode($input.params().header.get('x-order-src'))","#,
    r#""route":"$util.urlEncode($context.resourcePath)""#,
    "}",
);

/// The four services this rule spans, over ONE engine.
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
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
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

/// One JSON-RPC call of an emulated service, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = match response.body().is_empty() {
        true => Value::Null,
        false => serde_json::from_slice(response.body()).unwrap_or(Value::Null),
    };
    (status, parsed)
}

/// An EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// An Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One IAM awsQuery call that must answer 200, as its body.
async fn iam(router: &MultiServiceRouter, case: &str, body: String) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup {case:?}: expected HTTP 200 from IAM, got {status} with {text:?}"
    );
    text
}

/// One API Gateway control-plane call that must answer `expected`, as its parsed body.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("authorization", authorization("apigateway"))
        .header("content-type", JSON)
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {status} with \
         {text:?}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// One IAM role: created with `trust` as its trust policy and `document` as its one inline
/// policy, answering its ARN.
async fn role(
    router: &MultiServiceRouter,
    name: &str,
    trust: &str,
    document: &str,
) -> String {
    let created = iam(
        router,
        &format!("CreateRole({name})"),
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}\
             &AssumeRolePolicyDocument={trust}"
        ),
    )
    .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup CreateRole({name}): no <Arn> in {created}"));
    iam(
        router,
        &format!("PutRolePolicy({name})"),
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={name}\
             &PolicyName=inline&PolicyDocument={document}"
        ),
    )
    .await;
    arn
}

/// One identity-based policy allowing `action` on `resource`, written with none of the
/// characters a form body treats specially.
fn policy(action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// One queue of this account: its url and the ARN a target names it by, read back from Amazon
/// SQS rather than built here.
async fn queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {case:?}: expected a QueueArn, got {attributes}"))
        .to_string();
    (url, arn)
}

/// One resource of the api under `parent`, answering its id.
async fn resource(router: &MultiServiceRouter, api: &str, parent: &str, part: &str) -> String {
    let created = apigw(
        router,
        &format!("CreateResource({part})"),
        "POST",
        &format!("/restapis/{api}/resources/{parent}"),
        Some(json!({ "pathPart": part })),
        201,
    )
    .await;
    created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateResource({part}): no id in {created}"))
        .to_string()
}

/// A `POST /{part}/{{id}}` method whose `AWS` integration sends `SINK_TEMPLATE` to the queue
/// `uri` names under `credentials`, answering nothing of its own.
async fn sink_method(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
    part: &str,
    authorization_type: &str,
    uri: &str,
    credentials: &str,
) {
    let parent = resource(router, api, root, part).await;
    let child = resource(router, api, &parent, "{id}").await;
    let case = format!("POST /{part}/{{id}}");
    apigw(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{child}/methods/POST"),
        Some(json!({ "authorizationType": authorization_type })),
        201,
    )
    .await;
    apigw(
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
            "requestParameters": {
                "integration.request.header.Content-Type":
                    "'application/x-www-form-urlencoded'",
            },
            "requestTemplates": { JSON: SINK_TEMPLATE },
        })),
        201,
    )
    .await;
    apigw(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{child}/methods/POST/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{child}/methods/POST/integration/responses/200"),
        Some(json!({ "statusCode": "200" })),
        201,
    )
    .await;
}

/// How long a poll for what a queue holds keeps looking before it gives up: generous, because
/// what it waits for is a condition and not a duration — every delivery this suite makes is
/// made within the `PutEvents` that triggered it.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The messages the queue at `url` holds, polled until there are `wanted` of them or
/// [`POLL_DEADLINE`] passes — so a row asserting an EMPTY queue reads it once the deliveries
/// that would have filled it have had every chance to.
async fn messages(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = ::std::time::Instant::now() + POLL_DEADLINE;
    let mut held: Vec<Value> = Vec::new();
    loop {
        let answer = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({
                "QueueUrl": url,
                "MaxNumberOfMessages": 10,
                "MessageAttributeNames": ["All"],
            }),
        )
        .await;
        held.extend(
            answer["Messages"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
        if held.len() >= wanted || ::std::time::Instant::now() >= deadline {
            return held;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The value of the message attribute `name`, or `None` when the message carries none.
fn attribute(message: &Value, name: &str) -> Option<String> {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(str::to_string)
}

/// One stage this suite deploys, and everything the rows address it by.
struct Deployed {
    api: String,
    account: String,
    sink_url: String,
    dead_letter_url: String,
    dead_letter_arn: String,
    /// A role EventBridge can assume that may invoke every method of this api's stage.
    invoker: String,
    /// A role it can assume whose grant names another api's methods, so it allows no invoke
    /// of this one.
    stranger: String,
}

impl Deployed {
    /// The ARN a target names one method of this stage by.
    fn target_arn(&self, path: &str) -> String {
        format!(
            "arn:aws:execute-api:{REGION}:{}:{}/{STAGE}/POST/{path}",
            self.account, self.api
        )
    }
}

/// The api, its stage and the roles every row below uses.
///
/// Two methods: `/orders/{id}`, whose integration may write to the sink, and `/boom/{id}`,
/// whose integration names a role API Gateway cannot assume — an integration the stage cannot
/// perform, which it answers `500` through its `API_CONFIGURATION_ERROR` gateway response.
async fn deployed(router: &MultiServiceRouter) -> Deployed {
    let (sink_url, sink_arn) = queue(router, "sink", "rest-stage-target-sink").await;
    let (dead_letter_url, dead_letter_arn) =
        queue(router, "dead letter", "rest-stage-target-dlq").await;
    let account = sink_arn
        .split(':')
        .nth(4)
        .unwrap_or_default()
        .to_string();
    // The dead-letter queue's own policy admits the write EventBridge makes as its own
    // service principal ("for … Amazon SQS queues, EventBridge relies on resource-based
    // policies"), so what reaches it is decided by the target's delivery and not by this
    // queue.
    sqs(
        router,
        "dead letter",
        "SetQueueAttributes",
        &json!({
            "QueueUrl": dead_letter_url,
            "Attributes": {
                "Policy": json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": dead_letter_arn,
                    }],
                })
                .to_string(),
            },
        }),
    )
    .await;

    let sender = role(
        router,
        "rest-stage-sink-sender",
        APIGATEWAY_TRUST,
        &policy("sqs:SendMessage", &sink_arn),
    )
    .await;
    // A role the api cannot assume at all, which is what makes the `/boom` integration one
    // the stage cannot perform.
    let unassumable = role(
        router,
        "rest-stage-unassumable",
        EVENTS_TRUST,
        &policy("sqs:SendMessage", &sink_arn),
    )
    .await;

    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "rest-stage-target" })),
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
    let sink_uri = format!(
        "arn:aws:apigateway:{REGION}:sqs:path/{account}/{}",
        sink_url.rsplit('/').next().unwrap_or_default()
    );
    // The method the rule delivers to is authorized by IAM, which is what makes the request
    // EventBridge signs the only one it serves.
    sink_method(router, &api, &root, "orders", "AWS_IAM", &sink_uri, &sender).await;
    sink_method(router, &api, &root, "boom", "NONE", &sink_uri, &unassumable).await;
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    let deployed = Deployed {
        api,
        account,
        sink_url,
        dead_letter_url,
        dead_letter_arn,
        invoker: String::new(),
        stranger: String::new(),
    };
    let invoker = role(
        router,
        "rest-stage-invoker",
        EVENTS_TRUST,
        &policy(
            "execute-api:Invoke",
            &format!(
                "arn:aws:execute-api:{REGION}:{}:{}/{STAGE}/POST/*",
                deployed.account, deployed.api
            ),
        ),
    )
    .await;
    // The same grant on ANOTHER api's methods: a role EventBridge can assume that allows no
    // invoke of this one.
    let stranger = role(
        router,
        "rest-stage-stranger",
        EVENTS_TRUST,
        &policy(
            "execute-api:Invoke",
            &format!(
                "arn:aws:execute-api:{REGION}:{}:zzz9999999/{STAGE}/POST/*",
                deployed.account
            ),
        ),
    )
    .await;
    Deployed {
        invoker,
        stranger,
        ..deployed
    }
}

/// Wire one rule of the default bus to one target of the stage.
///
/// The rule matches `source` alone of the sources this suite publishes, so a row's own
/// publication reaches that row's target and no other row's.
async fn wired(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    source: &str,
    target: Value,
) {
    events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventPattern": json!({
                "source": [source],
                "detail-type": [DETAIL_TYPE],
            })
            .to_string(),
        }),
    )
    .await;
    let answer = events(
        router,
        case,
        "PutTargets",
        &json!({ "Rule": name, "Targets": [target] }),
    )
    .await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "setup {case:?}: expected the target to be accepted, got {answer}"
    );
}

/// Publish one entry on the default bus, which has to be accepted.
async fn published(
    router: &MultiServiceRouter,
    case: &str,
    source: &str,
    detail_type: &str,
    detail: &Value,
) {
    let answer = events(
        router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": source,
                "DetailType": detail_type,
                "Detail": detail.to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the entry to be accepted, got {answer}"
    );
}

/// One row of the delivery table: which event is published, and what the stage then received —
/// `None` for an event the rule does not match, which reaches no stage at all.
struct Row {
    case: &'static str,
    detail_type: &'static str,
    detail: Value,
    /// The `id` the path of the request carries and the `x-order-src` header it carries, as
    /// the target's parameters name them out of the event — `None` where nothing is
    /// delivered.
    expected: Option<(&'static str, &'static str)>,
}

#[tokio::test]
async fn a_rule_targeting_a_rest_api_stage_delivers_the_matched_event_to_it() {
    let router = emulator();
    let stage = deployed(&router).await;
    wired(
        &router,
        "the delivering rule",
        "rest-stage-delivers",
        SOURCE,
        json!({
            "Id": "stage",
            "Arn": stage.target_arn("orders/*"),
            "RoleArn": stage.invoker,
            "HttpParameters": {
                // Each value names a place in the EVENT, which is what makes one rule
                // address a different resource and carry a different header per event.
                "PathParameterValues": ["$.detail.id"],
                "HeaderParameters": { SOURCE_HEADER: "$.detail.src" },
            },
            "InputTransformer": {
                "InputPathsMap": { "d": "$.detail", "t": "$.detail-type" },
                "InputTemplate": r#"{"wrapped": <d>, "kind": <t>}"#,
            },
            "DeadLetterConfig": { "Arn": stage.dead_letter_arn },
        }),
    )
    .await;

    let rows: &[Row] = &[
        Row {
            case: "a matching event, whose detail names the path and the header",
            detail_type: DETAIL_TYPE,
            detail: json!({ "id": "ord-abc123", "src": "checkout-web", "amount": 42 }),
            expected: Some(("ord-abc123", "checkout-web")),
        },
        Row {
            case: "another matching event, whose detail names another path and header",
            detail_type: DETAIL_TYPE,
            detail: json!({ "id": "w-1", "src": "runner", "lines": [{ "sku": "A-1" }] }),
            expected: Some(("w-1", "runner")),
        },
        Row {
            case: "a detail-type differing only in letter case, which the rule does not \
                   match",
            detail_type: "orderPlaced",
            detail: json!({ "id": "ord-near-miss", "src": "checkout-web" }),
            expected: None,
        },
    ];

    for row in rows {
        published(&router, row.case, SOURCE, row.detail_type, &row.detail).await;
        let wanted = usize::from(row.expected.is_some());
        let held = messages(&router, row.case, &stage.sink_url, wanted).await;
        let Some((id, src)) = row.expected else {
            assert!(
                held.is_empty(),
                "case {:?}: expected no request at the stage at all, got {held:?}",
                row.case
            );
            let dead = messages(&router, row.case, &stage.dead_letter_url, 0).await;
            assert!(
                dead.is_empty(),
                "case {:?}: an event the rule does not match is not a failed delivery, got \
                 {dead:?}",
                row.case
            );
            continue;
        };
        assert_eq!(
            held.len(),
            1,
            "case {:?}: expected exactly the one request this event made, got {held:?}",
            row.case
        );
        let body: Value = held[0]["Body"]
            .as_str()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the document the stage's template wrote, got {}",
                    row.case, held[0]
                )
            });
        // What the target asked to be delivered: the transformer's own rendering, carrying
        // exactly the two members its template names.
        assert_eq!(
            body["delivered"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok()),
            Some(json!({ "wrapped": row.detail, "kind": row.detail_type })),
            "case {:?}: expected the transformed event as the request's body, got {body}",
            row.case
        );
        // The path parameter the delivery filled, and the header it contributed, each read
        // out of the event.
        assert_eq!(
            body["pathId"].as_str(),
            Some(id),
            "case {:?}: expected the path the event names, got {body}",
            row.case
        );
        assert_eq!(
            body["orderSrc"].as_str(),
            Some(src),
            "case {:?}: expected the header the event names, got {body}",
            row.case
        );
        // The resource the request reached, which is the method the target's ARN named.
        assert_eq!(
            body["route"].as_str(),
            Some("/orders/{id}"),
            "case {:?}: expected the stage's own resource path, got {body}",
            row.case
        );
        assert!(
            messages(&router, row.case, &stage.dead_letter_url, 0)
                .await
                .is_empty(),
            "case {:?}: a delivery the stage served dead-letters nothing",
            row.case
        );
        sqs(
            &router,
            row.case,
            "DeleteMessage",
            &json!({
                "QueueUrl": stage.sink_url,
                "ReceiptHandle": held[0]["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        )
        .await;
    }
}

/// One row of the failure table: which target the rule delivers to, and what its dead letter
/// then carries.
struct Failure {
    case: &'static str,
    /// The `source` this row publishes under, which its own rule is the only one to match.
    source: &'static str,
    /// The resource path of the target's ARN, which decides what the stage answers.
    path: &'static str,
    /// Whether the target's role allows `execute-api:Invoke` on that ARN.
    granted: bool,
    /// The condition the dead letter names.
    code: &'static str,
    /// What the dead letter's `ERROR_MESSAGE` has to carry.
    message: &'static str,
    /// Whether that message has to name the invoke URL the delivery addressed, which is the
    /// address this emulator serves the stage at.
    names_the_url: bool,
    /// How many retries the dead letter reports, `None` where the delivery retried nothing.
    retries: Option<i64>,
}

#[tokio::test]
async fn a_delivery_to_a_stage_that_fails_is_dead_lettered_with_what_it_came_to() {
    let router = emulator();
    let stage = deployed(&router).await;
    let failures: &[Failure] = &[
        Failure {
            case: "a role whose grant allows no invoke of this api",
            source: "sim2real.orders.ungranted",
            path: "orders/*",
            granted: false,
            code: NO_PERMISSIONS,
            message: "execute-api:Invoke",
            names_the_url: false,
            retries: None,
        },
        Failure {
            // The stage's integration names a role it cannot assume, so it answers its
            // `API_CONFIGURATION_ERROR` gateway response: a 5xx the delivery retries.
            case: "a stage answering 5xx, which is retried and then dead-lettered",
            source: "sim2real.orders.failing",
            path: "boom/*",
            granted: true,
            code: CONNECTION_FAILURE,
            message: "Got status code \"500\" while trying to invoke \"POST ",
            names_the_url: true,
            retries: Some(RETRIES_ALLOWED),
        },
        Failure {
            // A method of this api the deployment holds none of, which the stage answers
            // through the gateway response for a request matching no method: a 4xx is the
            // endpoint REFUSING the event, so it is dead-lettered on the first attempt
            // without being tried again.
            case: "a stage answering 4xx, which is not retried",
            source: "sim2real.orders.unrouted",
            path: "nowhere/*",
            granted: true,
            code: CONNECTION_FAILURE,
            message: "Got status code \"403\" while trying to invoke \"POST ",
            names_the_url: true,
            retries: None,
        },
    ];

    for (index, failure) in failures.iter().enumerate() {
        let role_arn = match failure.granted {
            true => &stage.invoker,
            false => &stage.stranger,
        };
        wired(
            &router,
            failure.case,
            &format!("rest-stage-failure-{index}"),
            failure.source,
            json!({
                "Id": "stage",
                "Arn": stage.target_arn(failure.path),
                "RoleArn": role_arn,
                "HttpParameters": { "PathParameterValues": ["$.detail.id"] },
                "DeadLetterConfig": { "Arn": stage.dead_letter_arn },
                "RetryPolicy": { "MaximumRetryAttempts": RETRIES_ALLOWED },
            }),
        )
        .await;
        published(
            &router,
            failure.case,
            failure.source,
            DETAIL_TYPE,
            &json!({ "id": format!("ord-{index}"), "src": "runner" }),
        )
        .await;
        let dead = messages(&router, failure.case, &stage.dead_letter_url, 1).await;
        assert_eq!(
            dead.len(),
            1,
            "case {:?}: expected the one dead letter this delivery produced, got {dead:?}",
            failure.case
        );
        let code = attribute(&dead[0], "ERROR_CODE");
        assert_eq!(
            code.as_deref(),
            Some(failure.code),
            "case {:?}: expected the {:?} condition, got {}",
            failure.case,
            failure.code,
            dead[0]
        );
        let message = attribute(&dead[0], "ERROR_MESSAGE").unwrap_or_default();
        assert!(
            message.contains(failure.message),
            "case {:?}: expected a message carrying {:?}, got {message:?}",
            failure.case,
            failure.message
        );
        if failure.names_the_url {
            // The URL the delivery addressed: the address this emulator serves the stage at,
            // which is its endpoint and the path the data plane reads the api id out of.
            let addressed = format!(
                "/_aws/execute-api/{}/{STAGE}/{}/ord-{index}",
                stage.api,
                failure.path.trim_end_matches("/*")
            );
            assert!(
                message.contains(&addressed),
                "case {:?}: expected the message to name {addressed:?}, got {message:?}",
                failure.case
            );
        }
        assert_eq!(
            attribute(&dead[0], "TARGET_ARN").as_deref(),
            Some(stage.target_arn(failure.path).as_str()),
            "case {:?}: expected the target's own ARN on the dead letter, got {}",
            failure.case,
            dead[0]
        );
        let reported = attribute(&dead[0], RETRY_ATTEMPTS)
            .and_then(|value| value.parse::<i64>().ok());
        assert_eq!(
            reported, failure.retries,
            "case {:?}: expected {:?} retries reported, got {reported:?}",
            failure.case, failure.retries
        );
        // Nothing reached the sink: the delivery that failed made no request the stage
        // served.
        let held = messages(&router, failure.case, &stage.sink_url, 0).await;
        assert!(
            held.is_empty(),
            "case {:?}: expected no message behind the stage, got {held:?}",
            failure.case
        );
        sqs(
            &router,
            failure.case,
            "DeleteMessage",
            &json!({
                "QueueUrl": stage.dead_letter_url,
                "ReceiptHandle": dead[0]["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        )
        .await;
    }
}
