// SPDX-License-Identifier: CC0-1.0

//! An HTTP api route whose integration is a FIRST-CLASS SERVICE ACTION — `AWS_PROXY` naming
//! an `IntegrationSubtype` — performed end to end: this service, EventBridge, Amazon SQS,
//! IAM and the token service on ONE engine, and the action's effect read back where a client
//! reads it.
//!
//! The RULE every table below is one slice of: **an HTTP api route whose integration names
//! an `IntegrationSubtype` performs that AWS service action on every matching request, with
//! each of the action's parameters taken from the integration's `RequestParameters` — one
//! mapping expression over the request, the invocation's context or the stage's variables,
//! or a static value — issued as a session of the role its `CredentialsArn` names, so the
//! service serving the action decides on that role's own policies whether it happens; and
//! the caller is answered the action's own answer, or the gateway's `500` when no action was
//! performed.**
//!
//! Nothing asserted here is a value this emulator fills in: every assertion is on something
//! ANOTHER service holds — an event on the queue a rule of the named bus targets, a message
//! on the queue a send named — which is why those services are mounted beside this one.
//!
//! The steps BEFORE the integration (the claim, stage resolution, the deployment gate, route
//! matching, the authorization gate, the CORS decoration and the payload quota) are owned by
//! `http_api_servable_stage.rs` and its siblings and are not restated here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, Reader};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_form::form_encode;

/// The Region every request below is signed for, unless a suite reaching this estate
/// through another endpoint names one of its own ([`Emulator::at_endpoint`]).
const REGION: &str = "us-east-1";

/// The access-key id standing for the account's own published credential: neither the token
/// service nor IAM holds a record of it, so every request this suite makes on its own behalf
/// speaks for the account's ambient identity.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The role an integration below names, and the inline policy this suite writes on it.
pub(crate) const ROLE_NAME: &str = "apigateway-publishes-events";
const INLINE_POLICY: &str = "inline";

/// The actions a statement on that role names.
pub(crate) const PUT_EVENTS_ACTION: &str = "events:PutEvents";
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The custom bus an integration publishes onto, and the rule that carries this suite's
/// control queue on it.
pub(crate) const BUS: &str = "orders-bus";
const RULE: &str = "matching";
const NEAR_MISS_RULE: &str = "near-miss";
const TARGET_ID: &str = "t1";

/// The service principal EventBridge delivers to a queue as, which the queue's own policy
/// has to grant: "for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge
/// relies on resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The status and sentence an HTTP api answers for an integration that produced no response.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// The status and sentence it answers for a request that cannot satisfy what its route's
/// integration maps onto a parameter the action requires.
const BAD_REQUEST: u16 = 400;
const BAD_REQUEST_MESSAGE: &str = "Bad Request";

/// The media types a first-class subtype route answers a performed action's document under:
/// the one a QUERY interface states an answer in, and the one an AWS JSON protocol does.
const XML_MEDIA_TYPE: &str = "text/xml";
const JSON_MEDIA_TYPE: &str = "application/json";

/// The status an HTTP api answers for a first-class service action the service serving it
/// REFUSED: that service's own answer, passed through — EventBridge's `AccessDeniedException`
/// is its modelled 400 (`smithy_query(query="shape", shape="AccessDeniedException")` on the
/// EventBridge model).
pub(crate) const ACTION_REFUSED: u16 = 400;

/// The error that service's own document names for a publication the policies in force do
/// not permit.
pub(crate) const ACCESS_DENIED: &str = "AccessDeniedException";

/// The session name API Gateway opens an integration role's session under, which is what the
/// relayed refusal names as the principal.
pub(crate) const SESSION_NAME: &str = "BackplaneAssumeRoleSession";

