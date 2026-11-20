// SPDX-License-Identifier: CC0-1.0

//! An HTTP api whose routes front Amazon SQS through the first-class `AWS_PROXY` QUEUE
//! subtypes, configured and then driven on an estate where every service the call passes
//! through is mounted together.
//!
//! Two rules, both of which hold for every first-class subtype and not only for the ones a
//! recording happened to exercise:
//!
//! RULE 1 (the WRITE): **an `AWS_PROXY` integration naming an `IntegrationSubtype` is created,
//! and patched, only when its `RequestParameters` DEFINE every parameter the action that
//! subtype names cannot be called without; a write that leaves any of them undefined is
//! refused `400 BadRequestException`, naming the subtype and its whole required list, and
//! stores nothing.** `Integration$IntegrationSubtype` is "supported only for HTTP API
//! AWS_PROXY integrations. Specifies the AWS service action to invoke" and
//! `Integration$RequestParameters` is "a key-value map specifying parameters that are passed
//! to AWS_PROXY integrations" — the ARGUMENTS of that one action — so a mapping that leaves a
//! required argument undefined configures a call that could never be made, and API Gateway
//! settles that at the write rather than at invoke time.
//!
//! RULE 2 (the ADDRESS and the ACTION): **an api's `ApiEndpoint` is an address THIS emulator
//! serves the api's data plane at, so a client holding that value ALONE reaches the api's
//! route, and the route's subtype action is carried out in the service that OWNS it as a
//! session of the role the integration's `CredentialsArn` names — so what the queue holds
//! afterwards is what the action did.** `Api$ApiEndpoint` is "the URI of the API" and "the
//! stage name is typically appended to this URI to form a complete path to a deployed API
//! stage", which is the only address a client of the control plane is given.
//!
//! Four crates have to agree for rule 2 to hold — API Gateway v2 publishes the address,
//! resolves the request to the route and performs the subtype, IAM holds the role and its
//! policies, the token service mints the session the call is made as, and Amazon SQS holds
//! the queue and decides the action on that session's own permissions — so the suite lives
//! here rather than in any one crate. The API Gateway service that owns REST apis is mounted
//! beside them because the `execute-api` endpoint space is SHARED by the two data planes: an
//! address that resolves at all is one exactly one of them claimed.
//!
//! Nothing asserted here is a value this gateway fills in: every effect is read back through
//! Amazon SQS's OWN operations, on the queue that service holds.
//!
//! The rows span what the rules generalise over: for rule 1 the subtype and the SHAPE of its
//! required list (one parameter and several, in that subtype's own order), which required
//! parameter is the one left out, and whether the write is the create or the patch; for rule 2
//! the subtype and so the action, the endpoint the control-plane client was configured with (an
//! IPv4 loopback literal, a loopback NAME, a bracketed IPv6 literal, one of AWS's own service
//! hosts), the Region, the stage and the form of the invoke URL built from the published value,
//! the number of messages already waiting, and where a mapped argument comes from (a static
//! value, a request header, a member of the request body). The recorded cases — three creates
//! with an empty mapping, and one send driven at the address a loopback client was published —
//! are four of those rows.
//!
//! What the owning crate's own suites pin and this one does not restate: the whole first-class
//! catalogue against the create and the patch
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/integration_validation.rs`),
//! the queue subtypes reached at the api's own documented host
//! (`.../tests/all/http_api_service_action_integration.rs`), and the addressing half of the
//! published endpoint on its own (`.../tests/all/http_api_published_invoke_address.rs`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigatewayv2_published_endpoint_runs_its_lambda_route::announced_parts;
use crate::query_wire::{element, elements, form_encode};

/// The status each write answers: the create's, the patch's and the read's.
const CREATED: u16 = 201;
const SERVED: u16 = 200;

/// The refusal of a write this service models no more specific error for, and the member its
/// text is carried under.
const BAD_REQUEST: u16 = 400;
const BAD_REQUEST_SHAPE: &str = "BadRequestException";
const MESSAGE_MEMBER: &str = "message";

/// The media type a first-class subtype route answers a performed action's document under: the
/// queue api's answers are stated in the one a QUERY interface uses.
const XML_MEDIA_TYPE: &str = "text/xml";

/// The role every integration below is performed as, and the inline policy the grant is
/// written on.
const ROLE_NAME: &str = "apigatewayv2-drives-the-queue";
const INLINE_POLICY: &str = "may-act-on-the-queue";

/// The attribute Amazon SQS reports a queue's backlog under
/// (`smithy_query(query="doc", shape="QueueAttributeName")` on the Amazon SQS model), and the
/// visibility window every queue below is created with: zero, that attribute's documented
/// floor, so what a row asserts about the backlog is about the action under test and not about
/// a message being temporarily out of sight.
const VISIBLE_COUNT: &str = "ApproximateNumberOfMessages";
const NO_VISIBILITY_WINDOW: &str = "0";

/// How long the queue is re-read for, and how often, while waiting for what an action did to
/// be observable. Generous on purpose: what is waited for is the CONDITION, and the budget is
/// never the thing asserted, so a loaded host only takes more turns of the loop.
const POLL_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The header a row that maps an argument out of the request sends it in.
const HANDLE_HEADER: &str = "x-receipt-handle";

/// API Gateway v2, the API Gateway service that owns REST apis, Amazon SQS, IAM and the token
/// service over ONE engine, as `core/router/src/lib.rs` mounts them: the action an integration
/// performs is dispatched back into this same router, so the queue it acts on is the queue
/// this suite created.
struct Estate {
    router: MultiServiceRouter,
    /// The Region every call is signed for, and the Region in the hosts of the other services.
    region: String,
    /// The endpoint the CONTROL-PLANE client is configured with: the `Host` its calls carry,
    /// which is what decides the address a minted api announces.
    control_host: String,
}

impl Estate {
    fn new(control_host: &str, region: &str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite exercises the services, not SigV4: a control-plane call carries a
        // credential scope so the router can name the target service, and the invoke of the
        // published address carries none, as a client of a published HTTP api endpoint does.
        // The INTEGRATION's own call is signed by the emulator, with the session it minted.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
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
            control_host: control_host.to_string(),
        }
    }

    /// A SigV4 `Authorization` whose credential scope names `service`, which is what routes
    /// the request. The signature is never checked here.
    fn authorization(&self, service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{}/{service}/aws4_request, \
             SignedHeaders=host, Signature=not-verified",
            self.region
        )
    }

    /// One Amazon SQS call that must be served (`awsJson1_0`, `AmazonSQS.<Operation>`).
    async fn sqs(&self, case: &str, operation: &str, body: &Value) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("sqs.{}.amazonaws.com", self.region))
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("AmazonSQS.{operation}"))
            .header("authorization", self.authorization("sqs"))
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = parsed_body(response.body());
        assert_eq!(
            status, SERVED,
            "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} \
             with {parsed}"
        );
        parsed
    }

    /// One awsQuery call to IAM that must be served.
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
            .header("host", "iam.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", self.authorization("iam"))
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, SERVED,
            "case {case:?}: IAM {action} must succeed, got {status} {answered}"
        );
    }

    /// One API Gateway v2 control-plane call (`restJson1` under `/v2`), addressed at the
    /// endpoint the CLIENT was configured with, as `(status, parsed body)`.
    async fn control(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let payload = match &body {
            Some(value) => Bytes::from(value.to_string()),
            None => Bytes::new(),
        };
        let host = self.control_host.clone();
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header("authorization", self.authorization("apigateway"))
            .body(payload)
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, parsed_body(response.body()))
    }

    /// One client request to a route of the deployed stage — the invoke the integration is
    /// performed for. `endpoint` is the address the api ANNOUNCED and `path` what the client
    /// appends to it; answers `(status, media type, the body as text)`. No credential: the
    /// route's `AuthorizationType` is the default `NONE`.
    ///
    /// Handed straight to the router IN PROCESS, so no socket is opened and nothing leaves
    /// this test: the authority is only the address the request carries.
    async fn invoke(
        &self,
        endpoint: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (u16, String, String) {
        let (authority, base_path) = announced_parts(endpoint);
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!("https://{authority}{base_path}{path}"))
            .header("host", authority.as_str())
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let media_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        (status, media_type, answered)
    }

    /// The role an integration's `CredentialsArn` names, trusted by API Gateway — written with
    /// IAM's own operations, so the role a session is minted of is the record `iam:CreateRole`
    /// wrote. Answers its ARN.
    async fn role(&self, case: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": "apigateway.amazonaws.com" },
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
        format!(
            "arn:aws:iam::{}:role/{ROLE_NAME}",
            engine::emulator_account_id()
        )
    }

    /// Write the grant of `action` onto that role as its inline identity policy.
    async fn grant(&self, case: &str, action: &str) {
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": action, "Resource": "*" }],
        })
        .to_string();
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

    /// A queue whose received messages stay visible, created with that service's own
    /// `CreateQueue`; answers its url.
    async fn queue(&self, case: &str, name: &str) -> String {
        let created = self
            .sqs(
                case,
                "CreateQueue",
                &json!({
                    "QueueName": name,
                    "Attributes": { "VisibilityTimeout": NO_VISIBILITY_WINDOW },
                }),
            )
            .await;
        text(case, &created, "QueueUrl")
    }

    /// One message onto the queue at `url`, sent with that service's own `SendMessage`;
    /// answers the id it minted for it.
    async fn send(&self, case: &str, url: &str, body: &str) -> String {
        let sent = self
            .sqs(
                case,
                "SendMessage",
                &json!({ "QueueUrl": url, "MessageBody": body }),
            )
            .await;
        text(case, &sent, "MessageId")
    }

    /// The receipt handle of one message waiting on the queue at `url`, taken with that
    /// service's own `ReceiveMessage` — the handle a consumer would delete by.
    async fn one_handle(&self, case: &str, url: &str) -> String {
        let received = self
            .sqs(
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        received["Messages"][0]["ReceiptHandle"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ReceiptHandle in {received}"))
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

    /// The queue at `url` once it reports exactly `expected` messages waiting: the backlog is
    /// re-read until that holds, or until a deadline far beyond any span this emulator owes
    /// has passed. Waits for the CONDITION rather than for a span.
    async fn until_waiting(&self, case: &str, url: &str, expected: i64) {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let waiting = self.waiting(case, url).await;
            if waiting == expected {
                return;
            }
            // A backlog waited on to GROW that has already gone past the count expected of it
            // is settled at the wrong value, and re-reading cannot bring it back: say so at
            // once rather than at the deadline. A backlog waited on to EMPTY (`expected` zero)
            // is above it until the action lands, so there the deadline is the only answer.
            assert!(
                waiting < expected || expected == 0,
                "case {case:?}: expected the queue to hold {expected} message(s), got {waiting}"
            );
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the queue to hold {expected} message(s), got {waiting}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Every message waiting on the queue at `url`, as `(body, id)`, taken off it with that
    /// service's own `ReceiveMessage`.
    async fn drained(&self, case: &str, url: &str) -> Vec<(String, String)> {
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
            .iter()
            .map(|message| {
                (
                    message["Body"].as_str().unwrap_or_default().to_string(),
                    message["MessageId"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    /// A bare HTTP api; answers `(ApiId, the address its ApiEndpoint announces)` — the value a
    /// client builds its invoke URL from, which [`invoke`](Self::invoke) takes as it stands.
    async fn create_api(&self, case: &str) -> (String, String) {
        let (status, api) = self
            .control(
                "POST",
                "/v2/apis",
                Some(json!({ "name": "queue-subtype-api", "protocolType": "HTTP" })),
            )
            .await;
        assert_eq!(
            status, CREATED,
            "case {case:?}: CreateApi must answer {CREATED}, got {status} {api}"
        );
        (text(case, &api, "apiId"), text(case, &api, "apiEndpoint"))
    }

    /// One CreateIntegration of a first-class subtype, as `(status, body)` — the write rule 1
    /// is about.
    async fn create_integration(
        &self,
        api_id: &str,
        subtype: &str,
        role_arn: &str,
        request_parameters: Value,
    ) -> (u16, Value) {
        self.control(
            "POST",
            &format!("/v2/apis/{api_id}/integrations"),
            Some(json!({
                "integrationType": "AWS_PROXY",
                "integrationSubtype": subtype,
                "payloadFormatVersion": "1.0",
                "credentialsArn": role_arn,
                "requestParameters": request_parameters,
            })),
        )
        .await
    }

    /// One UpdateIntegration carrying only a new mapping, as `(status, body)`.
    async fn patch_mapping(
        &self,
        api_id: &str,
        integration_id: &str,
        request_parameters: Value,
    ) -> (u16, Value) {
        self.control(
            "PATCH",
            &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
            Some(json!({ "requestParameters": request_parameters })),
        )
        .await
    }

    /// The integration of `integration_id`, read back through this service's own reader.
    async fn read_integration(&self, case: &str, api_id: &str, integration_id: &str) -> Value {
        let (status, read) = self
            .control(
                "GET",
                &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
                None,
            )
            .await;
        assert_eq!(
            status, SERVED,
            "case {case:?}: GetIntegration must answer {SERVED}, got {status} {read}"
        );
        read
    }

    /// How many integrations the api holds, read through its own collection reader — so a
    /// refused write is shown to have stored NOTHING rather than merely to have answered 400.
    async fn integration_count(&self, case: &str, api_id: &str) -> usize {
        let (status, listed) = self
            .control("GET", &format!("/v2/apis/{api_id}/integrations"), None)
            .await;
        assert_eq!(
            status, SERVED,
            "case {case:?}: GetIntegrations must answer {SERVED}, got {status} {listed}"
        );
        listed["items"].as_array().map_or(0, Vec::len)
    }

    /// One route over `integration_id`, and the stage that serves it with auto-deploy on.
    async fn deploy_route(
        &self,
        case: &str,
        api_id: &str,
        integration_id: &str,
        route_key: &str,
        stage: &str,
    ) {
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
            status, CREATED,
            "case {case:?}: CreateRoute must answer {CREATED}, got {status} {route}"
        );
        let (status, created) = self
            .control(
                "POST",
                &format!("/v2/apis/{api_id}/stages"),
                Some(json!({ "stageName": stage, "autoDeploy": true })),
            )
            .await;
        assert_eq!(
            status, CREATED,
            "case {case:?}: CreateStage must answer {CREATED}, got {status} {created}"
        );
    }
}

/// A response body as the document a client parses, or the text itself when it is not JSON.
fn parsed_body(body: &[u8]) -> Value {
    if body.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).to_string()))
}

/// One string member of a document that must carry it.
fn text(case: &str, document: &Value, member: &str) -> String {
    document[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} in {document}"))
        .to_string()
}

/// How API Gateway words the refusal of a subtype integration whose mapping leaves a parameter
/// its action requires undefined: fixed text, with the subtype the request named and that
/// subtype's whole required list, in the order it publishes them.
///
/// Written out here ON PURPOSE, rather than imported from the handler's own renderer: a test
/// that formats the sentence with the very function under test asserts only that the function
/// agrees with itself. Do not "deduplicate" this into an import — the two literals the first
/// test pins are what hold both copies to API Gateway's own wording.
fn unmapped_required_parameters_message(subtype: &str, required: &[&str]) -> String {
    format!(
        "Operation: {subtype} requires enabling passthrough, or defining all of the following \
         parameters in the parameter mapping: [{}]",
        required.join(", ")
    )
}

/// Assert one write was refused as this service's modelled invalid request, with the sentence
/// `expected`.
fn assert_refused(case: &str, expected: &str, status: u16, body: &Value) {
    assert_eq!(
        status, BAD_REQUEST,
        "case {case:?}: expected {BAD_REQUEST} from the write, got {status} {body}"
    );
    let shape = body["__type"].as_str().unwrap_or_default();
    assert!(
        shape.rsplit(['#', '/']).next() == Some(BAD_REQUEST_SHAPE),
        "case {case:?}: expected {BAD_REQUEST_SHAPE}, got {body}"
    );
    assert_eq!(
        body[MESSAGE_MEMBER].as_str().unwrap_or_default(),
        expected,
        "case {case:?}: wrong refusal sentence, got {body}"
    );
}

/// One first-class subtype of rule 1: the subtype a request names, and the parameters the
/// action it names cannot be called without — in the ORDER API Gateway lists them for that
/// subtype, which is per-subtype and neither alphabetical nor the serving model's declaration
/// order.
struct Subtype {
    case: &'static str,
    name: &'static str,
    required: &'static [&'static str],
}

/// The subtypes rule 1 is read over. Each required list is the members the serving service's
/// own input marks REQUIRED, spelled as a mapping spells a parameter (upper camel case):
///
/// * `smithy_query(query="operation", operation="SendMessage")` on the Amazon SQS model —
///   `QueueUrl`, `MessageBody`;
/// * `operation="ReceiveMessage"` — `QueueUrl`;
/// * `operation="DeleteMessage"` — `QueueUrl`, `ReceiptHandle`, which API Gateway lists for
///   this subtype in the opposite order to its `SQS-SendMessage` sibling;
/// * `operation="PurgeQueue"` — `QueueUrl`;
/// * `smithy_query(query="operation", operation="StopExecution")` on the Step Functions model
///   — `executionArn`.
///
/// `SQS-SendMessage` and `StepFunctions-StopExecution` are entries no recording behind this
/// suite exercised as a refusal, so the rule is read beyond the cases that motivated it.
const SUBTYPES: &[Subtype] = &[
    Subtype {
        case: "a send, whose two arguments are listed queue first",
        name: "SQS-SendMessage",
        required: &["QueueUrl", "MessageBody"],
    },
    Subtype {
        case: "a read, whose one argument is the queue",
        name: "SQS-ReceiveMessage",
        required: &["QueueUrl"],
    },
    Subtype {
        case: "a delete, whose two arguments are listed handle first",
        name: "SQS-DeleteMessage",
        required: &["ReceiptHandle", "QueueUrl"],
    },
    Subtype {
        case: "a purge, whose one argument is the queue",
        name: "SQS-PurgeQueue",
        required: &["QueueUrl"],
    },
    Subtype {
        case: "an action of another service entirely",
        name: "StepFunctions-StopExecution",
        required: &["ExecutionArn"],
    },
];

/// The mapping that defines `parameters`, each with a value a mapping may carry.
fn mapping(parameters: &[&str]) -> Value {
    let defined: serde_json::Map<String, Value> = parameters
        .iter()
        .map(|parameter| {
            (
                (*parameter).to_string(),
                json!(format!("$request.body.{parameter}")),
            )
        })
        .collect();
    Value::Object(defined)
}

/// RULE 1: an `AWS_PROXY` integration naming a first-class subtype is written — created and
/// patched alike — only when its `RequestParameters` define every parameter that subtype's
/// action requires; a write that leaves any of them undefined is refused `400`
/// `BadRequestException` naming the subtype and its whole required list, and stores nothing.
#[tokio::test]
async fn a_subtype_integration_is_written_only_with_the_parameters_its_action_requires() {
    // The sentence this suite will hold every refusal below to, pinned against API Gateway's own
    // wording once per SHAPE of the bracketed list — one required parameter and several. These
    // two assertions are about THIS FILE's spelling of the sentence, not about the service: what
    // holds the handler to it is `assert_refused` on every row, which compares the text the
    // service answered with the text these two literals fix.
    assert_eq!(
        unmapped_required_parameters_message("SQS-PurgeQueue", &["QueueUrl"]),
        "Operation: SQS-PurgeQueue requires enabling passthrough, or defining all of the \
         following parameters in the parameter mapping: [QueueUrl]",
        "the refusal must be worded as API Gateway words it"
    );
    assert_eq!(
        unmapped_required_parameters_message("SQS-DeleteMessage", &["ReceiptHandle", "QueueUrl"]),
        "Operation: SQS-DeleteMessage requires enabling passthrough, or defining all of the \
         following parameters in the parameter mapping: [ReceiptHandle, QueueUrl]",
        "a required list of more than one must be comma-separated inside the brackets, in that \
         subtype's own order"
    );

    let estate = Estate::new("127.0.0.1:45923", "us-west-2");
    let role_arn = estate.role("the role every integration below names").await;
    for subtype in SUBTYPES {
        let case = subtype.case;
        let expected = unmapped_required_parameters_message(subtype.name, subtype.required);
        let (api_id, _announced) = estate.create_api(case).await;

        // The empty mapping — the recorded request — and then each ONE required parameter left
        // out: refused with the sentence naming the whole list, and nothing stored.
        let mut incomplete: Vec<(String, Value)> = vec![("no mapping at all".to_string(), json!({}))];
        for dropped in subtype.required {
            let kept: Vec<&str> = subtype
                .required
                .iter()
                .copied()
                .filter(|parameter| parameter != dropped)
                .collect();
            incomplete.push((format!("without {dropped}"), mapping(&kept)));
        }
        // A mapping that carries the required name with NO value behind it supplies the action
        // no argument, so it is no definition of one either.
        let blank: serde_json::Map<String, Value> = subtype
            .required
            .iter()
            .map(|parameter| ((*parameter).to_string(), json!("")))
            .collect();
        incomplete.push(("with every required name left blank".to_string(), Value::Object(blank)));

        for (variant, request_parameters) in &incomplete {
            let case = format!("{case} {variant}");
            let (status, refused) = estate
                .create_integration(&api_id, subtype.name, &role_arn, request_parameters.clone())
                .await;
            assert_refused(&case, &expected, status, &refused);
            assert_eq!(
                estate.integration_count(&case, &api_id).await,
                0,
                "case {case:?}: a refused create must store no integration"
            );
        }

        // Every required parameter defined: created, and reported back by the reader as it was
        // configured.
        let complete = mapping(subtype.required);
        let (status, created) = estate
            .create_integration(&api_id, subtype.name, &role_arn, complete.clone())
            .await;
        assert_eq!(
            status, CREATED,
            "case {case:?}: a complete mapping must be created, got {status} {created}"
        );
        let integration_id = text(case, &created, "integrationId");
        let read = estate.read_integration(case, &api_id, &integration_id).await;
        assert_eq!(
            read["requestParameters"], complete,
            "case {case:?}: GetIntegration must report the mapping that was sent, got {read}"
        );
        assert_eq!(
            read["integrationSubtype"],
            json!(subtype.name),
            "case {case:?}: GetIntegration must report the subtype, got {read}"
        );

        // The PATCH is held to the same rule: a mapping that would leave a required parameter
        // undefined is refused, and the integration keeps the mapping it had.
        for (variant, request_parameters) in &incomplete {
            let case = format!("{case} patched {variant}");
            let (status, refused) = estate
                .patch_mapping(&api_id, &integration_id, request_parameters.clone())
                .await;
            assert_refused(&case, &expected, status, &refused);
            let read = estate.read_integration(&case, &api_id, &integration_id).await;
            assert_eq!(
                read["requestParameters"], complete,
                "case {case:?}: a refused patch must leave the stored mapping alone, got {read}"
            );
        }

        // And a patch that still defines them all, beside a parameter the action does not
        // require, is applied.
        let mut widened = complete.clone();
        widened["Unrequired"] = json!("a static value");
        let (status, patched) = estate
            .patch_mapping(&api_id, &integration_id, widened.clone())
            .await;
        assert_eq!(
            status, SERVED,
            "case {case:?}: a complete patch must be applied, got {status} {patched}"
        );
        let read = estate.read_integration(case, &api_id, &integration_id).await;
        assert_eq!(
            read["requestParameters"], widened,
            "case {case:?}: GetIntegration must report the patched mapping, got {read}"
        );
    }
}

/// What a row of rule 2 asserts once its route was invoked at the address the api published.
enum Outcome {
    /// The queue holds the message the mapping described afterwards, carrying the id the
    /// answer reported.
    Delivers(&'static str),
    /// The answer carries the message the queue was holding.
    AnswersTheWaitingMessage,
    /// The queue holds no message afterwards.
    EmptiesTheQueue,
}

/// One row of rule 2: the client that created the api, the subtype its route performs, what
/// the queue holds first, and what the invoke must then have done.
struct QueueRow {
    case: &'static str,
    /// The `Host` every control-plane call of this row carries.
    control_host: &'static str,
    region: &'static str,
    /// Whether that client resolves nothing but the endpoint it reached, which is the address
    /// the api is then announced on.
    reaches_only_the_emulator: bool,
    stage: &'static str,
    /// Whether the invoke URL appends the stage name to the published endpoint.
    appends_the_stage_name: bool,
    subtype: &'static str,
    /// The permission the action needs, as the serving service publishes it.
    permission: &'static str,
    route_key: &'static str,
    path: &'static str,
    /// The messages already waiting on the queue when the route is invoked.
    waiting: &'static [&'static str],
    /// The mapping, as a function of `(queue url, the handle of one waiting message)`.
    parameters: fn(&str, &str) -> Value,
    /// Whether the invoke carries that handle in a header, which is where one row's mapping
    /// takes it from.
    handle_in_a_header: bool,
    body: &'static str,
    /// Whether the envelope carries the `{Action}Result` element: whether that action's own
    /// output declares any member at all (`SendMessage` and `ReceiveMessage` do, `DeleteMessage`
    /// and `PurgeQueue` answer `Unit`).
    result_element: bool,
    outcome: Outcome,
}

fn queue_rows() -> Vec<QueueRow> {
    vec![
        QueueRow {
            case: "the recorded case: a send driven at the address a loopback client was \
                   published",
            control_host: "127.0.0.1:45923",
            region: "us-west-2",
            reaches_only_the_emulator: true,
            stage: "$default",
            appends_the_stage_name: false,
            subtype: "SQS-SendMessage",
            permission: "sqs:SendMessage",
            route_key: "POST /send",
            path: "/send",
            waiting: &[],
            parameters: |url, _handle| {
                json!({ "QueueUrl": url, "MessageBody": "$request.body.note" })
            },
            handle_in_a_header: false,
            body: r#"{"note":"placed"}"#,
            result_element: true,
            outcome: Outcome::Delivers("placed"),
        },
        QueueRow {
            case: "a read at the same endpoint named by its loopback NAME, on a NAMED stage \
                   whose name the invoke URL appends",
            control_host: "localhost:9297",
            region: "eu-west-1",
            reaches_only_the_emulator: true,
            stage: "reads",
            appends_the_stage_name: true,
            subtype: "SQS-ReceiveMessage",
            permission: "sqs:ReceiveMessage",
            route_key: "POST /recv",
            path: "/recv",
            waiting: &["a marker"],
            parameters: |url, _handle| {
                json!({ "QueueUrl": url, "MaxNumberOfMessages": "10" })
            },
            handle_in_a_header: false,
            body: "",
            result_element: true,
            outcome: Outcome::AnswersTheWaitingMessage,
        },
        QueueRow {
            case: "a delete at a bracketed IPv6 loopback literal, on a NAMED stage, by the \
                   handle the request carries",
            control_host: "[::1]:9297",
            region: "ap-southeast-2",
            reaches_only_the_emulator: true,
            stage: "live",
            appends_the_stage_name: true,
            subtype: "SQS-DeleteMessage",
            permission: "sqs:DeleteMessage",
            route_key: "POST /delete",
            path: "/delete",
            waiting: &["to be deleted"],
            parameters: |url, _handle| {
                json!({
                    "QueueUrl": url,
                    "ReceiptHandle": format!("$request.header.{HANDLE_HEADER}"),
                })
            },
            handle_in_a_header: true,
            body: "",
            result_element: false,
            outcome: Outcome::EmptiesTheQueue,
        },
        QueueRow {
            case: "a purge of a queue holding several, at a loopback literal on another port",
            control_host: "127.0.0.1:8081",
            region: "us-east-2",
            reaches_only_the_emulator: true,
            stage: "$default",
            appends_the_stage_name: false,
            subtype: "SQS-PurgeQueue",
            permission: "sqs:PurgeQueue",
            route_key: "POST /purge",
            path: "/purge",
            waiting: &["one", "two", "three"],
            parameters: |url, _handle| json!({ "QueueUrl": url }),
            handle_in_a_header: false,
            body: "",
            result_element: false,
            outcome: Outcome::EmptiesTheQueue,
        },
        QueueRow {
            case: "a send by a client that resolves AWS's own hosts, published on the api's \
                   own documented host",
            control_host: "apigateway.us-east-1.amazonaws.com",
            region: "us-east-1",
            reaches_only_the_emulator: false,
            stage: "$default",
            appends_the_stage_name: false,
            subtype: "SQS-SendMessage",
            permission: "sqs:SendMessage",
            route_key: "POST /orders",
            path: "/orders",
            waiting: &[],
            parameters: |url, _handle| {
                json!({ "QueueUrl": url, "MessageBody": "$request.body.payload.note" })
            },
            handle_in_a_header: false,
            body: r#"{"payload":{"note":"nested"}}"#,
            result_element: true,
            outcome: Outcome::Delivers("nested"),
        },
        QueueRow {
            case: "a delete by a STATIC handle, by a client that resolves AWS's own hosts, on a \
                   NAMED stage whose name the invoke URL appends",
            control_host: "apigateway.eu-central-1.amazonaws.com",
            region: "eu-central-1",
            reaches_only_the_emulator: false,
            stage: "drops",
            appends_the_stage_name: true,
            subtype: "SQS-DeleteMessage",
            permission: "sqs:DeleteMessage",
            route_key: "POST /drop",
            path: "/drop",
            waiting: &["to be dropped"],
            parameters: |url, handle| json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            handle_in_a_header: false,
            body: "",
            result_element: false,
            outcome: Outcome::EmptiesTheQueue,
        },
    ]
}

/// The authority and base path the api of `api_id` must be ANNOUNCED on for the row's client: a
/// client that resolves nothing but the emulator's own endpoint is announced that endpoint with
/// the api behind the reserved data-plane path, and a client that resolves AWS's own hostnames
/// is announced the api's own documented `execute-api` host.
fn expected_announced_parts(row: &QueueRow, api_id: &str) -> (String, String) {
    if row.reaches_only_the_emulator {
        (
            row.control_host.to_string(),
            format!("/_aws/execute-api/{api_id}"),
        )
    } else {
        (
            format!("{api_id}.execute-api.{}.amazonaws.com", row.region),
            String::new(),
        )
    }
}

/// Assert the row's api is published on an address this emulator serves.
fn assert_announced(row: &QueueRow, api_id: &str, announced: &str) {
    let case = row.case;
    assert!(
        announced.starts_with("https://"),
        "case {case:?}: an HTTP api must publish an https:// endpoint, got {announced:?}"
    );
    let (authority, base_path) = announced_parts(announced);
    let (expected_authority, expected_base_path) = expected_announced_parts(row, api_id);
    assert_eq!(
        authority, expected_authority,
        "case {case:?}: the api must be published on {expected_authority}, got {announced:?}"
    );
    assert_eq!(
        base_path, expected_base_path,
        "case {case:?}: the api must be published under {expected_base_path:?}, got \
         {announced:?}"
    );
}

/// RULE 2: the address an api publishes is one this emulator serves, so a client holding
/// nothing but that value drives the route's first-class queue action end to end — the send
/// lands the mapped body on the queue Amazon SQS holds, the read answers the message waiting
/// there, and the delete and the purge leave it empty — each answered the serving api's own
/// `{Action}Response` envelope.
#[tokio::test]
async fn the_published_api_endpoint_drives_each_queue_subtype_end_to_end() {
    for row in queue_rows() {
        let case = row.case;
        let estate = Estate::new(row.control_host, row.region);
        let role_arn = estate.role(case).await;
        estate.grant(case, row.permission).await;
        let url = estate.queue(case, "subject").await;
        let mut sent: Vec<String> = Vec::new();
        for body in row.waiting {
            sent.push(estate.send(case, &url, body).await);
        }
        let handle = if row.waiting.is_empty() {
            String::new()
        } else {
            estate.one_handle(case, &url).await
        };

        let (api_id, announced) = estate.create_api(case).await;
        let (status, created) = estate
            .create_integration(
                &api_id,
                row.subtype,
                &role_arn,
                (row.parameters)(&url, &handle),
            )
            .await;
        assert_eq!(
            status, CREATED,
            "case {case:?}: CreateIntegration must answer {CREATED}, got {status} {created}"
        );
        let integration_id = text(case, &created, "integrationId");
        estate
            .deploy_route(case, &api_id, &integration_id, row.route_key, row.stage)
            .await;

        // The one value a client of the control plane is given, and the URL it builds out of it.
        // The reader a client comes back to reports the same address, so what it dials is what
        // the api was minted with.
        assert_announced(&row, &api_id, &announced);
        let (status, fetched) = estate
            .control("GET", &format!("/v2/apis/{api_id}"), None)
            .await;
        assert_eq!(
            status, SERVED,
            "case {case:?}: GetApi must answer {SERVED}, got {status} {fetched}"
        );
        assert_eq!(
            fetched["apiEndpoint"],
            json!(announced),
            "case {case:?}: GetApi must report the published endpoint {announced:?}, got \
             {fetched}"
        );
        let path = if row.appends_the_stage_name {
            format!("/{}{}", row.stage, row.path)
        } else {
            row.path.to_string()
        };
        let headers: Vec<(&str, &str)> = if row.handle_in_a_header {
            vec![(HANDLE_HEADER, handle.as_str())]
        } else {
            Vec::new()
        };

        let (status, media_type, answered) = estate
            .invoke(&announced, &path, &headers, row.body)
            .await;
        assert_eq!(
            status, SERVED,
            "case {case:?}: {announced}{path} must answer {SERVED}, got {status} {answered}"
        );
        assert_eq!(
            media_type, XML_MEDIA_TYPE,
            "case {case:?}: expected the media type a query interface answers under, got \
             {media_type:?} with {answered}"
        );

        // The envelope is the ACTION's own: its response element, the result element only where
        // that action's output declares members, and the metadata naming the request.
        let action = row
            .subtype
            .split_once('-')
            .map_or("", |(_service, action)| action);
        assert!(
            answered.contains(&format!("<{action}Response")),
            "case {case:?}: expected the action's own response element, got {answered}"
        );
        assert_eq!(
            answered.contains(&format!("<{action}Result")),
            row.result_element,
            "case {case:?}: expected a result element={}, got {answered}",
            row.result_element
        );
        assert!(
            !element(&answered, "RequestId").is_empty(),
            "case {case:?}: expected the response metadata to name the request, got {answered}"
        );

        match row.outcome {
            Outcome::Delivers(expected_body) => {
                let reported = element(&answered, "MessageId");
                estate.until_waiting(case, &url, 1).await;
                let delivered = estate.drained(case, &url).await;
                assert_eq!(
                    delivered,
                    vec![(expected_body.to_string(), reported.clone())],
                    "case {case:?}: expected the mapped body on the queue under the id the \
                     route reported ({reported}), got {delivered:?}"
                );
            }
            Outcome::AnswersTheWaitingMessage => {
                let expected: Vec<String> =
                    row.waiting.iter().map(|body| (*body).to_string()).collect();
                assert_eq!(
                    elements(&answered, "Body"),
                    expected,
                    "case {case:?}: expected the queue's own message, got {answered}"
                );
                assert_eq!(
                    elements(&answered, "MessageId"),
                    sent,
                    "case {case:?}: expected the id the send minted, got {answered}"
                );
            }
            Outcome::EmptiesTheQueue => {
                assert!(
                    !row.waiting.is_empty(),
                    "case {case:?}: the queue must hold something for emptying it to mean \
                     anything"
                );
                estate.until_waiting(case, &url, 0).await;
            }
        }
    }
}
