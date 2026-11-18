// SPDX-License-Identifier: CC0-1.0

//! What a message published to the AWS IoT DATA PLANE does, and who may publish it at all,
//! observed where a client observes it: as messages the queue's OWN `ReceiveMessage` serves and
//! as the shadow document the plane's own `GetThingShadow` answers.
//!
//! RULE, in two halves, one table each.
//!
//! 1. A message the broker accepts is evaluated by the rules engine, and every ENABLED topic
//!    rule whose `FROM`-clause topic filter covers the published topic has each of its actions
//!    performed for the message its statement selects — for an `sqs` action, one
//!    `sqs:SendMessage` of that selected message to the queue the action's `queueUrl`
//!    addresses, in the form the action asks for (`SqsAction$useBase64`), under the action's own
//!    role — and NOTHING is performed for an action that cannot be performed faithfully: one
//!    whose role does not admit the IoT service principal or carries no grant for that send on
//!    that queue, one whose `queueUrl` carries a substitution template this emulator's rules
//!    engine does not render, and one whose queue Amazon SQS itself refuses the send to. None of
//!    it changes the data plane's answer to the publisher, which is why every assertion of that
//!    table is made on the QUEUE.
//! 2. A call into that plane is decided against the identity-based policies in force on the
//!    principal the request's credential speaks for and the resource the request ADDRESSES
//!    (`topic/<topic>` for a message, `thing/<thingName>` for a shadow), so an allowed publish
//!    is taken and then forwarded by the rule that selects it, an allowed `UpdateThingShadow`
//!    answers the MERGED state document and is readable back through `GetThingShadow`, and a
//!    call the policies do not allow is refused with this plane's own refusal envelope and
//!    performs nothing — no message published, no rule run, no shadow moved.
//!
//! Four crates have to agree for that to hold — the IoT data plane takes the publish and the
//! authorization decision, the IoT control plane holds the rule and runs the engine, IAM holds
//! the caller's and the action's policies, and Amazon SQS takes (or refuses) the send — so the
//! suite lives here: a crate-local suite can only ever see one side of it.
//!
//! The first table's rows differ along what its rule generalises over: the form of the topic
//! filter (exact, the single-level `+`, the multi-level `#`, one that does not cover the topic),
//! the rule's enablement, what the statement SELECTS (the whole message, a projection, a
//! condition that holds and one that does not), the form the payload arrives in (a document and
//! text that is not one) and the QoS it is published at (both values `Qos` ranges over), a
//! publish the broker refuses outright, the form the action sends in (`useBase64` true, false
//! and left out), how many actions one rule carries, what the action's ROLE says about the send
//! (the grant, a wildcard grant, another action, another queue, no inline policy at all, a trust
//! policy naming another service principal), and what its `queueUrl` addresses (the queue, a
//! template, a queue the account does not hold).
//!
//! The second table's rows differ along WHICH operation of the plane the caller calls (a message
//! publish, a shadow write, a shadow read), what its policies allow while it calls (every action
//! of the service, exactly the action called scoped to the resource it addresses, another action
//! of the service, nothing of the plane at all), and how the topic the publish addresses is
//! shaped (one level and a multi-level name), with each row standing on what the earlier rows
//! wrote — so a refused write is visible as the state it did not change.
//!
//! Every row of both tables asserts BOTH what the call answered and what the target holds
//! afterwards, so a call that answered without doing the work, or did the work without
//! answering, fails here. A row that expects NOTHING forwarded is judged against a queue that
//! has since taken a control message whose own forward is known to work, so it cannot pass
//! against a queue nothing ever reached.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    rules_engine::SQS_SEND_MESSAGE_ACTION, SERVICE_PRINCIPAL,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The queue every action of these rows forwards to, and a second queue that exists only so a
/// role can be granted the send on ANOTHER queue's ARN.
const QUEUE: &str = "iot-rule-forwarded";
const ANOTHER_QUEUE: &str = "iot-rule-elsewhere";

/// The rule each row stores. A rule name carries letters, digits and underscores alone
/// (`RuleName`, `pattern ^[a-zA-Z0-9_]+$`).
const RULE: &str = "to_the_queue";

/// The CONTROL rule every row also stores, its topic, and the message published to it: a
/// forward that is known to be performable, which is what makes a row expecting NOTHING a real
/// assertion. No row's topic filter covers this topic.
const CONTROL_RULE: &str = "control_forward";
const CONTROL_TOPIC: &str = "control/forward";
const CONTROL_MESSAGE: &str = r#"{"control":"forwarded"}"#;

/// The SQL version a rule is stored with (`TopicRulePayload$awsIotSqlVersion`).
const SQL_VERSION: &str = "2016-03-23";

/// The message the rows publish unless one names its own: a document whose members the
/// projection and condition rows select over.
const MESSAGE: &str = r#"{"marker":"m-1","temp":42,"state":{"reported":{"ok":true}}}"#;

/// Another Amazon SQS action, for the role that is granted something other than the send.
const ANOTHER_SQS_ACTION: &str = "sqs:GetQueueUrl";

/// The service principal a role has to trust for Lambda to assume it — here, the principal
/// that is NOT the one the rules engine acts as.
const ANOTHER_PRINCIPAL: &str = "lambda.amazonaws.com";

/// How long one row's observation of the forwarding is given, and how long between looks.
/// Generous beside the work a row does — the forward is made before the publish is answered —
/// without making a genuine regression cost minutes a row: what each wait is ON is a condition
/// ([`forwarded_bodies`]), never a span.
const DEADLINE: Duration = Duration::from_secs(30);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The long poll one drain of the queue makes, in seconds: NONE, because the rules engine's
/// send completes before the publish is answered, so a message that was forwarded is already
/// in the queue by the time the first drain asks. The waiting a slow host needs is the drain
/// LOOP's ([`DEADLINE`]).
const WAIT_TIME_SECONDS: u64 = 0;