/// This service, EventBridge, Amazon SQS, IAM and the token service over ONE shared engine,
/// as `core/router/src/lib.rs` mounts them: the action an integration performs is dispatched
/// back into this same router, so the bus it publishes onto is the bus this suite put a rule
/// on.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
    /// The Region every call below is signed for, and the Region in the hosts of the other
    /// services this estate holds.
    region: String,
    /// The endpoint the CONTROL-PLANE client is configured with: the `Host` its calls carry.
    /// A value this service ANNOUNCES follows the endpoint the request came in on, so which
    /// address an api is published at is a function of this.
    control_endpoint: String,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        Self::at_endpoint(&format!("apigateway.{REGION}.amazonaws.com"), REGION)
    }

    /// The same estate reached by a client configured with `control_endpoint` and signing for
    /// `region` — the emulator's own loopback endpoint for a client that resolves nothing
    /// else, or one of AWS's own service hosts for a client that does ([`new`](Self::new)).
    pub(crate) fn at_endpoint(control_endpoint: &str, region: &str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the Region and the caller's access key id.
        // The INTEGRATION's own call is signed by this emulator, with the session it minted.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                engine,
            ),
        );
        Self {
            router,
            region: region.to_string(),
            control_endpoint: control_endpoint.to_string(),
        }
    }

    /// A SigV4 `Authorization` whose credential scope names `service`, which is what routes
    /// the request. The signature is never checked here.
    fn authorization(&self, service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{}/{service}/\
             aws4_request, SignedHeaders=host, Signature=not-verified",
            self.region
        )
    }

    /// One `awsJson` RPC call against the service whose signing name is `signing_name`, which
    /// must be served.
    async fn rpc(
        &self,
        case: &str,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header(
                "host",
                format!("{signing_name}.{}.amazonaws.com", self.region),
            )
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", self.authorization(signing_name))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = response.into_body();
        let parsed: Value = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&payload)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).to_string()))
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// One EventBridge call that must be served (`awsJson1_1`, `AWSEvents.<Operation>`).
    pub(crate) async fn events(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "events", "AWSEvents", "1.1", operation, body)
            .await
    }

    /// One Amazon SQS call that must be served (`awsJson1_0`, `AmazonSQS.<Operation>`).
    async fn sqs(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "sqs", "AmazonSQS", "1.0", operation, body)
            .await
    }

    /// One awsQuery call to IAM, as the account's own credential, which must be served.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", self.authorization("iam"))
            .body(bytes::Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: IAM {action} must succeed, got {status} {answered}"
        );
    }

    /// One control-plane call against this service (`restJson1` under `/v2`), as its status
    /// and parsed body.
    pub(crate) async fn control(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(value.to_string()),
            None => bytes::Bytes::new(),
        };
        let host = self.control_endpoint.clone();
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header("authorization", self.authorization("apigateway"))
            .body(payload)
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = response.into_body();
        let parsed: Value = if answered.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&answered)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answered).to_string()))
        };
        (status, parsed)
    }

    /// An api with one integration, one route over it and the `$default` stage with
    /// auto-deploy on; answers `(ApiId, the address its ApiEndpoint announces)` — the value a
    /// client builds its invoke URL from, which [`invoke`](Self::invoke) takes as it stands.
    pub(crate) async fn api(
        &self,
        case: &str,
        integration: Value,
        route_key: &str,
    ) -> (String, String) {
        let (status, api) = self
            .control(
                "POST",
                "/v2/apis",
                Some(json!({ "name": "subtype-api", "protocolType": "HTTP" })),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateApi answered {status} {api}"
        );
        let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
        let announced = api["apiEndpoint"].as_str().unwrap_or_default().to_string();
        let (status, created) = self
            .control(
                "POST",
                &format!("/v2/apis/{api_id}/integrations"),
                Some(integration),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateIntegration answered {status} {created}"
        );
        let integration_id = created["integrationId"].as_str().unwrap_or_default();
        let (status, route) = self
            .control(
                "POST",
                &format!("/v2/apis/{api_id}/routes"),
                Some(json!({
                    "routeKey": route_key,
                    "target": format!("integrations/{integration_id}"),
                })),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateRoute answered {status} {route}"
        );
        let (status, stage) = self
            .control(
                "POST",
                &format!("/v2/apis/{api_id}/stages"),
                Some(json!({
                    "stageName": "$default",
                    "autoDeploy": true,
                    "stageVariables": { "source": "app.stage-variable" },
                })),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateStage answered {status} {stage}"
        );
        (api_id, announced)
    }

    /// One client request to a route of the deployed stage, carrying `body` — the invoke the
    /// integration is performed for. `endpoint` is the address the api ANNOUNCED and `path`
    /// what the client appends to it. No credential: the route's `AuthorizationType` is the
    /// default `NONE`.
    pub(crate) async fn invoke(
        &self,
        endpoint: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (u16, Value) {
        let (status, _media_type, parsed) = self.answered(endpoint, path, headers, body).await;
        (status, parsed)
    }

    /// The same invoke, with the MEDIA TYPE the route answered beside its status and body —
    /// the half of the answer that says which wire form the document is stated in.
    async fn answered(
        &self,
        endpoint: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (u16, String, Value) {
        let (authority, base_path) =
            crate::http_api_published_invoke_address::announced_parts(endpoint);
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!("https://{authority}{base_path}{path}"))
            .header("host", authority.as_str())
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let media_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let answered = response.into_body();
        let parsed: Value = if answered.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&answered)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answered).to_string()))
        };
        (status, media_type, parsed)
    }

    /// The role an integration's `CredentialsArn` names, trusted by API Gateway and carrying
    /// `statements` as its inline identity policy — written with IAM's own operations, so the
    /// role a session is minted of is the record `iam:CreateRole` wrote. Answers its ARN.
    pub(crate) async fn role(&self, case: &str, statements: Option<Value>) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Service": "apigateway.amazonaws.com"},
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        self.iam(
            case,
            "CreateRole",
            &[
                ("RoleName", ROLE_NAME.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        if let Some(statements) = statements {
            let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
            self.iam(
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", ROLE_NAME.to_string()),
                    ("PolicyName", INLINE_POLICY.to_string()),
                    ("PolicyDocument", document),
                ],
            )
            .await;
        }
        role_arn()
    }

    /// A queue carrying the policy EventBridge's delivery needs; answers `(url, arn)`.
    async fn queue(&self, case: &str, name: &str) -> (String, String) {
        let url = self.queue_with(case, name, json!({})).await;
        let attributes = self
            .sqs(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
            .to_string();
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "AllowEventBridge",
                "Effect": "Allow",
                "Principal": {"Service": EVENTS_PRINCIPAL},
                "Action": SEND_MESSAGE_ACTION,
                "Resource": arn,
            }],
        })
        .to_string();
        self.sqs(
            case,
            "SetQueueAttributes",
            &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
        )
        .await;
        (url, arn)
    }

    /// The custom bus, a rule on it matching exactly `(source, detail_type)` with its own
    /// queue, and a NEAR-MISS rule differing from it only in the letter case of the detail
    /// type, with a queue of its own. Answers `(matching queue url, near-miss queue url)`.
    pub(crate) async fn bus_with_rules(
        &self,
        case: &str,
        source: &str,
        detail_type: &str,
    ) -> (String, String) {
        self.events(case, "CreateEventBus", &json!({ "Name": BUS }))
            .await;
        let (matching_url, matching_arn) = self.queue(case, "matching-queue").await;
        let (near_miss_url, near_miss_arn) = self.queue(case, "near-miss-queue").await;
        let rules: [(&str, String, &str); 2] = [
            (
                RULE,
                json!({ "source": [source], "detail-type": [detail_type] }).to_string(),
                matching_arn.as_str(),
            ),
            (
                // EventBridge string matching is exact and case-sensitive, so this rule —
                // which differs only in the letter case of the detail type — must receive
                // nothing.
                NEAR_MISS_RULE,
                json!({ "source": [source], "detail-type": [detail_type.to_lowercase()] })
                    .to_string(),
                near_miss_arn.as_str(),
            ),
        ];
        for (name, pattern, target) in rules {
            self.events(
                case,
                "PutRule",
                &json!({
                    "Name": name,
                    "EventBusName": BUS,
                    "EventPattern": pattern,
                    "State": "ENABLED",
                }),
            )
            .await;
            let targets = self
                .events(
                    case,
                    "PutTargets",
                    &json!({
                        "Rule": name,
                        "EventBusName": BUS,
                        "Targets": [{ "Id": TARGET_ID, "Arn": target }],
                    }),
                )
                .await;
            assert_eq!(
                targets["FailedEntryCount"],
                json!(0),
                "case {case:?}: PutTargets reported a failed entry: {targets}"
            );
        }
        (matching_url, near_miss_url)
    }

    /// A queue named `name` created with `attributes`: the ONE create every queue in this
    /// suite is made with, so the kinds it needs differ in that map alone. Answers its url.
    async fn queue_with(&self, case: &str, name: &str, attributes: Value) -> String {
        let created = self
            .sqs(
                case,
                "CreateQueue",
                &json!({ "QueueName": name, "Attributes": attributes }),
            )
            .await;
        created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
            .to_string()
    }

    /// A FIFO queue, which is the one kind whose send is answered a `SequenceNumber`
    /// ("This parameter applies only to FIFO (first-in-first-out) queues",
    /// `SendMessageResult$SequenceNumber`); answers its url.
    async fn fifo_queue(&self, case: &str, name: &str) -> String {
        self.queue_with(case, name, json!({ "FifoQueue": "true" }))
            .await
    }

    /// Every message waiting on the queue at `url`, as its parsed body, taken off it.
    pub(crate) async fn drained(&self, case: &str, url: &str) -> Vec<Value> {
        self.received(case, url)
            .await
            .iter()
            .map(|message| {
                let text = message["Body"].as_str().unwrap_or_default();
                serde_json::from_str(text).unwrap_or(Value::String(text.to_string()))
            })
            .collect()
    }

    /// Every message waiting on the queue at `url`, WHOLE — the record a consumer reads, so
    /// the id the queue holds for a message can be compared with the id its sender was
    /// answered — taken off it.
    async fn received(&self, case: &str, url: &str) -> Vec<Value> {
        let received = self
            .sqs(
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        received["Messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

/// The ARN of the role an integration below names, as IAM holds it.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/{ROLE_NAME}",
        engine::emulator_account_id()
    )
}

/// The integration an api below carries: the first-class subtype `subtype`, performed under
/// `credentials_arn`, with `request_parameters` mapping the action's parameters.
pub(crate) fn subtype_integration(
    subtype: &str,
    credentials_arn: Option<&str>,
    request_parameters: Value,
) -> Value {
    let mut integration = json!({
        "integrationType": "AWS_PROXY",
        "integrationSubtype": subtype,
        "payloadFormatVersion": "1.0",
        "requestParameters": request_parameters,
    });
    if let Some(credentials_arn) = credentials_arn {
        integration["credentialsArn"] = json!(credentials_arn);
    }
    integration
}

/// A statement granting `action` on every resource, which is what an integration's role
/// needs to perform the action the subtype names.
pub(crate) fn grant(action: &str) -> Value {
    json!([{ "Effect": "Allow", "Action": action, "Resource": "*" }])
}

/// The text of every `element` of the document the route answered, in document order.
///
/// Parsed with `quick-xml` (already a dependency of this workspace) rather than by slicing
/// the body, so the answer of a service whose api is a QUERY api is read exactly the way a
/// client's own XML reader reads it — which is what answering that service's own document is
/// for. A body that is not well-formed XML fails the case rather than reading as nothing.
fn envelope_elements(answered: &Value, element: &str) -> Vec<String> {
    let body = answered.as_str().unwrap_or_default().to_string();
    let mut reader = Reader::from_str(&body);
    let mut inside = false;
    let mut found: Vec<String> = Vec::new();
    loop {
        match reader.read_event().unwrap_or_else(|error| {
            panic!("the route's answer must be well-formed XML: {error} in {body:?}")
        }) {
            Event::Start(start) => inside = start.name().as_ref() == element.as_bytes(),
            Event::Text(text) if inside => {
                let decoded = text
                    .xml10_content()
                    .unwrap_or_else(|error| panic!("the answer must decode: {error}"));
                found.push(decoded.into_owned());
            }
            Event::End(_) => inside = false,
            Event::Eof => break,
            _ => {}
        }
    }
    found
}

/// The one `MessageId` the answer of a send carries, or `None` for an answer carrying none.
fn answered_message_id(answered: &Value) -> Option<String> {
    envelope_elements(answered, "MessageId").into_iter().next()
}

/// RULE: every parameter of the action comes from the integration's `RequestParameters` — a
/// mapping expression over the request, the invocation's context or the stage's variables,
/// or a static value — so the event that lands on the named bus carries exactly what the
/// mapping says, and a rule differing from it only in letter case receives nothing.
#[tokio::test]
async fn a_subtype_route_publishes_the_event_its_request_parameters_map() {
    // Rows differ along WHICH mapping expression supplies each parameter: static values, the
    // request's body whole and by member, a header, a query-string parameter, a path
    // variable, a stage variable, and a context variable.
    struct Row {
        case: &'static str,
        route_key: &'static str,
        path: &'static str,
        headers: &'static [(&'static str, &'static str)],
        body: &'static str,
        parameters: fn() -> Value,
        source: &'static str,
        detail_type: &'static str,
        detail: fn() -> Value,
    }
    let rows: &[Row] = &[
        Row {
            case: "static values with the body as the detail",
            route_key: "POST /orders",
            path: "/orders",
            headers: &[],
            body: r#"{"id":"o-1"}"#,
            parameters: || {
                json!({
                    "Source": "app.orders",
                    "DetailType": "OrderPlaced",
                    "Detail": "$request.body",
                    "EventBusName": BUS,
                })
            },
            source: "app.orders",
            detail_type: "OrderPlaced",
            detail: || json!({ "id": "o-1" }),
        },
        Row {
            case: "a header, a body member, and a nested document as the detail",
            route_key: "POST /invoices",
            path: "/invoices",
            headers: &[("x-source", "app.invoices")],
            body: r#"{"kind":"InvoiceRaised","payload":{"total":12}}"#,
            parameters: || {
                json!({
                    "Source": "$request.header.x-source",
                    "DetailType": "$request.body.kind",
                    "Detail": "$request.body.payload",
                    "EventBusName": BUS,
                })
            },
            source: "app.invoices",
            detail_type: "InvoiceRaised",
            detail: || json!({ "total": 12 }),
        },
        Row {
            case: "a query-string parameter and the method the context states",
            route_key: "POST /signals",
            path: "/signals?source=app.signals",
            headers: &[],
            body: r#"{"seen":true}"#,
            parameters: || {
                json!({
                    "Source": "$request.querystring.source",
                    "DetailType": "$context.httpMethod",
                    "Detail": "$request.body",
                    "EventBusName": BUS,
                })
            },
            source: "app.signals",
            detail_type: "POST",
            detail: || json!({ "seen": true }),
        },
        Row {
            case: "a stage variable, a path variable, and a static detail",
            route_key: "POST /shipments/{kind}",
            path: "/shipments/Shipped",
            headers: &[],
            body: "",
            parameters: || {
                json!({
                    "Source": "$stageVariables.source",
                    "DetailType": "$request.path.kind",
                    "Detail": r#"{"static":true}"#,
                    "EventBusName": BUS,
                })
            },
            source: "app.stage-variable",
            detail_type: "Shipped",
            detail: || json!({ "static": true }),
        },
    ];
    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        let role = emulator.role(case, Some(grant(PUT_EVENTS_ACTION))).await;
        let (matching, near_miss) = emulator.bus_with_rules(case, row.source, row.detail_type).await;
        let endpoint = emulator
            .api(
                case,
                subtype_integration("EventBridge-PutEvents", Some(&role), (row.parameters)()),
                row.route_key,
            )
            .await
            .1;
        let (status, media_type, answered) = emulator
            .answered(&endpoint, row.path, row.headers, row.body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the action's own answer, got {status} {answered}"
        );
        // The answer is EventBridge's own: one ingested entry, with the id that service
        // minted for it, as the JSON document that service's own api states one in.
        assert_eq!(
            media_type, JSON_MEDIA_TYPE,
            "case {case:?}: expected {JSON_MEDIA_TYPE:?}, got {media_type:?} for {answered}"
        );
        assert_eq!(
            answered["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected no failed entry in {answered}"
        );
        assert!(
            answered["Entries"][0]["EventId"].is_string(),
            "case {case:?}: expected EventBridge's own event id in {answered}"
        );
        let delivered = emulator.drained(case, &matching).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: expected one event on the matching rule's queue, got {delivered:?}"
        );
        let event = &delivered[0];
        assert_eq!(
            event["source"].as_str().unwrap_or_default(),
            row.source,
            "case {case:?}: expected the mapped source, got {event}"
        );
        assert_eq!(
            event["detail-type"].as_str().unwrap_or_default(),
            row.detail_type,
            "case {case:?}: expected the mapped detail type, got {event}"
        );
        assert_eq!(
            event["detail"],
            (row.detail)(),
            "case {case:?}: expected the mapped detail, got {event}"
        );
        let missed = emulator.drained(case, &near_miss).await;
        assert!(
            missed.is_empty(),
            "case {case:?}: expected nothing on the case-differing rule's queue, got {missed:?}"
        );
    }
}

