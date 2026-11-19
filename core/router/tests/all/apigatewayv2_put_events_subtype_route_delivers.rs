// SPDX-License-Identifier: CC0-1.0

//! An HTTP api route whose integration is the first-class `EventBridge-PutEvents` subtype,
//! driven on an estate where every service the call passes through is mounted together.
//!
//! RULE: **an HTTP api route whose integration is `IntegrationType=AWS_PROXY` naming an
//! `IntegrationSubtype` ANSWERS every matching request with the outcome of that AWS service
//! action, performed as a session of the role its `CredentialsArn` names with the arguments
//! its `RequestParameters` map — so for `EventBridge-PutEvents` a role that holds the action
//! is answered `200` carrying that service's own document (an `Entries` list whose entry
//! carries the `EventId` EventBridge assigned, and `FailedEntryCount` 0) and the event reaches
//! the target of every rule of the named bus whose pattern matches it exactly, while a role
//! that does not hold the action is answered that service's own refusal — `400`
//! `AccessDeniedException` — with nothing published; and writing the grant onto the same role
//! makes the same route publish.**
//!
//! `Integration$IntegrationSubtype` is "supported only for HTTP API AWS_PROXY integrations.
//! Specifies the AWS service action to invoke", and `Integration$RequestParameters` is "a
//! key-value map specifying parameters that are passed to AWS_PROXY integrations" — the
//! arguments of that action. EventBridge's `AccessDeniedException` is its modelled `400`
//! (`smithy_query(query="shape", shape="AccessDeniedException")` on the EventBridge model),
//! and an event pattern's string matching is exact, so a rule differing from a matching one
//! only in the letter case of `detail-type` matches nothing.
//!
//! Five crates have to agree for that to hold — API Gateway v2 resolves the request to the
//! route and performs the subtype, IAM holds the role and its policies, the token service
//! mints the session the call is made as, EventBridge decides the publication on that
//! session's own permissions and routes the event, and Amazon SQS holds the queue the rule
//! targets — so the suite lives here rather than in any one crate. The API Gateway service
//! that owns REST apis is mounted beside them, because the `execute-api` endpoint space is
//! SHARED by the two data planes: an address that resolves at all is one exactly one of them
//! claimed.
//!
//! Nothing asserted here is a value this gateway fills in: the id the route reports is read
//! back off the event waiting on the queue ANOTHER service holds, and the refusal is the text
//! the service serving the action stated.
//!
//! The rows span what the rule generalises over: the endpoint the control-plane client was
//! configured with (an IPv4 loopback literal, a loopback NAME, one of AWS's own service
//! hosts), the Region the api lives in, the stage it is deployed to and the form of the invoke
//! URL built from the published endpoint, the bus, source and detail-type strings (one
//! carrying a space, one upper-case throughout), the mapping expression the `Detail` comes
//! from (the request body whole and one of its members), and whether the integration's role
//! holds the action when the route is first invoked. The recorded case — one api created
//! through the emulator's loopback endpoint, invoked with and without the grant — is the first
//! two rows.
//!
//! What the owning crate's own suites pin and this one does not restate: every mapping
//! expression and every policy outcome over several subtypes
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/http_api_service_action_integration.rs`),
//! and the addressing half of the published endpoint
//! (`.../tests/all/http_api_published_invoke_address.rs`).
//!
//! The second test reads the same rule where ONE api carries two sibling routes: the
//! publication is decided on the role each integration's own `CredentialsArn` names, so the
//! route whose role is refused the action publishes nothing while its sibling, mapping the
//! same event onto the same bus, publishes it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// The subtype every route below names, and the action its role has to hold for the
/// publication to happen.
const PUT_EVENTS_SUBTYPE: &str = "EventBridge-PutEvents";
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The role an integration's `CredentialsArn` names, and the inline policy this suite writes
/// the grant on.
const ROLE_NAME: &str = "apigatewayv2-publishes-events";
const INLINE_POLICY: &str = "may-publish";

/// The service principal EventBridge delivers to a queue as, which the queue's own policy has
/// to grant: "for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge
/// relies on resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The rules every row puts on its bus, the queues they target, and the id each target
/// carries.
const MATCHING_RULE: &str = "matching";
const NEAR_MISS_RULE: &str = "near-miss";
const MATCHING_QUEUE: &str = "matching-queue";
const NEAR_MISS_QUEUE: &str = "near-miss-queue";
const TARGET_ID: &str = "t1";

/// The status a performed action answers, and the status and error shape the service serving
/// it answers for a publication the policies in force do not permit — relayed to the caller
/// as that service stated it.
const PERFORMED: u16 = 200;
const ACTION_REFUSED: u16 = 400;
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The members a relayed refusal carries: the error shape, under the discriminator a JSON
/// error envelope names a shape in, and the text that service stated.
const SHAPE_MEMBER: &str = "__type";
const DETAIL_MEMBER: &str = "Message";

/// How long a delivery is waited for, and how often the queue is looked at while waiting.
/// Generous: the test waits for the CONDITION, and the budget is never the thing asserted.
const DELIVERY_BUDGET: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// What one route's `RequestParameters` map onto the action, and the role the call is made
/// as: the four arguments of a `PutEvents` entry this subtype takes, and the integration's
/// `CredentialsArn`.
struct Mapping<'a> {
    source: &'a str,
    detail_type: &'a str,
    /// The mapping expression the `Detail` comes from, or a static value.
    detail_mapping: &'a str,
    bus: &'a str,
    role_arn: &'a str,
}

/// API Gateway v2, the API Gateway service that owns REST apis, EventBridge, Amazon SQS, IAM
/// and the token service over ONE engine, as `core/router/src/lib.rs` mounts them: the action
/// the integration performs is dispatched back into this same router, so the bus it publishes
/// onto is the bus this suite put a rule on.
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
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = parsed_body(response.body());
        assert_eq!(
            status, PERFORMED,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// One EventBridge call that must be served (`awsJson1_1`, `AWSEvents.<Operation>`).
    async fn events(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "events", "AWSEvents", "1.1", operation, body)
            .await
    }

    /// One Amazon SQS call that must be served (`awsJson1_0`, `AmazonSQS.<Operation>`).
    async fn sqs(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "sqs", "AmazonSQS", "1.0", operation, body)
            .await
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
            status, PERFORMED,
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

    /// One client request to a route of the deployed stage, carrying `body` — the invoke the
    /// integration is performed for. `endpoint` is the address the api ANNOUNCED and `path`
    /// what the client appends to it. No credential: the route's `AuthorizationType` is the
    /// default `NONE`.
    ///
    /// Handed straight to the router IN PROCESS, so no socket is opened and nothing leaves
    /// this test: the authority is only the address the request carries.
    async fn invoke(&self, endpoint: &str, path: &str, body: &str) -> (u16, Value) {
        let (authority, base_path) = announced_parts(endpoint);
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{authority}{base_path}{path}"))
            .header("host", authority.as_str())
            .header("content-type", "application/json")
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, parsed_body(response.body()))
    }

    /// The role an integration's `CredentialsArn` names, trusted by API Gateway — written with
    /// IAM's own operations, so the role a session is minted of is the record `iam:CreateRole`
    /// wrote. Answers its ARN.
    async fn role(&self, case: &str, name: &str) -> String {
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
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        format!(
            "arn:aws:iam::{}:role/{name}",
            engine::emulator_account_id()
        )
    }

    /// Write the grant of `action` onto the role `name` as its inline identity policy.
    async fn grant(&self, case: &str, name: &str, action: &str) {
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": action, "Resource": "*" }],
        })
        .to_string();
        self.iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", INLINE_POLICY.to_string()),
                ("PolicyDocument", document),
            ],
        )
        .await;
    }

    /// A queue carrying the policy EventBridge's delivery needs; answers `(url, arn)`.
    async fn queue(&self, case: &str, name: &str) -> (String, String) {
        let created = self
            .sqs(case, "CreateQueue", &json!({ "QueueName": name }))
            .await;
        let url = text(case, &created, "QueueUrl");
        let attributes = self
            .sqs(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
        let arn = text(case, &attributes["Attributes"], "QueueArn");
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "AllowEventBridge",
                "Effect": "Allow",
                "Principal": { "Service": EVENTS_PRINCIPAL },
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
    /// queue, and a NEAR-MISS rule differing from it only in the LETTER CASE of the detail
    /// type, with a queue of its own. Answers `(matching queue url, near-miss queue url)`.
    async fn bus_with_rules(
        &self,
        case: &str,
        bus: &str,
        source: &str,
        detail_type: &str,
    ) -> (String, String) {
        self.events(case, "CreateEventBus", &json!({ "Name": bus }))
            .await;
        let (matching_url, matching_arn) = self.queue(case, MATCHING_QUEUE).await;
        let (near_miss_url, near_miss_arn) = self.queue(case, NEAR_MISS_QUEUE).await;
        let folded = case_folded(detail_type);
        assert_ne!(
            folded, detail_type,
            "case {case:?}: the near-miss rule must differ from the matching one"
        );
        let rules: [(&str, String, &str); 2] = [
            (
                MATCHING_RULE,
                json!({ "source": [source], "detail-type": [detail_type] }).to_string(),
                matching_arn.as_str(),
            ),
            (
                NEAR_MISS_RULE,
                json!({ "source": [source], "detail-type": [folded] }).to_string(),
                near_miss_arn.as_str(),
            ),
        ];
        for (name, pattern, target) in rules {
            self.events(
                case,
                "PutRule",
                &json!({
                    "Name": name,
                    "EventBusName": bus,
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
                        "EventBusName": bus,
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

    /// A bare HTTP api; answers `(ApiId, the address its ApiEndpoint announces)` — the value a
    /// client builds its invoke URL from, which [`invoke`](Self::invoke) takes as it stands.
    async fn create_api(&self, case: &str) -> (String, String) {
        let (status, api) = self
            .control(
                "POST",
                "/v2/apis",
                Some(json!({ "name": "subtype-api", "protocolType": "HTTP" })),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateApi must answer 201, got {status} {api}"
        );
        (text(case, &api, "apiId"), text(case, &api, "apiEndpoint"))
    }

    /// One route of that api over a first-class `EventBridge-PutEvents` integration performed
    /// under `role_arn`, with the mapping `Source`/`DetailType`/`Detail`/`EventBusName`.
    async fn add_route(&self, case: &str, api_id: &str, route_key: &str, mapped: &Mapping<'_>) {
        let (status, created) = self
            .control(
                "POST",
                &format!("/v2/apis/{api_id}/integrations"),
                Some(json!({
                    "integrationType": "AWS_PROXY",
                    "integrationSubtype": PUT_EVENTS_SUBTYPE,
                    "payloadFormatVersion": "1.0",
                    "credentialsArn": mapped.role_arn,
                    "requestParameters": {
                        "Source": mapped.source,
                        "DetailType": mapped.detail_type,
                        "Detail": mapped.detail_mapping,
                        "EventBusName": mapped.bus,
                    },
                })),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateIntegration must answer 201, got {status} {created}"
        );
        let integration_id = text(case, &created, "integrationId");

        // The reader a client comes back to holds the first-class integration as it was
        // configured, which is what the route's dispatch then performs.
        let (status, read) = self
            .control(
                "GET",
                &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
                None,
            )
            .await;
        assert_eq!(
            status, PERFORMED,
            "case {case:?}: GetIntegration must answer 200, got {status} {read}"
        );
        for (member, expected) in [
            ("integrationType", json!("AWS_PROXY")),
            ("integrationSubtype", json!(PUT_EVENTS_SUBTYPE)),
            ("credentialsArn", json!(mapped.role_arn)),
        ] {
            assert_eq!(
                read[member], expected,
                "case {case:?}: GetIntegration must report {member} as {expected}, got {read}"
            );
        }
        assert_eq!(
            read["requestParameters"]["Detail"],
            json!(mapped.detail_mapping),
            "case {case:?}: GetIntegration must report the mapped Detail, got {read}"
        );

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
            "case {case:?}: CreateRoute must answer 201, got {status} {route}"
        );
    }

    /// That api's stage, with auto-deploy on, which is what serves its routes.
    async fn deploy(&self, case: &str, api_id: &str, stage_name: &str) {
        let (status, stage) = self
            .control(
                "POST",
                &format!("/v2/apis/{api_id}/stages"),
                Some(json!({ "stageName": stage_name, "autoDeploy": true })),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateStage must answer 201, got {status} {stage}"
        );
    }

    /// Every message waiting on the queue at `url`, as its parsed body, taken off it.
    async fn drained(&self, case: &str, url: &str) -> Vec<Value> {
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
                let body = message["Body"].as_str().unwrap_or_default();
                serde_json::from_str(body).unwrap_or_else(|_| json!(body))
            })
            .collect()
    }

    /// Look at the queue at `url` until it holds something, or until the delivery budget has
    /// passed; answers everything seen. Waits for the CONDITION rather than for a span.
    async fn delivered(&self, case: &str, url: &str) -> Vec<Value> {
        let deadline = Instant::now() + DELIVERY_BUDGET;
        loop {
            let seen = self.drained(case, url).await;
            if !seen.is_empty() || Instant::now() >= deadline {
                return seen;
            }
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
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

/// `value` with the letter case of every character swapped — the near-miss spelling, which an
/// event pattern's exact string matching must not match.
fn case_folded(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_uppercase() {
                character.to_lowercase().next().unwrap_or(character)
            } else {
                character.to_uppercase().next().unwrap_or(character)
            }
        })
        .collect()
}

/// The announced endpoint split into the authority a client dials and the path it carries
/// ahead of the stage — empty for an endpoint that is a bare host.
fn announced_parts(announced: &str) -> (String, String) {
    let without_scheme = announced
        .split_once("://")
        .map_or(announced, |(_scheme, rest)| rest);
    match without_scheme.find('/') {
        Some(at) => (
            without_scheme[..at].to_string(),
            without_scheme[at..].to_string(),
        ),
        None => (without_scheme.to_string(), String::new()),
    }
}

/// One row: the client that created the api, the estate it publishes onto, what the mapping
/// publishes, and whether the integration's role holds the action to begin with.
struct Row {
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
    bus: &'static str,
    source: &'static str,
    detail_type: &'static str,
    route_key: &'static str,
    path: &'static str,
    body: &'static str,
    /// The mapping expression the integration maps onto the action's `Detail`.
    detail_mapping: &'static str,
    /// The document that mapping resolves to for this row's body.
    detail: fn() -> Value,
    /// Whether the role may publish when the route is FIRST invoked.
    granted: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: an api created through the emulator's loopback \
                   endpoint, whose role may publish",
            control_host: "127.0.0.1:45923",
            region: "us-west-2",
            reaches_only_the_emulator: true,
            stage: "$default",
            appends_the_stage_name: false,
            bus: "orders-bus",
            source: "app.orders",
            detail_type: "OrderPlaced",
            route_key: "POST /orders",
            path: "/orders",
            body: r#"{"id":"o-1"}"#,
            detail_mapping: "$request.body",
            detail: || json!({ "id": "o-1" }),
            granted: true,
        },
        Row {
            case: "the recorded denial: the same estate, a role that may not publish until \
                   the grant is written",
            control_host: "127.0.0.1:45923",
            region: "us-west-2",
            reaches_only_the_emulator: true,
            stage: "$default",
            appends_the_stage_name: false,
            bus: "orders-bus",
            source: "app.orders",
            detail_type: "OrderPlaced",
            route_key: "POST /orders",
            path: "/orders",
            body: r#"{"id":"o-1"}"#,
            detail_mapping: "$request.body",
            detail: || json!({ "id": "o-1" }),
            granted: false,
        },
        Row {
            case: "a client that resolves AWS's own hosts, in another Region, with the stage \
                   name appended and a body MEMBER as the detail",
            control_host: "apigateway.eu-west-1.amazonaws.com",
            region: "eu-west-1",
            reaches_only_the_emulator: false,
            stage: "$default",
            appends_the_stage_name: true,
            bus: "billing-bus",
            source: "com.acme.billing",
            detail_type: "Invoice Raised",
            route_key: "POST /invoices",
            path: "/invoices",
            body: r#"{"payload":{"total":12}}"#,
            detail_mapping: "$request.body.payload",
            detail: || json!({ "total": 12 }),
            granted: true,
        },
        Row {
            case: "the loopback endpoint named by its NAME, a NAMED stage, an upper-case \
                   detail type and a role that may not publish yet",
            control_host: "localhost:9297",
            region: "ap-southeast-2",
            reaches_only_the_emulator: true,
            stage: "live",
            appends_the_stage_name: true,
            bus: "shipments-bus",
            source: "app.shipments",
            detail_type: "SHIPMENT_DISPATCHED",
            route_key: "POST /shipments",
            path: "/shipments",
            body: r#"{"id":"s-9"}"#,
            detail_mapping: "$request.body",
            detail: || json!({ "id": "s-9" }),
            granted: false,
        },
    ]
}

/// The authority and base path the api of `api_id` must be ANNOUNCED on for the row's client: a
/// client that resolves nothing but the emulator's own endpoint is announced that endpoint with
/// the api behind the reserved data-plane path, and a client that resolves AWS's own hostnames
/// is announced the api's own documented `execute-api` host.
fn expected_announced_parts(row: &Row, api_id: &str) -> (String, String) {
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
fn assert_announced(row: &Row, api_id: &str, announced: &str) {
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

/// The path within the published endpoint a client of this row builds.
fn invoke_path(row: &Row) -> String {
    if row.appends_the_stage_name {
        format!("/{}{}", row.stage, row.path)
    } else {
        row.path.to_string()
    }
}

/// The event one row expects on the queue the matching rule targets: what the integration's
/// mapping describes, as EventBridge's own envelope states it.
struct Published<'a> {
    case: &'a str,
    source: &'a str,
    detail_type: &'a str,
    detail: Value,
}

/// Assert `answered` is EventBridge's own answer to one publication, and answer the `EventId`
/// it assigned — the value a client of such a route reads the id of what it just did from.
fn published_event_id(case: &str, status: u16, answered: &Value) -> String {
    assert_eq!(
        status, PERFORMED,
        "case {case:?}: a performed publication must answer {PERFORMED}, got {status} \
         {answered}"
    );
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected no failed entry in {answered}"
    );
    let entries = answered["Entries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        entries.len(),
        1,
        "case {case:?}: expected one entry in {answered}"
    );
    let event_id = entries[0]["EventId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an EventId in {answered}"))
        .to_string();
    assert!(
        !event_id.is_empty(),
        "case {case:?}: expected a non-empty EventId in {answered}"
    );
    event_id
}

/// Assert the queue the matching rule targets holds exactly the event the mapping described,
/// carrying the id the route reported.
fn assert_delivered(expected: &Published<'_>, delivered: &[Value], event_id: &str) {
    let case = expected.case;
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: expected one event on the matching rule's queue, got {delivered:?}"
    );
    let event = &delivered[0];
    assert_eq!(
        event["id"].as_str(),
        Some(event_id),
        "case {case:?}: the delivered event must carry the id the route reported \
         ({event_id}), got {event}"
    );
    assert_eq!(
        event["source"].as_str(),
        Some(expected.source),
        "case {case:?}: expected the mapped source, got {event}"
    );
    assert_eq!(
        event["detail-type"].as_str(),
        Some(expected.detail_type),
        "case {case:?}: expected the mapped detail type, got {event}"
    );
    assert_eq!(
        event["detail"], expected.detail,
        "case {case:?}: expected the detail the mapping describes, got {event}"
    );
}

/// Assert `answered` is the refusal the service serving the action stated for a role that does
/// not hold it: its own status, the error shape it named and text of its own.
fn assert_refused(case: &str, status: u16, answered: &Value) {
    assert_eq!(
        status, ACTION_REFUSED,
        "case {case:?}: a refused publication must answer {ACTION_REFUSED}, got {status} \
         {answered}"
    );
    let shape = answered[SHAPE_MEMBER].as_str().unwrap_or_default();
    assert!(
        shape.ends_with(ACCESS_DENIED),
        "case {case:?}: expected the serving service's own {ACCESS_DENIED}, got {answered}"
    );
    let detail = answered[DETAIL_MEMBER].as_str().unwrap_or_default();
    assert!(
        detail.contains(PUT_EVENTS_ACTION),
        "case {case:?}: expected a refusal naming {PUT_EVENTS_ACTION:?}, got {answered}"
    );
}

/// RULE: a route whose integration is `AWS_PROXY` naming `EventBridge-PutEvents` performs that
/// publication under the role its `CredentialsArn` names — answered `200` with the `EventId`
/// EventBridge assigned and `FailedEntryCount` 0, with the event on the target of the rule whose
/// pattern matches it exactly and nothing on the target of the rule differing only in letter
/// case — and a role that does not hold `events:PutEvents` is answered that service's own `400`
/// `AccessDeniedException` with nothing published, until the grant is written.
#[tokio::test]
async fn a_put_events_subtype_route_publishes_under_its_integration_role() {
    for row in rows() {
        let case = row.case;
        let estate = Estate::new(row.control_host, row.region);
        let role_arn = estate.role(case, ROLE_NAME).await;
        if row.granted {
            estate.grant(case, ROLE_NAME, PUT_EVENTS_ACTION).await;
        }
        let (matching, near_miss) = estate
            .bus_with_rules(case, row.bus, row.source, row.detail_type)
            .await;
        let (api_id, announced) = estate.create_api(case).await;
        estate
            .add_route(
                case,
                &api_id,
                row.route_key,
                &Mapping {
                    source: row.source,
                    detail_type: row.detail_type,
                    detail_mapping: row.detail_mapping,
                    bus: row.bus,
                    role_arn: &role_arn,
                },
            )
            .await;
        estate.deploy(case, &api_id, row.stage).await;
        assert_announced(&row, &api_id, &announced);
        let path = invoke_path(&row);
        let expected = Published {
            case,
            source: row.source,
            detail_type: row.detail_type,
            detail: (row.detail)(),
        };

        // The first invoke: performed for a role that holds the action, refused by the service
        // serving it for one that does not.
        let (status, answered) = estate.invoke(&announced, &path, row.body).await;
        let first_id = if row.granted {
            let event_id = published_event_id(case, status, &answered);
            let delivered = estate.delivered(case, &matching).await;
            assert_delivered(&expected, &delivered, &event_id);
            Some(event_id)
        } else {
            assert_refused(case, status, &answered);
            let delivered = estate.drained(case, &matching).await;
            assert!(
                delivered.is_empty(),
                "case {case:?}: a refused publication must deliver nothing, got {delivered:?}"
            );
            // The one permission the role was missing, written onto the same role.
            estate.grant(case, ROLE_NAME, PUT_EVENTS_ACTION).await;
            None
        };

        // The SAME route again: a role that holds the action publishes, and each publication
        // is its own event — the id a later call reports is not the one an earlier call did.
        let (status, answered) = estate.invoke(&announced, &path, row.body).await;
        let event_id = published_event_id(case, status, &answered);
        if let Some(first_id) = &first_id {
            assert_ne!(
                &event_id, first_id,
                "case {case:?}: each publication must carry its own event id, got \
                 {event_id} twice"
            );
        }
        let delivered = estate.delivered(case, &matching).await;
        assert_delivered(&expected, &delivered, &event_id);

        // The rule differing from the matching one only in the letter case of the detail type
        // receives nothing, whichever way the route was reached.
        let missed = estate.drained(case, &near_miss).await;
        assert!(
            missed.is_empty(),
            "case {case:?}: expected nothing on the case-differing rule's queue, got \
             {missed:?}"
        );
    }
}

/// The two roles the sibling routes below are performed as, and the routes themselves.
const UNDER_PRIVILEGED_ROLE: &str = "apigatewayv2-may-not-publish";
const REFUSED_ROUTE_KEY: &str = "POST /refused";
const REFUSED_ROUTE_PATH: &str = "/refused";
const PERMITTED_ROUTE_KEY: &str = "POST /permitted";
const PERMITTED_ROUTE_PATH: &str = "/permitted";

/// RULE: the publication is decided on the policies in force on the role EACH integration's
/// own `CredentialsArn` names, so one api can carry both a route whose role is refused the
/// action — with nothing published for it — and a sibling route, mapping the very same event
/// onto the very same bus, whose role publishes it.
#[tokio::test]
async fn each_route_is_performed_as_its_own_integration_role() {
    let case = "two sibling routes of one api, one role refused the action and one granted it";
    let source = "app.orders";
    let detail_type = "OrderPlaced";
    let bus = "orders-bus";
    let body = r#"{"id":"o-2"}"#;
    let estate = Estate::new("127.0.0.1:45923", "us-west-2");
    let refused_role = estate.role(case, UNDER_PRIVILEGED_ROLE).await;
    let permitted_role = estate.role(case, ROLE_NAME).await;
    estate.grant(case, ROLE_NAME, PUT_EVENTS_ACTION).await;
    let (matching, near_miss) = estate
        .bus_with_rules(case, bus, source, detail_type)
        .await;
    let (api_id, announced) = estate.create_api(case).await;
    for (route_key, role_arn) in [
        (REFUSED_ROUTE_KEY, refused_role.as_str()),
        (PERMITTED_ROUTE_KEY, permitted_role.as_str()),
    ] {
        estate
            .add_route(
                case,
                &api_id,
                route_key,
                &Mapping {
                    source,
                    detail_type,
                    detail_mapping: "$request.body",
                    bus,
                    role_arn,
                },
            )
            .await;
    }
    estate.deploy(case, &api_id, "$default").await;

    // The route whose role does not hold the action: that service's own refusal, and nothing
    // published.
    let (status, answered) = estate
        .invoke(&announced, REFUSED_ROUTE_PATH, body)
        .await;
    assert_refused(case, status, &answered);
    let delivered = estate.drained(case, &matching).await;
    assert!(
        delivered.is_empty(),
        "case {case:?}: a refused publication must deliver nothing, got {delivered:?}"
    );

    // The sibling route, mapping the same event onto the same bus as a role that does hold it:
    // performed, and delivered to the queue the matching rule targets.
    let (status, answered) = estate
        .invoke(&announced, PERMITTED_ROUTE_PATH, body)
        .await;
    let event_id = published_event_id(case, status, &answered);
    let delivered = estate.delivered(case, &matching).await;
    assert_delivered(
        &Published {
            case,
            source,
            detail_type,
            detail: json!({ "id": "o-2" }),
        },
        &delivered,
        &event_id,
    );
    let missed = estate.drained(case, &near_miss).await;
    assert!(
        missed.is_empty(),
        "case {case:?}: expected nothing on the case-differing rule's queue, got {missed:?}"
    );
}