/// How many messages one drain reads at a time.
const RECEIVE_BATCH: u64 = 10;

/// The emulator these rows run against: the IoT control plane, the IoT data plane, Amazon SQS
/// and IAM on ONE engine — the shape `core/router/src/lib.rs` mounts them in.
struct Emulator {
    router: MultiServiceRouter,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope is what routes a request here.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iot_awsiotservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iotdataplane_iotmoonrakerservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    Emulator { router }
}

/// The access key id every call of the first table presents: one neither IAM nor STS holds a
/// record for, which is the account's own published credential — the caller the data plane's
/// authorization gate does not bound, so what the first table decides is the RULE's and not an
/// identity's.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request, and whose credential is `access_key_id`, which is the identity the services
/// decide the request as. The signature itself is never checked in this harness.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential={access_key_id}/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One HTTP answer: the status, the error shape it names, and the body, read as JSON where
/// there is one.
struct Answer {
    status: u16,
    /// The `x-amzn-errortype` header, which is where a restJson1 client resolves the error
    /// shape of a refusal from.
    error_type: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

impl Emulator {
    /// One call as the account's own credential.
    async fn send(
        &self,
        method: &str,
        uri: &str,
        signing_name: &str,
        content_type: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answer {
        self.send_as(
            ACCOUNT_KEY_ID,
            method,
            uri,
            signing_name,
            content_type,
            headers,
            body,
        )
        .await
    }

    /// One call presenting `access_key_id`, which is the identity the service decides it as.
    #[allow(clippy::too_many_arguments)]
    async fn send_as(
        &self,
        access_key_id: &str,
        method: &str,
        uri: &str,
        signing_name: &str,
        content_type: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answer {
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("authorization", authorization(access_key_id, signing_name))
            .header("content-type", content_type);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = self
            .router
            .dispatch(
                request
                    .body(bytes::Bytes::from(body))
                    .expect("the test request is well formed"),
            )
            .await;
        Answer {
            status: response.status().as_u16(),
            error_type: response
                .headers()
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            body: response.into_body().to_vec(),
        }
    }

    /// `CreateTopicRule`: the restJson1 `POST /rules/{ruleName}` of the IoT control plane,
    /// whose body IS the `topicRulePayload` (the member is the operation's HTTP payload).
    async fn create_topic_rule(&self, case: &str, rule_name: &str, payload: &Value) {
        let answer = self
            .send(
                "POST",
                &format!("/rules/{rule_name}"),
                "iot",
                "application/json",
                &[],
                serde_json::to_vec(payload).expect("the payload serializes"),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: CreateTopicRule({rule_name}) answered {} {}",
            answer.status,
            answer.text()
        );
    }

    /// `Publish` of the IoT data plane: `POST /topics/{topic}`, the call the rules engine runs
    /// on, answered with whatever the broker said — so a row can assert both an accepted
    /// publish and one the broker refuses.
    async fn publish(&self, topic: &str, qos: u8, payload: &[u8]) -> Answer {
        self.publish_as(ACCOUNT_KEY_ID, topic, qos, payload).await
    }

    /// The same `Publish`, presenting `access_key_id` — the identity the data plane's
    /// authorization gate decides the call as.
    ///
    /// A `{topic}` path label carries no `/` of its own, so a topic name's separators travel
    /// percent-encoded, which is how an SDK sends a topic name holding slashes. The encoding is
    /// this test crate's one encoder ([`crate::query_wire::form_encode`]), whose unreserved set
    /// is RFC 3986's — exactly what one URI component may carry literally.
    async fn publish_as(
        &self,
        access_key_id: &str,
        topic: &str,
        qos: u8,
        payload: &[u8],
    ) -> Answer {
        self.send_as(
            access_key_id,
            "POST",
            &format!("/topics/{}?qos={qos}", form_encode(topic)),
            "iotdata",
            "application/octet-stream",
            &[],
            payload.to_vec(),
        )
        .await
    }

    /// One Amazon SQS call (`awsJson1_0`) that must be served, answered with its document.
    async fn sqs(&self, case: &str, operation: &str, body: &Value) -> Value {
        let answer = self
            .send(
                "POST",
                "/",
                "sqs",
                "application/x-amz-json-1.0",
                &[("x-amz-target", &format!("AmazonSQS.{operation}"))],
                serde_json::to_vec(body).expect("the request serializes"),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: SQS {operation} answered {} {}",
            answer.status,
            answer.text()
        );
        answer.json()
    }

    /// One IAM call over the `awsQuery` protocol that must be served, answered with its XML
    /// body.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, &str)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let answer = self
            .send(
                "POST",
                "/",
                "iam",
                "application/x-www-form-urlencoded",
                &[],
                body.into_bytes(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: IAM {action} answered {} {}",
            answer.status,
            answer.text()
        );
        answer.text()
    }
}

/// A queue of `name`, answered with the URL a client addresses it by.
async fn given_queue(emulator: &Emulator, case: &str, name: &str) -> String {
    let created = emulator
        .sqs(case, "CreateQueue", &json!({ "QueueName": name }))
        .await;
    created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue({name}) reported no QueueUrl"))
        .to_string()
}

/// The ARN of the queue `queue_url` addresses, as its own `GetQueueAttributes` reports it —
/// the resource a statement about that send names.
async fn given_queue_arn(emulator: &Emulator, case: &str, queue_url: &str) -> String {
    let attributes = emulator
        .sqs(
            case,
            "GetQueueAttributes",
            &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
        )
        .await;
    attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: GetQueueAttributes({queue_url}) reported no QueueArn")
        })
        .to_string()
}

/// A trust policy admitting one service principal, written as a client sends it.
fn trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// An identity policy allowing one action on one resource.
fn identity_policy(action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string()
}

/// What an action's ROLE says about the send: which service principal its trust policy admits,
/// and what its inline policy allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleKind {
    /// Trusts the rules engine and allows the send on the queue the action addresses.
    Granted,
    /// Trusts the rules engine and allows every Amazon SQS action on every resource.
    Wildcard,
    /// Trusts the rules engine and allows another Amazon SQS action on that queue.
    AnotherAction,
    /// Trusts the rules engine and allows the send on ANOTHER queue.
    AnotherQueue,
    /// Trusts the rules engine and carries no inline policy at all.
    NoPolicy,
    /// Allows the send, but trusts a service principal that is not the rules engine's.
    AnotherPrincipal,
}