/// RULE: the action is issued as a session of the role the integration's `CredentialsArn`
/// names, so the service serving it decides on that role's own policies whether it happens —
/// and a request whose action that service refused is answered THAT service's own refusal,
/// passed through, rather than a success for a publication nobody made.
#[tokio::test]
async fn a_subtype_route_performs_the_action_as_the_role_its_credentials_arn_names() {
    // Rows differ along what the integration's role may do when the route is invoked: a
    // grant of the action, a grant of another action only, no policy at all, an explicit
    // deny, and an integration naming no role — which is the one case the call is made as the
    // in-process caller.
    let rows: &[(&str, Option<Value>, bool, bool)] = &[
        (
            "a role whose policy grants the action",
            Some(grant(PUT_EVENTS_ACTION)),
            true,
            true,
        ),
        (
            "a role whose policy grants another action only",
            Some(grant(SEND_MESSAGE_ACTION)),
            true,
            false,
        ),
        ("a role carrying no policy at all", None, true, false),
        (
            "a role whose policy explicitly denies the action",
            Some(json!([
                { "Effect": "Allow", "Action": "*", "Resource": "*" },
                { "Effect": "Deny", "Action": PUT_EVENTS_ACTION, "Resource": "*" },
            ])),
            true,
            false,
        ),
        ("an integration naming no role", None, false, true),
    ];
    for (case, statements, names_role, performed) in rows {
        let emulator = Emulator::new();
        let role = emulator.role(case, statements.clone()).await;
        let (matching, _near_miss) = emulator
            .bus_with_rules(case, "app.orders", "OrderPlaced")
            .await;
        let (_api_id, endpoint) = emulator
            .api(
                case,
                subtype_integration(
                    "EventBridge-PutEvents",
                    names_role.then_some(role.as_str()),
                    json!({
                        "Source": "app.orders",
                        "DetailType": "OrderPlaced",
                        "Detail": "$request.body",
                        "EventBusName": BUS,
                    }),
                ),
                "POST /orders",
            )
            .await;
        let (status, answered) = emulator
            .invoke(&endpoint, "/orders", &[], r#"{"id":"o-1"}"#)
            .await;
        let expected_status = if *performed { 200 } else { ACTION_REFUSED };
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status} from the route, got {status} {answered}"
        );
        if !*performed {
            // EventBridge's OWN refusal, relayed: the error shape it named, and the text it
            // stated — which names the action it refused and the principal it refused it
            // for, the assumed-role session of the integration's role, so the call really
            // was made as that role.
            let refusal = answered["__type"].as_str().unwrap_or_default();
            assert!(
                refusal.ends_with(ACCESS_DENIED),
                "case {case:?}: expected the service's own {ACCESS_DENIED}, got {answered}"
            );
            let detail = answered["Message"].as_str().unwrap_or_default();
            let session = format!("assumed-role/{ROLE_NAME}/{SESSION_NAME}");
            assert!(
                detail.contains(&session) && detail.contains(PUT_EVENTS_ACTION),
                "case {case:?}: expected a refusal naming {session:?} and \
                 {PUT_EVENTS_ACTION:?}, got {answered}"
            );
        }
        let reached = emulator.drained(case, &matching).await;
        assert_eq!(
            reached.len(),
            usize::from(*performed),
            "case {case:?}: expected performed={performed}, got {reached:?}"
        );
    }
}

/// RULE: the action performed is the one the `IntegrationSubtype` names, whichever service
/// owns it — and a subtype this emulator cannot perform, or a mapping the action cannot be
/// called with, is answered as an integration that produced no response rather than as a call
/// nothing made.
#[tokio::test]
async fn the_action_a_subtype_route_performs_is_the_one_its_subtype_names() {
    // Rows differ along the SUBTYPE and the mapping: the send another service owns, one
    // mapping a parameter this emulator does not carry into the action, and a subtype it
    // performs no action for. A mapping that defines NO value at all for a parameter the
    // action requires is not among them: such an integration is refused at the write, where
    // `tests/all/integration_validation.rs` asserts it, so no route can carry one.
    //
    // The mapping is a function of the queue's url, which only exists once the queue does.
    type Parameters = fn(&str) -> Value;
    let rows: &[(&str, &str, Parameters, Option<&str>)] = &[
        (
            "the send Amazon SQS owns",
            "SQS-SendMessage",
            |url| json!({ "QueueUrl": url, "MessageBody": "$request.body.note" }),
            Some("placed"),
        ),
        (
            "a send mapping a parameter this emulator does not carry",
            "SQS-SendMessage",
            |url| {
                json!({
                    "QueueUrl": url,
                    "MessageBody": "$request.body.note",
                    "MessageAttributes": r#"{"a":{"DataType":"String","StringValue":"b"}}"#,
                })
            },
            None,
        ),
        (
            // A subtype this emulator catalogues NO action for: `IntegrationSubtype` is an
            // opaque string the control plane admits whatever it names, so the route exists,
            // the mapping is whatever that action would take, and only the action is missing.
            "a subtype this emulator performs no action for",
            "S3-PutObject",
            |_url| {
                json!({
                    "Bucket": "orders",
                    "Key": "placed",
                    "Body": "$request.body.note",
                })
            },
            None,
        ),
    ];
    for (case, subtype, parameters, expected) in rows {
        let emulator = Emulator::new();
        let role = emulator.role(case, Some(grant(SEND_MESSAGE_ACTION))).await;
        let (url, _arn) = emulator.queue(case, "sent-to").await;
        let endpoint = emulator
            .api(
                case,
                subtype_integration(subtype, Some(&role), parameters(&url)),
                "POST /orders",
            )
            .await
            .1;
        let (status, answered) = emulator
            .invoke(&endpoint, "/orders", &[], r#"{"note":"placed"}"#)
            .await;
        let expected_status = if expected.is_some() {
            200
        } else {
            INTEGRATION_FAULT
        };
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status} from the route, got {status} {answered}"
        );
        let delivered = emulator.drained(case, &url).await;
        match expected {
            Some(body) => {
                // The answer is Amazon SQS's own, in the document that api answers a send
                // with, carrying the id that service minted.
                assert!(
                    answered_message_id(&answered).is_some(),
                    "case {case:?}: expected the queue's own message id in {answered}"
                );
                assert_eq!(
                    delivered,
                    vec![Value::String((*body).to_string())],
                    "case {case:?}: expected the mapped message on the queue, got {delivered:?}"
                );
            }
            None => {
                // Nothing was CALLED, so the class is the gateway's own `500` and not the
                // answer a service that was called and refused the action passes through.
                assert_eq!(
                    answered["message"].as_str().unwrap_or_default(),
                    INTEGRATION_FAULT_MESSAGE,
                    "case {case:?}: expected the gateway's own sentence, got {answered}"
                );
                assert!(
                    delivered.is_empty(),
                    "case {case:?}: expected nothing on the queue, got {delivered:?}"
                );
            }
        }
    }
}

/// RULE: the caller of a first-class subtype route is answered the action's own answer in the
/// document the interface the action was performed over states one in — for a send to a
/// queue, the `{Action}Response` envelope of the queue api's query interface, carrying the
/// result members that service answered and the `ResponseMetadata` that names the request —
/// so the id a client reads off that answer is the id its consumer reads off the queue.
#[tokio::test]
async fn a_subtype_route_answers_the_document_the_serving_service_answered() {
    /// Which queue a row sends to, which is what decides whether the action answers a
    /// `SequenceNumber` at all.
    #[derive(Debug, Clone, Copy)]
    enum Queue {
        Standard,
        Fifo,
    }
    // Rows differ along the mapping the message body comes from, the TEXT it carries, and
    // the kind of queue it is sent to — which is what varies the members the service answers.
    struct Row {
        case: &'static str,
        queue: Queue,
        /// The mapping, which is a function of the queue's url: it only exists once the
        /// queue does.
        parameters: fn(&str) -> Value,
        body: &'static str,
        /// The text the queue must then hold for the message.
        queued: &'static str,
    }
    let rows: &[Row] = &[
        Row {
            case: "a member of the body onto a standard queue",
            queue: Queue::Standard,
            parameters: |url| json!({ "QueueUrl": url, "MessageBody": "$request.body.note" }),
            body: r#"{"note":"placed"}"#,
            queued: "placed",
        },
        Row {
            case: "a body carrying XML metacharacters",
            queue: Queue::Standard,
            parameters: |url| json!({ "QueueUrl": url, "MessageBody": "$request.body.note" }),
            body: r#"{"note":"a<b&c\"d'e>f"}"#,
            queued: "a<b&c\"d'e>f",
        },
        Row {
            case: "the whole request body onto a standard queue",
            queue: Queue::Standard,
            parameters: |url| json!({ "QueueUrl": url, "MessageBody": "$request.body" }),
            body: r#"{"note":"whole"}"#,
            queued: r#"{"note":"whole"}"#,
        },
        Row {
            case: "a member of the body onto a FIFO queue",
            queue: Queue::Fifo,
            parameters: |url| {
                json!({
                    "QueueUrl": url,
                    "MessageBody": "$request.body.note",
                    "MessageGroupId": "orders",
                    "MessageDeduplicationId": "$request.body.note",
                })
            },
            body: r#"{"note":"sequenced"}"#,
            queued: "sequenced",
        },
    ];
    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        let role = emulator.role(case, Some(grant(SEND_MESSAGE_ACTION))).await;
        let url = match row.queue {
            Queue::Standard => emulator.queue(case, "sent-to").await.0,
            Queue::Fifo => emulator.fifo_queue(case, "sent-to.fifo").await,
        };
        let endpoint = emulator
            .api(
                case,
                subtype_integration("SQS-SendMessage", Some(&role), (row.parameters)(&url)),
                "POST /send",
            )
            .await
            .1;
        let (status, media_type, answered) =
            emulator.answered(&endpoint, "/send", &[], row.body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the action's own answer, got {status} {answered}"
        );
        // The answer is the document the queue api answers a send with, under the media type
        // that interface states one in: the result members that service answered, under the
        // element the action names, beside the request it names — not a re-encoding of them
        // into another shape or media type.
        assert_eq!(
            media_type, XML_MEDIA_TYPE,
            "case {case:?}: expected {XML_MEDIA_TYPE:?}, got {media_type:?} for {answered}"
        );
        let digests = envelope_elements(&answered, "MD5OfMessageBody");
        assert_eq!(
            digests.len(),
            1,
            "case {case:?}: expected one MD5OfMessageBody in {answered}, got {digests:?}"
        );
        let request_ids = envelope_elements(&answered, "RequestId");
        assert!(
            request_ids.len() == 1
                && service_common::query_error_envelope::is_request_id(&request_ids[0]),
            "case {case:?}: expected the answer to name its request, got {request_ids:?}"
        );
        let sequence_numbers = envelope_elements(&answered, "SequenceNumber");
        let sequenced = matches!(row.queue, Queue::Fifo);
        assert_eq!(
            !sequence_numbers.is_empty(),
            sequenced,
            "case {case:?}: expected a SequenceNumber={sequenced}, got {sequence_numbers:?} in \
             {answered}"
        );
        // And the id it carries is the id the queue holds for the message the mapping sent.
        let received = emulator.received(case, &url).await;
        assert_eq!(
            received.len(),
            1,
            "case {case:?}: expected the mapped message on the queue, got {received:?}"
        );
        assert_eq!(
            received[0]["Body"].as_str().unwrap_or_default(),
            row.queued,
            "case {case:?}: expected the mapped text on the queue, got {received:?}"
        );
        assert_eq!(
            answered_message_id(&answered).as_deref(),
            received[0]["MessageId"].as_str(),
            "case {case:?}: expected the id the answer carries to be the id the consumer \
             reads, got {answered} against {received:?}"
        );
    }
}