impl RoleKind {
    /// The role's name, one per kind so a row can carry two actions under different roles.
    fn role_name(self) -> &'static str {
        match self {
            Self::Granted => "iot-rule-granted",
            Self::Wildcard => "iot-rule-wildcard",
            Self::AnotherAction => "iot-rule-another-action",
            Self::AnotherQueue => "iot-rule-another-queue",
            Self::NoPolicy => "iot-rule-no-policy",
            Self::AnotherPrincipal => "iot-rule-another-principal",
        }
    }

    /// The service principal its trust policy admits.
    fn principal(self) -> &'static str {
        match self {
            Self::AnotherPrincipal => ANOTHER_PRINCIPAL,
            _ => SERVICE_PRINCIPAL,
        }
    }

    /// The `(action, resource)` its inline policy allows, or `None` for a role carrying no
    /// inline policy at all.
    fn allowed(self, queue_arn: &str, another_arn: &str) -> Option<(String, String)> {
        match self {
            Self::Granted | Self::AnotherPrincipal => {
                Some((SQS_SEND_MESSAGE_ACTION.to_string(), queue_arn.to_string()))
            }
            Self::Wildcard => Some(("sqs:*".to_string(), "*".to_string())),
            Self::AnotherAction => {
                Some((ANOTHER_SQS_ACTION.to_string(), queue_arn.to_string()))
            }
            Self::AnotherQueue => Some((
                SQS_SEND_MESSAGE_ACTION.to_string(),
                another_arn.to_string(),
            )),
            Self::NoPolicy => None,
        }
    }
}

/// The role `kind` names, created as a client creates it; answered with the ARN IAM persisted.
async fn given_role(
    emulator: &Emulator,
    case: &str,
    kind: RoleKind,
    queue_arn: &str,
    another_arn: &str,
) -> String {
    let name = kind.role_name();
    let trust = trust_policy(kind.principal());
    let created = emulator
        .iam(
            case,
            "CreateRole",
            &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
        )
        .await;
    let arn = element(&created, "Arn");
    if let Some((action, resource)) = kind.allowed(queue_arn, another_arn) {
        let document = identity_policy(&action, &resource);
        emulator
            .iam(
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", name),
                    ("PolicyName", "rule-action"),
                    ("PolicyDocument", &document),
                ],
            )
            .await;
    }
    arn
}

/// What an action's `queueUrl` addresses.
#[derive(Debug, Clone, Copy)]
enum QueueKind {
    /// The queue the row created.
    TheQueue,
    /// A URL carrying a substitution template, whose value AWS IoT renders from the message
    /// and this emulator's rules engine does not compute.
    Templated,
    /// A well-formed URL for a queue the account does not hold.
    Absent,
}

impl QueueKind {
    /// The `queueUrl` the action carries, built from the row's own queue URL.
    fn queue_url(self, queue_url: &str) -> String {
        match self {
            Self::TheQueue => queue_url.to_string(),
            Self::Templated => format!("{queue_url}-${{topic()}}"),
            Self::Absent => format!("{queue_url}-absent"),
        }
    }
}

/// One `sqs` action of a row's rule.
#[derive(Debug, Clone, Copy)]
struct ActionSpec {
    role: RoleKind,
    queue: QueueKind,
    /// `SqsAction$useBase64`, or `None` for an action leaving the member out.
    use_base64: Option<bool>,
}

impl ActionSpec {
    /// The action every row carries unless it varies one of these: the granted role, the
    /// row's own queue, and the document sent as text.
    fn granted() -> Self {
        Self {
            role: RoleKind::Granted,
            queue: QueueKind::TheQueue,
            use_base64: Some(false),
        }
    }

    /// The action as a client writes it into `TopicRulePayload$actions`.
    fn action(&self, role_arn: &str, queue_url: &str) -> Value {
        let mut action = json!({
            "roleArn": role_arn,
            "queueUrl": self.queue.queue_url(queue_url),
        });
        if let Some(use_base64) = self.use_base64 {
            action["useBase64"] = json!(use_base64);
        }
        json!({ "sqs": action })
    }
}