/// RULE: a request that carries no value for the mapping expression the integration maps onto
/// a parameter the action REQUIRES is refused as a bad request — the mapping is the api's own
/// configuration and the request is what failed to satisfy it — and no action is performed
/// for it; a request that carries nothing for an OPTIONAL parameter's expression is performed
/// without that parameter.
#[tokio::test]
async fn a_request_that_cannot_satisfy_a_required_mapping_is_refused() {
    // Rows differ along WHICH required parameter the request fails to supply and HOW it
    // fails: a member no body holds, a body that is no document at all, an absent header, a
    // variable the stage does not state, and the queue the send is addressed to — plus the
    // optional parameter that is no refusal at all.
    struct Row {
        case: &'static str,
        parameters: fn(&str) -> Value,
        body: &'static str,
        /// The text the queue must hold afterwards, or `None` for a request no send was made
        /// for.
        queued: Option<&'static str>,
    }
    let rows: &[Row] = &[
        Row {
            case: "a body member the body does not hold",
            parameters: |url| json!({ "QueueUrl": url, "MessageBody": "$request.body.msg" }),
            body: r#"{"other":"x"}"#,
            queued: None,
        },
        Row {
            case: "a body that is no document at all",
            parameters: |url| json!({ "QueueUrl": url, "MessageBody": "$request.body.msg" }),
            body: "not a document",
            queued: None,
        },
        Row {
            case: "a header the request does not carry",
            parameters: |url| json!({ "QueueUrl": url, "MessageBody": "$request.header.x-note" }),
            body: r#"{"msg":"placed"}"#,
            queued: None,
        },
        Row {
            case: "a variable the stage does not state",
            parameters: |url| json!({ "QueueUrl": url, "MessageBody": "$stageVariables.absent" }),
            body: r#"{"msg":"placed"}"#,
            queued: None,
        },
        Row {
            case: "the queue the send is addressed to",
            parameters: |_url| json!({ "QueueUrl": "$request.body.queue", "MessageBody": "placed" }),
            body: r#"{"msg":"placed"}"#,
            queued: None,
        },
        Row {
            case: "an optional parameter the request does not carry",
            parameters: |url| {
                json!({
                    "QueueUrl": url,
                    "MessageBody": "$request.body.msg",
                    "DelaySeconds": "$request.body.delay",
                })
            },
            body: r#"{"msg":"placed"}"#,
            queued: Some("placed"),
        },
    ];
    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        let role = emulator.role(case, Some(grant(SEND_MESSAGE_ACTION))).await;
        let (url, _arn) = emulator.queue(case, "sent-to").await;
        let endpoint = emulator
            .api(
                case,
                subtype_integration("SQS-SendMessage", Some(&role), (row.parameters)(&url)),
                "POST /send",
            )
            .await
            .1;
        let (status, answered) = emulator.invoke(&endpoint, "/send", &[], row.body).await;
        let expected_status = if row.queued.is_some() {
            200
        } else {
            BAD_REQUEST
        };
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status} from the route, got {status} {answered}"
        );
        let delivered = emulator.drained(case, &url).await;
        match row.queued {
            None => {
                assert_eq!(
                    answered["message"].as_str().unwrap_or_default(),
                    BAD_REQUEST_MESSAGE,
                    "case {case:?}: expected the gateway's own sentence, got {answered}"
                );
                assert!(
                    delivered.is_empty(),
                    "case {case:?}: expected nothing on the queue, got {delivered:?}"
                );
            }
            Some(queued) => {
                assert!(
                    answered_message_id(&answered).is_some(),
                    "case {case:?}: expected the queue's own message id in {answered}"
                );
                assert_eq!(
                    delivered,
                    vec![Value::String(queued.to_string())],
                    "case {case:?}: expected the mapped message on the queue, got {delivered:?}"
                );
            }
        }
    }
}

/// The IAM actions the three queue subtypes below need
/// (`<arnNamespace>:<OperationName>`), and the attribute the backlog of a queue is reported
/// under.
const RECEIVE_MESSAGE_ACTION: &str = "sqs:ReceiveMessage";
const DELETE_MESSAGE_ACTION: &str = "sqs:DeleteMessage";
const PURGE_QUEUE_ACTION: &str = "sqs:PurgeQueue";
const VISIBLE_COUNT: &str = "ApproximateNumberOfMessages";

/// The status and error shape Amazon SQS answers a call the policies in force do not permit,
/// which is what a relayed refusal of a queue action carries.
const QUEUE_ACCESS_DENIED_STATUS: u16 = 403;
const QUEUE_ACCESS_DENIED: &str = "AccessDenied";

/// The member a relayed refusal names the error shape under.
const SHAPE_MEMBER: &str = "__type";

/// How long a poll waits for a condition to hold, and how long one turn of it waits before
/// looking again.
///
/// Generous on purpose: what is waited for is the CONDITION — a message answered off the
/// queue, a queue reporting an empty backlog — re-read until it holds, so a slow or loaded
/// host only takes more turns of the loop.
const QUEUE_POLL_DEADLINE: Duration = Duration::from_secs(60);
const QUEUE_POLL_INTERVAL: Duration = Duration::from_millis(20);

impl Emulator {
    /// A queue whose received messages stay visible, so what a row asserts about the backlog
    /// is about the action under test and not about a visibility window: "VisibilityTimeout —
    /// the visibility timeout for the queue, in seconds" (`CreateQueue$Attributes`), zero
    /// being that attribute's documented floor. Answers its url.
    async fn queue_without_a_visibility_window(&self, case: &str, name: &str) -> String {
        self.queue_with(case, name, json!({ "VisibilityTimeout": "0" }))
            .await
    }

    /// One message onto the queue at `url`, sent with that service's own `SendMessage`;
    /// answers the id it minted for it.
    async fn sent(&self, case: &str, url: &str, body: &str) -> String {
        let sent = self
            .sqs(case, "SendMessage", &json!({ "QueueUrl": url, "MessageBody": body }))
            .await;
        sent["MessageId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no MessageId in {sent}"))
            .to_string()
    }

    /// The receipt handle of one message waiting on the queue at `url`, taken with that
    /// service's own `ReceiveMessage` — the handle a consumer would delete by.
    async fn one_handle(&self, case: &str, url: &str) -> String {
        let received = self.received(case, url).await;
        received
            .first()
            .and_then(|message| message["ReceiptHandle"].as_str())
            .unwrap_or_else(|| panic!("case {case:?}: no ReceiptHandle in {received:?}"))
            .to_string()
    }

    /// How many messages the queue at `url` reports waiting, read through that service's own
    /// `GetQueueAttributes`.
    async fn waiting(&self, case: &str, url: &str) -> i64 {
        let attributes = self
            .sqs(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": url, "AttributeNames": [VISIBLE_COUNT] }),
            )
            .await;
        attributes["Attributes"][VISIBLE_COUNT]
            .as_str()
            .and_then(|count| count.parse::<i64>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: no {VISIBLE_COUNT} in {attributes}"))
    }

    /// The queue at `url` once it reports NO message waiting: the backlog is re-read until
    /// that holds, or until a deadline far beyond any span this emulator owes has passed.
    async fn until_empty(&self, case: &str, url: &str) {
        let deadline = Instant::now() + QUEUE_POLL_DEADLINE;
        loop {
            let waiting = self.waiting(case, url).await;
            if waiting == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the queue to hold no message, got {waiting}"
            );
            tokio::time::sleep(QUEUE_POLL_INTERVAL).await;
        }
    }

    /// One invoke of a READING route, repeated until its answer carries a message or a
    /// deadline far beyond any span this emulator owes has passed: a receive takes what is
    /// waiting at the instant it is served, so the CONDITION is polled rather than a time
    /// waited.
    async fn invoke_until_a_message_is_answered(
        &self,
        case: &str,
        endpoint: &str,
        path: &str,
    ) -> (u16, String, Value) {
        let deadline = Instant::now() + QUEUE_POLL_DEADLINE;
        loop {
            let answered = self.answered(endpoint, path, &[], "").await;
            if answered.0 != 200 || !envelope_elements(&answered.2, "Body").is_empty() {
                return answered;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the route to answer a message, got {:?}",
                answered.2
            );
            tokio::time::sleep(QUEUE_POLL_INTERVAL).await;
        }
    }
}

/// Every element NAME the document the route answered carries, in document order.
///
/// Parsed with `quick-xml`, like [`envelope_elements`], rather than by slicing the body: which
/// elements the envelope HAS is read the way a client's own XML reader reads it. A body that
/// is not well-formed XML fails the case.
fn envelope_element_names(case: &str, answered: &Value) -> Vec<String> {
    let body = answered.as_str().unwrap_or_default().to_string();
    let mut reader = Reader::from_str(&body);
    let mut names: Vec<String> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                names.push(String::from_utf8_lossy(start.name().as_ref()).to_string());
            }
            Ok(Event::Empty(empty)) => {
                names.push(String::from_utf8_lossy(empty.name().as_ref()).to_string());
            }
            Ok(Event::Eof) => return names,
            Ok(_) => {}
            Err(error) => panic!("case {case:?}: expected XML, got {error} in {body:?}"),
        }
    }
}

/// What a row asserts once its route was invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueOutcome {
    /// The answer carries the message the queue held — its body, the id its send minted, and
    /// a handle that still deletes it.
    AnswersTheMessage,
    /// The answer carries the result element and no message at all, the queue holding none.
    AnswersNoMessage,
    /// The queue holds no message afterwards.
    EmptiesTheQueue,
}

/// RULE: a route whose integration names a first-class Amazon SQS QUEUE subtype performs that
/// action — the read, the delete, the purge — with the parameters its `RequestParameters` map,
/// under the role its `CredentialsArn` names, and answers `200` carrying the queue api's own
/// query-interface `{Action}Response` envelope: the members that action's output declares, and
/// no result element at all for an action whose output declares none. So what a client reads
/// off the answer, and what the queue holds afterwards, are what the action did.
#[tokio::test]
async fn a_queue_subtype_route_performs_the_action_its_subtype_names() {
    /// One case: the subtype, the permission its action needs, the route, the mapping, the
    /// messages the queue holds first, and what the invoke must then have done.
    struct Row {
        case: &'static str,
        subtype: &'static str,
        permission: &'static str,
        route_key: &'static str,
        path: &'static str,
        bodies: &'static [&'static str],
        /// Whether the invoke carries the receipt handle of a message a consumer took off the
        /// queue, which is the value a delete is mapped from.
        carries_handle: bool,
        parameters: fn(&str) -> Value,
        /// Whether the envelope carries the `{Action}Result` element, which is whether the
        /// action's own output declares any member at all.
        result_element: bool,
        outcome: QueueOutcome,
    }
    // Rows differ along the ACTION and along what the queue holds when it is performed: a read
    // of a queue holding one message, a read of a queue holding none, a delete by the handle a
    // consumer holds, and a purge of a queue holding several.
    let rows: &[Row] = &[
        Row {
            case: "a read of a queue holding one message",
            subtype: "SQS-ReceiveMessage",
            permission: RECEIVE_MESSAGE_ACTION,
            route_key: "POST /recv",
            path: "/recv",
            bodies: &["a marker"],
            carries_handle: false,
            parameters: |url| json!({ "QueueUrl": url, "MaxNumberOfMessages": "10" }),
            result_element: true,
            outcome: QueueOutcome::AnswersTheMessage,
        },
        Row {
            case: "a read of a queue holding none",
            subtype: "SQS-ReceiveMessage",
            permission: RECEIVE_MESSAGE_ACTION,
            route_key: "POST /recv",
            path: "/recv",
            bodies: &[],
            carries_handle: false,
            parameters: |url| json!({ "QueueUrl": url }),
            result_element: true,
            outcome: QueueOutcome::AnswersNoMessage,
        },
        Row {
            case: "a delete by the handle a consumer holds",
            subtype: "SQS-DeleteMessage",
            permission: DELETE_MESSAGE_ACTION,
            route_key: "POST /delete",
            path: "/delete",
            bodies: &["to be deleted"],
            carries_handle: true,
            parameters: |url| {
                json!({ "QueueUrl": url, "ReceiptHandle": "$request.header.x-receipt-handle" })
            },
            result_element: false,
            outcome: QueueOutcome::EmptiesTheQueue,
        },
        Row {
            case: "a purge of a queue holding several",
            subtype: "SQS-PurgeQueue",
            permission: PURGE_QUEUE_ACTION,
            route_key: "POST /purge",
            path: "/purge",
            bodies: &["one", "two", "three"],
            carries_handle: false,
            parameters: |url| json!({ "QueueUrl": url }),
            result_element: false,
            outcome: QueueOutcome::EmptiesTheQueue,
        },
    ];
    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        let role = emulator.role(case, Some(grant(row.permission))).await;
        let url = emulator
            .queue_without_a_visibility_window(case, "subject")
            .await;
        let mut sent: Vec<String> = Vec::new();
        for body in row.bodies {
            sent.push(emulator.sent(case, &url, body).await);
        }
        let handle = if row.carries_handle {
            emulator.one_handle(case, &url).await
        } else {
            String::new()
        };
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if row.carries_handle {
            headers.push(("x-receipt-handle", handle.as_str()));
        }
        let endpoint = emulator
            .api(
                case,
                subtype_integration(row.subtype, Some(&role), (row.parameters)(&url)),
                row.route_key,
            )
            .await
            .1;

        let (status, media_type, answered) = if row.outcome == QueueOutcome::AnswersTheMessage {
            emulator
                .invoke_until_a_message_is_answered(case, &endpoint, row.path)
                .await
        } else {
            emulator.answered(&endpoint, row.path, &headers, "").await
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from the route, got {status} {answered}"
        );
        assert_eq!(
            media_type, XML_MEDIA_TYPE,
            "case {case:?}: expected the media type a query interface answers under, got \
             {media_type:?}"
        );
        // The envelope is the ACTION's own: its response element around the result element
        // the action's output declares members for, and the `ResponseMetadata` naming the
        // request.
        let action = row
            .subtype
            .split_once('-')
            .map(|(_service, action)| action)
            .unwrap_or_default();
        let names = envelope_element_names(case, &answered);
        assert_eq!(
            names.first().map(String::as_str),
            Some(format!("{action}Response").as_str()),
            "case {case:?}: expected the action's own response element, got {names:?}"
        );
        assert_eq!(
            names.contains(&format!("{action}Result")),
            row.result_element,
            "case {case:?}: expected a result element={}, got {names:?}",
            row.result_element
        );
        assert!(
            envelope_elements(&answered, "RequestId")
                .first()
                .is_some_and(|named| !named.is_empty()),
            "case {case:?}: expected the ResponseMetadata to name the request, got {answered}"
        );

        match row.outcome {
            QueueOutcome::AnswersTheMessage => {
                let expected: Vec<String> =
                    row.bodies.iter().map(|body| (*body).to_string()).collect();
                assert_eq!(
                    envelope_elements(&answered, "Body"),
                    expected,
                    "case {case:?}: expected the queue's own message, got {answered}"
                );
                assert_eq!(
                    envelope_elements(&answered, "MessageId"),
                    sent,
                    "case {case:?}: expected the id the send minted, got {answered}"
                );
                // The handle the answer carries still deletes the message, which is what
                // makes the read usable.
                let answered_handle = envelope_elements(&answered, "ReceiptHandle")
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| panic!("case {case:?}: no ReceiptHandle in {answered}"));
                emulator
                    .sqs(
                        case,
                        "DeleteMessage",
                        &json!({ "QueueUrl": url, "ReceiptHandle": answered_handle }),
                    )
                    .await;
                emulator.until_empty(case, &url).await;
            }
            QueueOutcome::AnswersNoMessage => {
                assert!(
                    !names.contains(&"Message".to_string()),
                    "case {case:?}: expected no message in {answered}"
                );
                assert_eq!(
                    emulator.waiting(case, &url).await,
                    0,
                    "case {case:?}: expected the queue to be left empty"
                );
            }
            QueueOutcome::EmptiesTheQueue => emulator.until_empty(case, &url).await,
        }
    }
}