/// One message the queue must hold: the selected message, in the form the action sends it in.
#[derive(Debug, Clone, Copy)]
enum Forwarded {
    /// The body IS the selected message.
    Text(&'static str),
    /// The body is the base64 encoding of the selected message, which is what
    /// `useBase64` asks for.
    Base64(&'static str),
}

impl Forwarded {
    /// Whether the queue message `body` is this one.
    fn matches(self, body: &str) -> bool {
        match self {
            Self::Text(expected) => same_message(expected, body),
            Self::Base64(expected) => aws_smithy_types::base64::decode(body)
                .ok()
                .is_some_and(|decoded| {
                    same_message(expected, &String::from_utf8_lossy(&decoded))
                }),
        }
    }
}

/// Whether `actual` is the message `expected`: the same text, or the same JSON DOCUMENT where
/// both are documents — a projected document's members are a set, and the order a JSON object
/// writes them in carries no meaning.
fn same_message(expected: &str, actual: &str) -> bool {
    if expected == actual {
        return true;
    }
    match (
        serde_json::from_str::<Value>(expected),
        serde_json::from_str::<Value>(actual),
    ) {
        (Ok(expected), Ok(actual)) => expected == actual,
        _ => false,
    }
}

/// Everything the queue holds BESIDE the control message, read once the control message is in
/// it and a further look finds nothing.
///
/// The control message is forwarded by a rule whose action is known to be performable, so a row
/// that expects NOTHING is judged against a queue that has already taken a later publish —
/// rather than against a queue nothing has reached yet, which would pass whatever the engine
/// did. A row that expects a message waits on THAT condition rather than on a span.
async fn forwarded_bodies(
    emulator: &Emulator,
    case: &str,
    queue_url: &str,
    control: &str,
) -> Vec<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut held: Vec<String> = Vec::new();
    loop {
        let batch = drained_once(emulator, case, queue_url).await;
        let empty = batch.is_empty();
        held.extend(batch);
        if empty && held.iter().any(|body| body == control) {
            break;
        }
        if Instant::now() >= giving_up_at {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    assert!(
        held.iter().any(|body| body == control),
        "case {case:?}: the control publish was never forwarded, so what the queue holds says \
         nothing about this row; it held {held:?}"
    );
    held.retain(|body| body != control);
    held
}

/// One drain of the queue: the bodies it served this time, which is none once everything sent
/// to it has been read.
async fn drained_once(emulator: &Emulator, case: &str, queue_url: &str) -> Vec<String> {
    let received = emulator
        .sqs(
            case,
            "ReceiveMessage",
            &json!({
                "QueueUrl": queue_url,
                "MaxNumberOfMessages": RECEIVE_BATCH,
                "WaitTimeSeconds": WAIT_TIME_SECONDS,
            }),
        )
        .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// One row: the rule the case stores, the message it publishes, and what the queue must hold
/// afterwards.
struct Row {
    case: &'static str,
    /// The rule's `sql`.
    sql: &'static str,
    /// The topic the message is published to.
    topic: &'static str,
    /// Whether the rule is created disabled.
    disabled: bool,
    /// The QoS the message is published at (`Qos`, range 0..1).
    qos: u8,
    /// Whether the broker REFUSES this publish, which is a 400 and no rule run at all.
    refused: bool,
    /// The message payload, as published.
    payload: &'static str,
    /// The `sqs` actions the rule carries.
    actions: Vec<ActionSpec>,
    /// The messages the queue must hold afterwards.
    expected: Vec<Forwarded>,
}

impl Row {
    /// A row with the defaults every row shares — an enabled rule of one `sqs` action under
    /// the granted role, publishing [`MESSAGE`] at QoS 0, expected to forward that message
    /// once as text — so each builder below changes exactly what its case varies.
    fn new(case: &'static str, sql: &'static str, topic: &'static str) -> Self {
        Self {
            case,
            sql,
            topic,
            disabled: false,
            qos: 0,
            refused: false,
            payload: MESSAGE,
            actions: vec![ActionSpec::granted()],
            expected: vec![Forwarded::Text(MESSAGE)],
        }
    }

    /// The rule forwards nothing for this message.
    fn forwards_nothing(mut self) -> Self {
        self.expected = Vec::new();
        self
    }

    /// The rule forwards `selected` rather than the message as published.
    fn forwards(mut self, selected: &'static str) -> Self {
        self.expected = vec![Forwarded::Text(selected)];
        self
    }

    /// The rule is created disabled.
    fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    /// The message is published at `qos`.
    fn at_qos(mut self, qos: u8) -> Self {
        self.qos = qos;
        self
    }

    /// The broker refuses this publish.
    fn refused(mut self) -> Self {
        self.refused = true;
        self.expected = Vec::new();
        self
    }

    /// The message published is `payload`, and is what the rule forwards.
    fn publishing(mut self, payload: &'static str) -> Self {
        self.payload = payload;
        self.expected = vec![Forwarded::Text(payload)];
        self
    }

    /// The rule carries `actions`, and the queue must then hold `expected`.
    fn with(mut self, actions: Vec<ActionSpec>, expected: Vec<Forwarded>) -> Self {
        self.actions = actions;
        self.expected = expected;
        self
    }

    /// The rule's one action runs under `role`, with everything else as the default.
    fn under(self, role: RoleKind, expected: Vec<Forwarded>) -> Self {
        self.with(
            vec![ActionSpec {
                role,
                ..ActionSpec::granted()
            }],
            expected,
        )
    }

    /// The rule's one action addresses `queue`, under `role`.
    fn addressing(self, queue: QueueKind, role: RoleKind) -> Self {
        self.with(
            vec![ActionSpec {
                role,
                queue,
                ..ActionSpec::granted()
            }],
            Vec::new(),
        )
    }
}

fn rows() -> Vec<Row> {
    vec![
        // What the statement and the filter say about the message.
        Row::new(
            "an exact topic filter and SELECT *",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        ),
        Row::new(
            "the same publish at the other QoS the model allows",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .at_qos(1),
        Row::new(
            "a single-level wildcard covering the topic",
            "SELECT * FROM 'sensors/+/reading'",
            "sensors/two/reading",
        ),
        Row::new(
            "a multi-level wildcard covering the topic",
            "SELECT * FROM 'sensors/#'",
            "sensors/three/deep/reading",
        ),
        Row::new(
            "a topic filter that does not cover the published topic",
            "SELECT * FROM 'sensors/one'",
            "sensors/other",
        )
        .forwards_nothing(),
        Row::new(
            "a disabled rule whose filter does cover the topic",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .disabled()
        .forwards_nothing(),
        Row::new(
            "a condition that holds for the message",
            "SELECT * FROM 'sensors/one' WHERE temp > 40",
            "sensors/one",
        ),
        Row::new(
            "a condition that does not hold for the message",
            "SELECT * FROM 'sensors/one' WHERE temp > 100",
            "sensors/one",
        )
        .forwards_nothing(),
        Row::new(
            "a select clause naming attributes instead of the whole message",
            "SELECT marker, temp AS celsius FROM 'sensors/one'",
            "sensors/one",
        )
        .forwards(r#"{"marker":"m-1","celsius":42}"#),
        Row::new(
            "a payload that is not a JSON document, selected whole",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .publishing("not a document"),
        Row::new(
            "a publish whose topic carries an MQTT wildcard, which the broker refuses",
            "SELECT * FROM 'sensors/+'",
            "sensors/+",
        )
        .refused(),
        // What the action says about the form the message is sent in.
        Row::new(
            "an action asking for base64 encoding",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .with(
            vec![ActionSpec {
                use_base64: Some(true),
                ..ActionSpec::granted()
            }],
            vec![Forwarded::Base64(MESSAGE)],
        ),
        Row::new(
            "an action leaving useBase64 out",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .with(
            vec![ActionSpec {
                use_base64: None,
                ..ActionSpec::granted()
            }],
            vec![Forwarded::Text(MESSAGE)],
        ),
        Row::new(
            "two sqs actions on one rule, one encoded and one not",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .with(
            vec![
                ActionSpec::granted(),
                ActionSpec {
                    use_base64: Some(true),
                    ..ActionSpec::granted()
                },
            ],
            vec![Forwarded::Text(MESSAGE), Forwarded::Base64(MESSAGE)],
        ),
        // What the action's role says about the send.
        Row::new(
            "an action whose role allows every Amazon SQS action on every resource",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .under(RoleKind::Wildcard, vec![Forwarded::Text(MESSAGE)]),
        Row::new(
            "an action whose role allows another Amazon SQS action on the queue",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .under(RoleKind::AnotherAction, Vec::new()),
        Row::new(
            "an action whose role allows the send on another queue",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .under(RoleKind::AnotherQueue, Vec::new()),
        Row::new(
            "an action whose role carries no inline policy",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .under(RoleKind::NoPolicy, Vec::new()),
        Row::new(
            "an action whose role trusts another service principal",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .under(RoleKind::AnotherPrincipal, Vec::new()),
        // What the action's queueUrl addresses.
        Row::new(
            "an action whose queueUrl carries a substitution template",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .addressing(QueueKind::Templated, RoleKind::Wildcard),
        Row::new(
            "an action whose queueUrl addresses a queue the account does not hold",
            "SELECT * FROM 'sensors/one'",
            "sensors/one",
        )
        .addressing(QueueKind::Absent, RoleKind::Wildcard),
    ]
}

/// RULE: a message the IoT data plane accepts is forwarded by the `sqs` action of every enabled
/// topic rule whose filter and statement select it — one send of the selected message to the
/// queue the action's `queueUrl` addresses, in the form `useBase64` asks for, under the action's
/// own role — and nothing is sent for an action whose role bars the send, whose `queueUrl` is a
/// template, or whose queue Amazon SQS refuses.
#[tokio::test]
async fn a_published_message_is_forwarded_to_the_queue_of_every_rule_that_selects_it() {
    for row in rows() {
        let emulator = emulator();
        let queue_url = given_queue(&emulator, row.case, QUEUE).await;
        let queue_arn = given_queue_arn(&emulator, row.case, &queue_url).await;
        // A second queue that EXISTS, so a role granted the send on it is refused on the
        // resource element rather than because the queue it names is not there.
        let another_url = given_queue(&emulator, row.case, ANOTHER_QUEUE).await;
        let another_arn = given_queue_arn(&emulator, row.case, &another_url).await;

        // One role per kind the row's actions name, created once even where two actions share
        // it. The granted role is created first whatever the row names, because the CONTROL
        // rule below runs under it.
        let granted =
            given_role(&emulator, row.case, RoleKind::Granted, &queue_arn, &another_arn).await;
        let mut roles: Vec<(RoleKind, String)> = vec![(RoleKind::Granted, granted.clone())];
        let mut actions: Vec<Value> = Vec::new();
        for spec in &row.actions {
            let role_arn = match roles.iter().find(|(kind, _)| *kind == spec.role) {
                Some((_, arn)) => arn.clone(),
                None => {
                    let arn =
                        given_role(&emulator, row.case, spec.role, &queue_arn, &another_arn).await;
                    roles.push((spec.role, arn.clone()));
                    arn
                }
            };
            actions.push(spec.action(&role_arn, &queue_url));
        }
        emulator
            .create_topic_rule(
                row.case,
                RULE,
                &json!({
                    "sql": row.sql,
                    "awsIotSqlVersion": SQL_VERSION,
                    "ruleDisabled": row.disabled,
                    "actions": actions,
                }),
            )
            .await;
        // The CONTROL rule: a forward that is known to be performable, over a topic no row's
        // filter covers. The publish to it comes after the row's own, so a row that expects
        // nothing is judged against a queue that has since taken a message.
        emulator
            .create_topic_rule(
                row.case,
                CONTROL_RULE,
                &json!({
                    "sql": format!("SELECT * FROM '{CONTROL_TOPIC}'"),
                    "awsIotSqlVersion": SQL_VERSION,
                    "ruleDisabled": false,
                    "actions": [ActionSpec::granted().action(&granted, &queue_url)],
                }),
            )
            .await;
        let answer = emulator
            .publish(row.topic, row.qos, row.payload.as_bytes())
            .await;
        if row.refused {
            assert_eq!(
                answer.status,
                400,
                "case {:?}: expected the broker to refuse the publish with 400, got {} {}",
                row.case,
                answer.status,
                answer.text()
            );
        } else {
            assert_eq!(
                answer.status,
                200,
                "case {:?}: expected the broker to accept the publish with 200, got {} {}",
                row.case,
                answer.status,
                answer.text()
            );
        }

        let control = emulator
            .publish(CONTROL_TOPIC, 0, CONTROL_MESSAGE.as_bytes())
            .await;
        assert_eq!(
            control.status,
            200,
            "case {:?}: the control publish must be accepted, got {} {}",
            row.case,
            control.status,
            control.text()
        );

        let held = forwarded_bodies(&emulator, row.case, &queue_url, CONTROL_MESSAGE).await;
        assert_eq!(
            held.len(),
            row.expected.len(),
            "case {:?}: expected the queue to hold {} message(s), got {held:?}",
            row.case,
            row.expected.len()
        );
        for expected in &row.expected {
            let matching = held.iter().filter(|body| expected.matches(body)).count();
            assert_eq!(
                matching, 1,
                "case {:?}: expected exactly one queue message carrying {expected:?}, got \
                 {held:?}",
                row.case
            );
        }
        // Nothing is ever sent to the second queue: no action of any row addresses it, so a
        // send that went anywhere other than where the action pointed shows up here. Read after
        // the control message was seen on the first queue, so the row's dispatch is done.
        let elsewhere = drained_once(&emulator, row.case, &another_url).await;
        assert!(
            elsewhere.is_empty(),
            "case {:?}: no action addresses the second queue, so it must hold nothing, got \
             {elsewhere:?}",
            row.case
        );
    }
}

/// The thing whose shadow the second table's shadow rows address, and the two topics its
/// publish rows publish to — both under the one filter the forwarding rule carries.
const THING: &str = "iot-identity-sensor";
const IDENTITY_TOPIC: &str = "sensors/identity";
const IDENTITY_DEEP_TOPIC: &str = "sensors/identity/deep/level";

/// The filter the second table's forwarding rule carries, which covers both of those topics.
const IDENTITY_FILTER: &str = "sensors/#";

/// The account every ARN a statement names belongs to.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The action that reads where the data plane answers, which is the one grant a row carrying no
/// grant of the plane's own operations holds.
const ENDPOINT_ACTION: &str = "iot:DescribeEndpoint";

/// The error shape and status the data plane refuses a call the policies in force do not permit
/// with, and the two members its refusal envelope carries.
const FORBIDDEN_EXCEPTION: &str = "ForbiddenException";
const FORBIDDEN_STATUS: u16 = 403;
const REFUSAL_MEMBERS: [&str; 2] = ["message", "traceId"];

/// The call one row of the second table makes against the data plane.
#[derive(Debug, Clone, Copy)]
enum Called {
    /// `Publish` of the row's marker document to this topic.
    Publish(&'static str),
    /// `UpdateThingShadow` reporting this member at this value.
    UpdateShadow(&'static str, i64),
    /// `GetThingShadow`.
    GetShadow,
}

impl Called {
    /// The IAM action this call performs.
    fn action(self) -> &'static str {
        match self {
            Self::Publish(_) => "iot:Publish",
            Self::UpdateShadow(_, _) => "iot:UpdateThingShadow",
            Self::GetShadow => "iot:GetThingShadow",
        }
    }

    /// Another action of the service, for the row whose grant is about something else.
    fn another_action(self) -> &'static str {
        match self {
            Self::Publish(_) => "iot:GetThingShadow",
            _ => "iot:Publish",
        }
    }

    /// The ARN of the resource this call ADDRESSES: the topic for a message, the thing for a
    /// shadow.
    fn resource_arn(self) -> String {
        match self {
            Self::Publish(topic) => format!("arn:aws:iot:{REGION}:{ACCOUNT}:topic/{topic}"),
            _ => format!("arn:aws:iot:{REGION}:{ACCOUNT}:thing/{THING}"),
        }
    }
}

/// What the row's caller is allowed while it makes that call.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// Every action of AWS IoT on every resource.
    EveryAction,
    /// Exactly the action the call makes, on the ARN the request addresses.
    TheActionOnTheResource,
    /// Another action of the service, on every resource.
    AnotherAction,
    /// Nothing of the data plane: the endpoint lookup alone.
    EndpointOnly,
}

impl Grant {
    /// The one `(action, resource)` the row's caller is allowed.
    fn allowed(self, call: Called) -> (String, String) {
        match self {
            Self::EveryAction => ("iot:*".to_string(), ANY_RESOURCE.to_string()),
            Self::TheActionOnTheResource => (call.action().to_string(), call.resource_arn()),
            Self::AnotherAction => (call.another_action().to_string(), ANY_RESOURCE.to_string()),
            Self::EndpointOnly => (ENDPOINT_ACTION.to_string(), ANY_RESOURCE.to_string()),
        }
    }
}

/// The resource element of a statement about every resource of the service.
const ANY_RESOURCE: &str = "*";

/// One row of the identity table: the call, what the caller may do while making it, and what
/// the call and the state must then look like.
struct IdentityRow {
    case: &'static str,
    call: Called,
    grant: Grant,
    /// Whether the plane SERVES the call rather than refusing it.
    served: bool,
    /// The shadow's `state.reported` document after the row, read back as the account's own
    /// credential, or `None` before the thing has a shadow at all.
    reported: Option<Value>,
    /// Whether the row's publish reaches the queue the rule forwards to.
    forwarded: bool,
}

/// The rows, in the order they run: each later row stands on what the earlier ones wrote, so a
/// refused write is visible as the state it did NOT change.
fn identity_rows() -> Vec<IdentityRow> {
    vec![
        IdentityRow {
            case: "a publish the caller's policies allow every action of the service for",
            call: Called::Publish(IDENTITY_TOPIC),
            grant: Grant::EveryAction,
            served: true,
            reported: None,
            forwarded: true,
        },
        IdentityRow {
            case: "a publish allowed exactly on the ARN of the topic it addresses",
            call: Called::Publish(IDENTITY_TOPIC),
            grant: Grant::TheActionOnTheResource,
            served: true,
            reported: None,
            forwarded: true,
        },
        IdentityRow {
            case: "a publish to a multi-level topic, allowed on that whole topic's ARN",
            call: Called::Publish(IDENTITY_DEEP_TOPIC),
            grant: Grant::TheActionOnTheResource,
            served: true,
            reported: None,
            forwarded: true,
        },
        IdentityRow {
            case: "a publish the caller is allowed another action of the service for",
            call: Called::Publish(IDENTITY_TOPIC),
            grant: Grant::AnotherAction,
            served: false,
            reported: None,
            forwarded: false,
        },
        IdentityRow {
            case: "a publish the caller is allowed nothing of the data plane for",
            call: Called::Publish(IDENTITY_TOPIC),
            grant: Grant::EndpointOnly,
            served: false,
            reported: None,
            forwarded: false,
        },
        IdentityRow {
            case: "a shadow write allowed exactly on the ARN of the thing it addresses",
            call: Called::UpdateShadow("battery", 87),
            grant: Grant::TheActionOnTheResource,
            served: true,
            reported: Some(json!({ "battery": 87 })),
            forwarded: false,
        },
        IdentityRow {
            case: "a second shadow write, whose member MERGES into the stored document",
            call: Called::UpdateShadow("signal", 4),
            grant: Grant::EveryAction,
            served: true,
            reported: Some(json!({ "battery": 87, "signal": 4 })),
            forwarded: false,
        },
        IdentityRow {
            case: "a shadow write the caller is allowed nothing of the data plane for",
            call: Called::UpdateShadow("battery", 11),
            grant: Grant::EndpointOnly,
            served: false,
            reported: Some(json!({ "battery": 87, "signal": 4 })),
            forwarded: false,
        },
        IdentityRow {
            case: "a shadow read allowed exactly on the ARN of the thing it addresses",
            call: Called::GetShadow,
            grant: Grant::TheActionOnTheResource,
            served: true,
            reported: Some(json!({ "battery": 87, "signal": 4 })),
            forwarded: false,
        },
        IdentityRow {
            case: "a shadow read the caller is allowed another action of the service for",
            call: Called::GetShadow,
            grant: Grant::AnotherAction,
            served: false,
            reported: Some(json!({ "battery": 87, "signal": 4 })),
            forwarded: false,
        },
    ]
}

/// Create `user_name`, put `document` in force on it, mint its long-term access key and answer
/// that key's id — the credential a request presents to speak for that user.
///
/// The `CreateAccessKey` answer CARRIES CREDENTIAL MATERIAL, so nothing here quotes it, not even
/// on a failure, which names the case and what it looked for instead.
async fn given_user_key(
    emulator: &Emulator,
    case: &str,
    user_name: &str,
    document: &str,
) -> String {
    emulator
        .iam(case, "CreateUser", &[("UserName", user_name)])
        .await;
    emulator
        .iam(
            case,
            "PutUserPolicy",
            &[
                ("UserName", user_name),
                ("PolicyName", "data-plane-call"),
                ("PolicyDocument", document),
            ],
        )
        .await;
    let minted = emulator
        .send(
            "POST",
            "/",
            "iam",
            "application/x-www-form-urlencoded",
            &[],
            format!("Action=CreateAccessKey&Version=2010-05-08&UserName={user_name}").into_bytes(),
        )
        .await;
    assert_eq!(
        minted.status, 200,
        "case {case:?}: the fixture CreateAccessKey({user_name}) must be served, got {}",
        minted.status
    );
    element(&minted.text(), "AccessKeyId")
}

/// The shadow's `state.reported` document, read as the ACCOUNT's own credential — the reading
/// that says what a refused write did or did not change.
async fn reported_state(emulator: &Emulator, case: &str) -> Value {
    let answer = emulator
        .send(
            "GET",
            &format!("/things/{THING}/shadow"),
            "iotdata",
            "application/json",
            &[],
            Vec::new(),
        )
        .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: GetThingShadow({THING}) must be served to the account, got {} {}",
        answer.status,
        answer.text()
    );
    answer.json()["state"]["reported"].clone()
}

/// RULE: a call into the IoT data plane is decided against the identity-based policies in force
/// on the principal the request's credential speaks for and the resource the request addresses —
/// `topic/<topic>` for a message, `thing/<thingName>` for a shadow — so an allowed `Publish` is
/// taken and forwarded by the rule that selects it, an allowed `UpdateThingShadow` answers the
/// MERGED state document and is readable back through `GetThingShadow`, and a call the policies
/// do not allow is refused with this plane's own refusal envelope and performs nothing.
#[tokio::test]
async fn a_data_plane_call_is_decided_against_the_callers_policies() {
    let emulator = emulator();
    let queue_url = given_queue(&emulator, "setup", QUEUE).await;
    let queue_arn = given_queue_arn(&emulator, "setup", &queue_url).await;
    let another_url = given_queue(&emulator, "setup", ANOTHER_QUEUE).await;
    let another_arn = given_queue_arn(&emulator, "setup", &another_url).await;
    let granted = given_role(&emulator, "setup", RoleKind::Granted, &queue_arn, &another_arn).await;

    // The thing the shadow rows address.
    let created = emulator
        .send(
            "POST",
            &format!("/things/{THING}"),
            "iot",
            "application/json",
            &[],
            b"{}".to_vec(),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateThing({THING}) must succeed, got {} {}",
        created.status,
        created.text()
    );

    // The rule that forwards every message published under the filter, and the control rule
    // whose forward is known to work.
    for (rule, filter) in [(RULE, IDENTITY_FILTER), (CONTROL_RULE, CONTROL_TOPIC)] {
        emulator
            .create_topic_rule(
                "setup",
                rule,
                &json!({
                    "sql": format!("SELECT * FROM '{filter}'"),
                    "awsIotSqlVersion": SQL_VERSION,
                    "ruleDisabled": false,
                    "actions": [ActionSpec::granted().action(&granted, &queue_url)],
                }),
            )
            .await;
    }

    for (index, row) in identity_rows().iter().enumerate() {
        let (action, resource) = row.grant.allowed(row.call);
        let document = identity_policy(&action, &resource);
        let user_name = format!("iot-identity-caller-{index}");
        let access_key_id = given_user_key(&emulator, row.case, &user_name, &document).await;

        // The marker this row's publish carries, so what the queue holds names the row that
        // sent it.
        let marker = json!({ "marker": format!("identity-{index}") }).to_string();
        let answer = match row.call {
            Called::Publish(topic) => {
                emulator
                    .publish_as(&access_key_id, topic, 0, marker.as_bytes())
                    .await
            }
            Called::UpdateShadow(member, value) => {
                let patch = json!({ "state": { "reported": { member: value } } }).to_string();
                emulator
                    .send_as(
                        &access_key_id,
                        "POST",
                        &format!("/things/{THING}/shadow"),
                        "iotdata",
                        "application/json",
                        &[],
                        patch.into_bytes(),
                    )
                    .await
            }
            Called::GetShadow => {
                emulator
                    .send_as(
                        &access_key_id,
                        "GET",
                        &format!("/things/{THING}/shadow"),
                        "iotdata",
                        "application/json",
                        &[],
                        Vec::new(),
                    )
                    .await
            }
        };

        if row.served {
            assert_eq!(
                answer.status,
                200,
                "case {:?}: expected the plane to serve the call with 200, got {} {}",
                row.case,
                answer.status,
                answer.text()
            );
            // A served shadow call answers the stored document itself, which is what makes an
            // accepted write observable in its own answer rather than only on a later read.
            if !matches!(row.call, Called::Publish(_)) {
                if let Some(expected) = &row.reported {
                    let answered = answer.json()["state"]["reported"].clone();
                    assert_eq!(
                        &answered, expected,
                        "case {:?}: expected the shadow call to answer the merged state \
                         {expected}, got {answered}",
                        row.case
                    );
                }
            }
        } else {
            assert_eq!(
                answer.status,
                FORBIDDEN_STATUS,
                "case {:?}: expected the plane to refuse the call with {FORBIDDEN_STATUS}, got \
                 {} {}",
                row.case,
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.error_type.as_deref(),
                Some(FORBIDDEN_EXCEPTION),
                "case {:?}: expected the refusal to name {FORBIDDEN_EXCEPTION} in the header a \
                 client resolves an error shape from, got {:?}",
                row.case,
                answer.error_type
            );
            let body = answer.json();
            let members: Vec<&str> = body
                .as_object()
                .map(|object| object.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                members,
                REFUSAL_MEMBERS.to_vec(),
                "case {:?}: expected the refusal envelope to carry exactly {REFUSAL_MEMBERS:?}, \
                 got {body}",
                row.case
            );
            assert!(
                body["message"].is_null(),
                "case {:?}: expected the refusal to state no message, got {body}",
                row.case
            );
            assert!(
                body["traceId"].as_str().is_some_and(|id| !id.is_empty()),
                "case {:?}: expected the refusal to carry a trace id, got {body}",
                row.case
            );
        }

        // What the STATE says, which is where a call that answered without doing the work, or
        // did the work without answering, shows up.
        if let Some(expected) = &row.reported {
            let held = reported_state(&emulator, row.case).await;
            assert_eq!(
                &held, expected,
                "case {:?}: expected the thing's reported state to be {expected}, got {held}",
                row.case
            );
        }

        let control = emulator
            .publish(CONTROL_TOPIC, 0, CONTROL_MESSAGE.as_bytes())
            .await;
        assert_eq!(
            control.status,
            200,
            "case {:?}: the control publish must be accepted, got {} {}",
            row.case,
            control.status,
            control.text()
        );
        let held = forwarded_bodies(&emulator, row.case, &queue_url, CONTROL_MESSAGE).await;
        let expected: Vec<String> = if row.forwarded {
            vec![marker.clone()]
        } else {
            Vec::new()
        };
        assert_eq!(
            held.len(),
            expected.len(),
            "case {:?}: expected the queue to hold {} forwarded message(s), got {held:?}",
            row.case,
            expected.len()
        );
        for body in &expected {
            let matching = held
                .iter()
                .filter(|forwarded| same_message(body, forwarded))
                .count();
            assert_eq!(
                matching, 1,
                "case {:?}: expected exactly one forwarded message carrying {body}, got {held:?}",
                row.case
            );
        }
    }
}