/// RULE: a queue action too is performed as a session of the role the integration's
/// `CredentialsArn` names, so Amazon SQS decides on that role's own policies whether it
/// happens — and a route whose role does not hold the action's permission is answered THAT
/// service's own refusal, relayed, with the queue untouched.
#[tokio::test]
async fn a_queue_subtype_route_is_performed_as_the_role_its_credentials_arn_names() {
    // Rows differ along the ACTION whose permission the role does not hold: each of the three
    // queue subtypes performed above, with the mapping that action needs.
    type Parameters = fn(&str) -> Value;
    let rows: &[(&str, &str, &str, &str, Parameters)] = &[
        (
            "a read the role may not make",
            "SQS-ReceiveMessage",
            "POST /recv",
            "/recv",
            |url| json!({ "QueueUrl": url }),
        ),
        (
            "a delete the role may not make",
            "SQS-DeleteMessage",
            "POST /delete",
            "/delete",
            // The handle is a static value: the action is refused on the role's policies
            // before the queue resolves a handle at all, so no real one is needed to drive
            // the refusal, and the message must still be waiting afterwards.
            |url| json!({ "QueueUrl": url, "ReceiptHandle": "a-handle-no-receive-minted" }),
        ),
        (
            "a purge the role may not make",
            "SQS-PurgeQueue",
            "POST /purge",
            "/purge",
            |url| json!({ "QueueUrl": url }),
        ),
    ];
    for (case, subtype, route_key, path, parameters) in rows {
        let emulator = Emulator::new();
        // A grant of ANOTHER action of the same service, so the role exists and is assumable
        // and only the permission under test is missing.
        let role = emulator.role(case, Some(grant("sqs:SendMessage"))).await;
        let url = emulator
            .queue_without_a_visibility_window(case, "subject")
            .await;
        emulator.sent(case, &url, "stays on the queue").await;
        let endpoint = emulator
            .api(
                case,
                subtype_integration(subtype, Some(&role), parameters(&url)),
                route_key,
            )
            .await
            .1;
        let (status, answered) = emulator.invoke(&endpoint, path, &[], "").await;
        assert_eq!(
            status, QUEUE_ACCESS_DENIED_STATUS,
            "case {case:?}: expected the service's own refusal, got {status} {answered}"
        );
        assert_eq!(
            answered[SHAPE_MEMBER].as_str().unwrap_or_default(),
            QUEUE_ACCESS_DENIED,
            "case {case:?}: expected the error shape that service named, got {answered}"
        );
        assert_eq!(
            emulator.waiting(case, &url).await,
            1,
            "case {case:?}: expected the queue to be untouched"
        );
    }
}

/// The namespace the QUERY interface of the queue api declares every document it answers in:
/// `http://queue.amazonaws.com/doc/<version>/`, whose version is the api version that model
/// states (`smithy_query(query="service_info")` on the Amazon SQS model: `2012-11-05`). A
/// client of such an interface reads the envelope with a namespace-aware reader, so the binding
/// is part of the answer and not decoration on it.
const QUEUE_QUERY_NAMESPACE: &str = "http://queue.amazonaws.com/doc/2012-11-05/";

/// The one route every row of the suite below carries, and the path its client posts to.
const ACTION_ROUTE_KEY: &str = "POST /act";
const ACTION_PATH: &str = "/act";

/// Every element of the document the route answered: the PATH of local names leading to it from
/// the document element, joined by `/`, the namespace the element itself RESOLVES into, and the
/// character data it carries.
///
/// Read with `quick-xml`'s NAMESPACE-aware reader, which is how a client of a query interface
/// reads one: a default namespace declared on the document element is in scope on every element
/// under it, and that is what makes a path like `SendMessageResponse/SendMessageResult/MessageId`
/// resolvable by a client asking for it in the serving interface's own namespace. A body that is
/// not a well-formed document fails the case rather than reading as nothing.
fn resolved_elements(case: &str, answered: &Value) -> Vec<(String, String, String)> {
    let body = answered.as_str().unwrap_or_default().to_string();
    let mut reader = NsReader::from_str(&body);
    let mut open: Vec<String> = Vec::new();
    let mut found: Vec<(String, String, String)> = Vec::new();
    loop {
        match reader.read_resolved_event() {
            Ok((resolved, Event::Start(start) | Event::Empty(start))) => {
                let namespace = match resolved {
                    ResolveResult::Bound(uri) => String::from_utf8_lossy(uri.0).to_string(),
                    _ => String::new(),
                };
                open.push(String::from_utf8_lossy(start.local_name().as_ref()).to_string());
                found.push((open.join("/"), namespace, String::new()));
            }
            Ok((_, Event::Text(text))) => {
                let decoded = text.xml10_content().unwrap_or_else(|error| {
                    panic!("case {case:?}: the answer must decode: {error}")
                });
                let path = open.join("/");
                if let Some(element) = found.last_mut() {
                    if element.0 == path {
                        element.2.push_str(&decoded);
                    }
                }
            }
            Ok((_, Event::End(_))) => {
                open.pop();
            }
            Ok((_, Event::Eof)) => return found,
            Ok(_) => {}
            Err(error) => {
                panic!("case {case:?}: expected a well-formed document, got {error} in {body:?}")
            }
        }
    }
}

/// The character data of every element the document carries at `path`, in document order.
fn at_path<'a>(elements: &'a [(String, String, String)], path: &str) -> Vec<&'a str> {
    elements
        .iter()
        .filter(|(element, _namespace, _text)| element == path)
        .map(|(_element, _namespace, text)| text.as_str())
        .collect()
}

/// RULE: the answer a first-class subtype route relays is the SERVING interface's own document,
/// read the way that interface's client reads it — for the queue api's query interface, the
/// `{Action}Response` element declared in that interface's namespace, carrying the members the
/// action's output declares under `{Action}Result` (and no result element for an action whose
/// output declares none) beside the `ResponseMetadata` naming the request, every element of it
/// in that same namespace — so the value a client asks for by its documented path, such as the
/// id of the message a send just enqueued, is the value the owning service holds.
#[tokio::test]
async fn a_queue_subtype_route_answers_the_serving_interfaces_own_namespaced_document() {
    /// One case: the action, the permission it needs, the queue it acts on, the mapping, the
    /// request, and what a client must then be able to read off the answer.
    struct Row {
        case: &'static str,
        subtype: &'static str,
        permission: &'static str,
        /// A FIFO queue, the one kind whose send is answered a `SequenceNumber`.
        fifo: bool,
        /// The messages already waiting on the queue when the route is invoked.
        waiting: &'static [&'static str],
        /// Whether the invoke carries the receipt handle of a message a consumer took off the
        /// queue, which is the value a delete is mapped from.
        carries_handle: bool,
        parameters: fn(&str) -> Value,
        body: &'static str,
        /// Whether the answer has to carry a message, which is a condition a read is polled
        /// for rather than a time waited.
        answers_a_message: bool,
        /// The paths a client reads a value at: each must resolve into the interface's
        /// namespace and carry a value.
        read_at: &'static [&'static str],
        /// The paths the answer must carry NO element at.
        absent: &'static [&'static str],
        /// Whether the id under the result element is the id the consumer then reads off the
        /// queue — what a client of a send correlates its send by.
        correlates_the_queued_id: bool,
    }
    // Rows differ along the ACTION performed (each of the four first-class queue subtypes),
    // along the kind of queue it acts on, and so along WHICH members the answer carries: a
    // send answered an id, the same send answered a sequence too, a read answered a whole
    // message, and the two actions whose output declares no member at all.
    let rows: &[Row] = &[
        Row {
            case: "a send of a mapped body to a standard queue",
            subtype: "SQS-SendMessage",
            permission: SEND_MESSAGE_ACTION,
            fifo: false,
            waiting: &[],
            carries_handle: false,
            parameters: |url| json!({ "QueueUrl": url, "MessageBody": "$request.body.msg" }),
            body: r#"{"msg":"a marker"}"#,
            answers_a_message: false,
            read_at: &[
                "SendMessageResponse/SendMessageResult/MessageId",
                "SendMessageResponse/SendMessageResult/MD5OfMessageBody",
                "SendMessageResponse/ResponseMetadata/RequestId",
            ],
            // `SendMessageResult$SequenceNumber` "applies only to FIFO queues", so a standard
            // queue's send carries none.
            absent: &["SendMessageResponse/SendMessageResult/SequenceNumber"],
            correlates_the_queued_id: true,
        },
        Row {
            case: "a send of a mapped body to a FIFO queue",
            subtype: "SQS-SendMessage",
            permission: SEND_MESSAGE_ACTION,
            fifo: true,
            waiting: &[],
            carries_handle: false,
            parameters: |url| {
                json!({
                    "QueueUrl": url,
                    "MessageBody": "$request.body.msg",
                    "MessageGroupId": "orders",
                    "MessageDeduplicationId": "$request.body.msg",
                })
            },
            // The text is the deduplication id too, which "can only include alphanumeric and
            // punctuation characters" (`SendMessage$MessageDeduplicationId`).
            body: r#"{"msg":"a-sequenced-marker"}"#,
            answers_a_message: false,
            read_at: &[
                "SendMessageResponse/SendMessageResult/MessageId",
                "SendMessageResponse/SendMessageResult/SequenceNumber",
                "SendMessageResponse/ResponseMetadata/RequestId",
            ],
            absent: &[],
            correlates_the_queued_id: true,
        },
        Row {
            case: "a read of a queue holding one message",
            subtype: "SQS-ReceiveMessage",
            permission: RECEIVE_MESSAGE_ACTION,
            fifo: false,
            waiting: &["a marker a consumer reads"],
            carries_handle: false,
            parameters: |url| json!({ "QueueUrl": url, "MaxNumberOfMessages": "10" }),
            body: "",
            answers_a_message: true,
            read_at: &[
                "ReceiveMessageResponse/ReceiveMessageResult/Message/MessageId",
                "ReceiveMessageResponse/ReceiveMessageResult/Message/Body",
                "ReceiveMessageResponse/ReceiveMessageResult/Message/ReceiptHandle",
                "ReceiveMessageResponse/ResponseMetadata/RequestId",
            ],
            absent: &[],
            correlates_the_queued_id: false,
        },
        Row {
            case: "a delete by the handle a consumer holds",
            subtype: "SQS-DeleteMessage",
            permission: DELETE_MESSAGE_ACTION,
            fifo: false,
            waiting: &["to be deleted"],
            carries_handle: true,
            parameters: |url| {
                json!({ "QueueUrl": url, "ReceiptHandle": "$request.header.x-receipt-handle" })
            },
            body: "",
            answers_a_message: false,
            read_at: &["DeleteMessageResponse/ResponseMetadata/RequestId"],
            // `DeleteMessage` declares no output member, so that interface writes no result
            // element for it.
            absent: &["DeleteMessageResponse/DeleteMessageResult"],
            correlates_the_queued_id: false,
        },
        Row {
            case: "a purge of a queue holding several",
            subtype: "SQS-PurgeQueue",
            permission: PURGE_QUEUE_ACTION,
            fifo: false,
            waiting: &["one", "two"],
            carries_handle: false,
            parameters: |url| json!({ "QueueUrl": url }),
            body: "",
            answers_a_message: false,
            read_at: &["PurgeQueueResponse/ResponseMetadata/RequestId"],
            absent: &["PurgeQueueResponse/PurgeQueueResult"],
            correlates_the_queued_id: false,
        },
    ];
    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        let role = emulator.role(case, Some(grant(row.permission))).await;
        let url = if row.fifo {
            emulator.fifo_queue(case, "subject.fifo").await
        } else {
            emulator
                .queue_without_a_visibility_window(case, "subject")
                .await
        };
        for waiting in row.waiting {
            emulator.sent(case, &url, waiting).await;
        }
        let handle = if row.carries_handle {
            emulator.one_handle(case, &url).await
        } else {
            String::new()
        };
        let headers: Vec<(&str, &str)> = if row.carries_handle {
            vec![("x-receipt-handle", handle.as_str())]
        } else {
            Vec::new()
        };
        let endpoint = emulator
            .api(
                case,
                subtype_integration(row.subtype, Some(&role), (row.parameters)(&url)),
                ACTION_ROUTE_KEY,
            )
            .await
            .1;

        let (status, media_type, answered) = if row.answers_a_message {
            emulator
                .invoke_until_a_message_is_answered(case, &endpoint, ACTION_PATH)
                .await
        } else {
            emulator
                .answered(&endpoint, ACTION_PATH, &headers, row.body)
                .await
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected the action's own answer, got {status} {answered}"
        );
        assert_eq!(
            media_type, XML_MEDIA_TYPE,
            "case {case:?}: expected {XML_MEDIA_TYPE:?}, got {media_type:?} for {answered}"
        );

        // The document element is the action's own response element, declared in the serving
        // interface's namespace, and every element under it resolves into that namespace — so
        // a namespace-aware client reader finds each of them.
        let action = row
            .subtype
            .split_once('-')
            .map(|(_service, action)| action)
            .unwrap_or_default();
        let elements = resolved_elements(case, &answered);
        let (path, namespace, _text) = elements.first().cloned().unwrap_or_default();
        let response_element = format!("{action}Response");
        assert_eq!(
            (path.as_str(), namespace.as_str()),
            (response_element.as_str(), QUEUE_QUERY_NAMESPACE),
            "case {case:?}: expected the action's own response element in the interface's \
             namespace, got {answered}"
        );
        for (path, namespace, _text) in &elements {
            assert_eq!(
                namespace, QUEUE_QUERY_NAMESPACE,
                "case {case:?}: expected {path:?} to resolve into the interface's namespace, \
                 got {namespace:?} in {answered}"
            );
        }

        // And each value a client reads is at the path that interface states it at.
        for path in row.read_at {
            let values = at_path(&elements, path);
            assert!(
                values.len() == 1 && !values[0].is_empty(),
                "case {case:?}: expected one value at {path:?}, got {values:?} in {answered}"
            );
        }
        for path in row.absent {
            assert!(
                at_path(&elements, path).is_empty(),
                "case {case:?}: expected no element at {path:?} in {answered}"
            );
        }

        if row.correlates_the_queued_id {
            // The id the client read is the id the queue holds for the message its send
            // enqueued: that correlation is what the relayed document is for.
            let received = emulator.received(case, &url).await;
            assert_eq!(
                received.len(),
                1,
                "case {case:?}: expected the mapped message on the queue, got {received:?}"
            );
            assert_eq!(
                at_path(&elements, &format!("{action}Response/{action}Result/MessageId")),
                vec![received[0]["MessageId"].as_str().unwrap_or_default()],
                "case {case:?}: expected the id the answer carries to be the id the consumer \
                 reads, got {answered} against {received:?}"
            );
        }
    }
}
