// SPDX-License-Identifier: CC0-1.0

//! WHO may publish a message to the AWS IoT data plane, decided over the SHAPES an identity
//! policy can take, and what that decision does to the topic rule that selects the message.
//!
//! RULE: a call into the IoT data plane is authorized by the shared IAM evaluation of the
//! identity-based policies in force on the principal the request's credential speaks for,
//! against the action `iot:<OperationName>` and the ARN of the resource the request ADDRESSES
//! (`topic/<topic>` for `POST /topics/{topic}`, `smithy_query(query="operation",
//! operation="Publish")`) — an explicit `Deny` refuses the call whatever else allows it,
//! otherwise an `Allow` whose action and resource elements cover the request grants it,
//! otherwise IAM's default deny refuses it, and a permissions boundary in force caps whatever
//! the identity-based policies granted. A refused call is answered HTTP 403
//! `ForbiddenException` in this plane's own envelope and PERFORMS NOTHING: nothing is published,
//! so no enabled topic rule selecting the topic runs and nothing reaches the rule's action
//! target, and the plane's own retained-message store keeps nothing the refused call asked it to
//! retain. A granted call is served, and the message is then forwarded by the rule's `sqs`
//! action to the queue its `queueUrl` addresses.
//!
//! Five crates have to agree for that to hold — the IoT data plane takes the publish and the
//! authorization decision, IAM holds the caller's documents and the ceiling over them, STS mints
//! the session an assumed role's credential speaks for, the IoT control plane runs the rules
//! engine, and Amazon SQS takes the send — so the suite lives here: a crate-local suite can only
//! ever see one side of it.
//!
//! The rows differ along what the rule generalises over: the KIND of principal the credential
//! speaks for (an IAM user's long-term key, and an assumed role's session), and the SHAPE of
//! what is in force on it — an `Allow` on the exact ARN the request addresses, one on a
//! wildcard over those ARNs, one over the whole service's actions, one scoped to another
//! topic's ARN, one naming another action of the service, no document at all, an explicit
//! `Deny` of the action beside a broad `Allow`, an explicit `Deny` over every resource beside a
//! narrow `Allow`, and a permissions boundary that allows the action, one that allows only
//! another action, and one that denies the action outright.
//!
//! Each row asserts both halves — what the plane ANSWERED the caller, and what the targets hold
//! afterwards: the queue the rule forwards to, read once a final publish whose forward is known
//! to work has arrived (so a row expecting nothing is judged against a queue that has already
//! taken everything that was ever sent), and the retained-message store, read through the
//! plane's own `GetRetainedMessage`. A call that answered without doing the work, or did the
//! work without answering, fails here.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    rules_engine::SQS_SEND_MESSAGE_ACTION, SERVICE_PRINCIPAL,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region segment of every ARN a
/// statement names.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below belongs to.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The topic every row publishes to, and another topic that exists only so a statement can be
/// scoped to a topic the request does NOT address.
const TOPIC: &str = "iot-authz-telemetry";
const ANOTHER_TOPIC: &str = "iot-authz-elsewhere";

/// A resource element covering the topic ARNs of this suite by a trailing wildcard — "you can
/// use a multi-character match wildcard (*)" in a policy's resource element.
const TOPIC_WILDCARD: &str = "iot-authz-*";

/// The resource element of a statement about every resource.
const ANY_RESOURCE: &str = "*";

/// Another action of the data plane, for the rows whose grant is about something else.
const ANOTHER_ACTION: &str = "iot:GetThingShadow";

/// The action a publish performs, and the whole service's actions.
const PUBLISH_ACTION: &str = "iot:Publish";
const EVERY_IOT_ACTION: &str = "iot:*";

/// The rule that forwards every message of [`TOPIC`], the role its `sqs` action runs under, and
/// the queue it forwards to. A rule name carries letters, digits and underscores alone
/// (`RuleName`, `pattern ^[a-zA-Z0-9_]+$`).
const RULE: &str = "iot_authz_forward";
const RULE_ROLE: &str = "iot-authz-rule-role";
const QUEUE: &str = "iot-authz-forwarded";

/// The SQL version the rule is stored with (`TopicRulePayload$awsIotSqlVersion`).
const SQL_VERSION: &str = "2016-03-23";

/// The API versions the two `awsQuery` services this suite calls are addressed at.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The access key id the setup and the last publish present: one neither IAM nor STS holds a
/// record for, which is the account's own published credential — the caller this gate does not
/// bound, so the setup is never refused for an identity's sake.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The `RoleSessionName` the assumed-role rows ask for.
const SESSION_NAME: &str = "publisher";

/// The marker the LAST publish of the suite carries: once it is in the queue, every publish made
/// before it has been forwarded or was never sent, so what the queue holds can be compared
/// against what the rows expected without waiting out a fixed span for the rows that expect
/// nothing.
const SENTINEL: &str = "sentinel";

/// The status and error shape this plane refuses a call the policies in force do not permit
/// with, and the two members its refusal envelope carries.
const FORBIDDEN_EXCEPTION: &str = "ForbiddenException";
const FORBIDDEN_STATUS: u16 = 403;
const REFUSAL_MEMBERS: [&str; 2] = ["message", "traceId"];

/// The status message this plane acknowledges an accepted publish with.
const ACCEPTED_MESSAGE: &str = "OK";

/// The status the plane reports a topic no retained message was ever stored for with
/// (`GetRetainedMessage`, `ResourceNotFoundException` — HTTP 404).
const RETAINED_ABSENT_STATUS: u16 = 404;

/// How long an observation of the forwarding waits before calling it a hang, and how long
/// between two looks. Generous beside the work a row does, and what the wait is ON is a
/// CONDITION ([`drained_markers`]), never a span.
const DEADLINE: Duration = Duration::from_secs(30);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The long poll one drain makes, in seconds: none, because the rules engine's send completes
/// before the publish is answered. The waiting a loaded host needs is the drain LOOP's.
const WAIT_TIME_SECONDS: u64 = 0;

/// How many messages one drain reads at a time.
const RECEIVE_BATCH: u64 = 10;

/// The emulator these rows run against: the IoT control plane, the IoT data plane, Amazon SQS,
/// IAM and STS on ONE engine — the shape `core/router/src/lib.rs` mounts them in.
struct Emulator {
    router: MultiServiceRouter,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope is what routes a request here, and the credential ID is the identity each service
    // decides it as.
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
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    Emulator { router }
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request, and whose credential is `access_key_id`, which is the identity the service decides
/// the request as. The signature itself is never checked in this harness.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential={access_key_id}/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One HTTP answer: the status, the error shape it names, and the body.
struct Answer {
    status: u16,
    /// The `x-amzn-errortype` header, where a restJson1 client resolves the error shape of a
    /// refusal from.
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

    /// One `awsQuery` call to `signing_name`, as the account's own credential.
    async fn form_call(
        &self,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, &str)],
    ) -> Answer {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        self.send(
            "POST",
            "/",
            signing_name,
            "application/x-www-form-urlencoded",
            &[],
            body.into_bytes(),
        )
        .await
    }

    /// One IAM call that must be served, answered with its XML body.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, &str)]) -> String {
        let answer = self
            .form_call("iam", IAM_API_VERSION, action, params)
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

    /// One call that mints a CREDENTIAL — an access key, a session — answered with the access
    /// key id alone. Nothing here quotes the body, not even on a failure: it carries credential
    /// material, so a failure names the case and the call instead.
    async fn minted_access_key_id(
        &self,
        case: &str,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, &str)],
    ) -> String {
        let answer = self.form_call(signing_name, version, action, params).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: {action} must be served, got {}",
            answer.status
        );
        opt_element(&answer.text(), "AccessKeyId")
            .unwrap_or_else(|| panic!("case {case:?}: {action} reported no AccessKeyId"))
    }

    /// `CreateTopicRule`: the restJson1 `POST /rules/{ruleName}` of the IoT control plane, whose
    /// body IS the `topicRulePayload` (the member is the operation's HTTP payload).
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

    /// `Publish` of the data plane — `POST /topics/{topic}` — presenting `access_key_id`, which
    /// is the identity the plane's authorization gate decides the call as.
    async fn publish_as(
        &self,
        access_key_id: &str,
        topic: &str,
        retain: bool,
        payload: &[u8],
    ) -> Answer {
        self.send_as(
            access_key_id,
            "POST",
            &format!("/topics/{}?qos=0&retain={retain}", form_encode(topic)),
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
}

/// The ARN of one topic, which is the resource a statement about a publish to it names.
fn topic_arn(topic: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:topic/{topic}")
}

/// One statement of an identity-based document.
fn statement(effect: &str, action: &str, resource: &str) -> Value {
    json!({ "Effect": effect, "Action": action, "Resource": resource })
}

/// An `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    statement("Allow", action, resource)
}

/// A `Deny` of `action` on `resource`.
fn deny(action: &str, resource: &str) -> Value {
    statement("Deny", action, resource)
}

/// The document carrying `statements`, as a client sends it.
fn document(statements: &[Value]) -> String {
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// A trust policy the account root may assume a role under.
fn account_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A trust policy admitting one service principal.
fn service_trust_policy(principal: &str) -> String {
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

/// The ARN of the queue `queue_url` addresses, as its own `GetQueueAttributes` reports it — the
/// resource a statement about that send names.
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

/// The role the rule's `sqs` action runs under: it trusts the rules engine's service principal
/// and is granted that one send on that one queue, so the forwarding this suite reads is itself
/// a permitted action.
async fn given_rule_role(emulator: &Emulator, case: &str, queue_arn: &str) -> String {
    let created = emulator
        .iam(
            case,
            "CreateRole",
            &[
                ("RoleName", RULE_ROLE),
                ("AssumeRolePolicyDocument", &service_trust_policy(SERVICE_PRINCIPAL)),
            ],
        )
        .await;
    let arn = element(&created, "Arn");
    emulator
        .iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", RULE_ROLE),
                ("PolicyName", "rule-action"),
                (
                    "PolicyDocument",
                    &document(&[allow(SQS_SEND_MESSAGE_ACTION, queue_arn)]),
                ),
            ],
        )
        .await;
    arn
}

/// What kind of principal the row's credential speaks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrincipalKind {
    /// An IAM user's own long-term access key.
    User,
    /// A session STS minted for an assumed role — the identity a workload's execution role
    /// calls as.
    RoleSession,
}

/// One row: the principal the publish is made as, what is in force on it, and whether the plane
/// serves the call.
struct Row {
    case: &'static str,
    principal: PrincipalKind,
    /// The statements of the identity-based document in force, or none for a principal carrying
    /// no document at all.
    statements: Vec<Value>,
    /// The statements of the permissions boundary attached to it, when it carries one.
    boundary: Option<Vec<Value>>,
    /// Whether the plane SERVES the publish rather than refusing it.
    served: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the action allowed on the exact ARN of the topic the request addresses",
            principal: PrincipalKind::User,
            statements: vec![allow(PUBLISH_ACTION, &topic_arn(TOPIC))],
            boundary: None,
            served: true,
        },
        Row {
            case: "the action allowed under a wildcard over the topic ARNs",
            principal: PrincipalKind::User,
            statements: vec![allow(PUBLISH_ACTION, &topic_arn(TOPIC_WILDCARD))],
            boundary: None,
            served: true,
        },
        Row {
            case: "every action of the service allowed on every resource",
            principal: PrincipalKind::User,
            statements: vec![allow(EVERY_IOT_ACTION, ANY_RESOURCE)],
            boundary: None,
            served: true,
        },
        Row {
            case: "no identity-based document in force at all",
            principal: PrincipalKind::User,
            statements: Vec::new(),
            boundary: None,
            served: false,
        },
        Row {
            case: "the action allowed only on another topic's ARN",
            principal: PrincipalKind::User,
            statements: vec![allow(PUBLISH_ACTION, &topic_arn(ANOTHER_TOPIC))],
            boundary: None,
            served: false,
        },
        Row {
            case: "another action of the service allowed on every resource",
            principal: PrincipalKind::User,
            statements: vec![allow(ANOTHER_ACTION, ANY_RESOURCE)],
            boundary: None,
            served: false,
        },
        Row {
            case: "an explicit deny of the action beside an allow of every action",
            principal: PrincipalKind::User,
            statements: vec![
                allow(EVERY_IOT_ACTION, ANY_RESOURCE),
                deny(PUBLISH_ACTION, &topic_arn(TOPIC)),
            ],
            boundary: None,
            served: false,
        },
        Row {
            case: "an explicit deny over every resource beside an allow on the exact ARN",
            principal: PrincipalKind::User,
            statements: vec![
                allow(PUBLISH_ACTION, &topic_arn(TOPIC)),
                deny(EVERY_IOT_ACTION, ANY_RESOURCE),
            ],
            boundary: None,
            served: false,
        },
        Row {
            case: "a permissions boundary that also allows the action on that ARN",
            principal: PrincipalKind::User,
            statements: vec![allow(EVERY_IOT_ACTION, ANY_RESOURCE)],
            boundary: Some(vec![allow(PUBLISH_ACTION, &topic_arn(TOPIC))]),
            served: true,
        },
        Row {
            case: "a permissions boundary allowing only another action of the service",
            principal: PrincipalKind::User,
            statements: vec![allow(EVERY_IOT_ACTION, ANY_RESOURCE)],
            boundary: Some(vec![allow(ANOTHER_ACTION, ANY_RESOURCE)]),
            served: false,
        },
        Row {
            case: "a permissions boundary explicitly denying the action",
            principal: PrincipalKind::User,
            statements: vec![allow(EVERY_IOT_ACTION, ANY_RESOURCE)],
            boundary: Some(vec![
                allow(EVERY_IOT_ACTION, ANY_RESOURCE),
                deny(PUBLISH_ACTION, ANY_RESOURCE),
            ]),
            served: false,
        },
        Row {
            case: "a role session whose role allows the action on the ARN it addresses",
            principal: PrincipalKind::RoleSession,
            statements: vec![allow(PUBLISH_ACTION, &topic_arn(TOPIC))],
            boundary: None,
            served: true,
        },
        Row {
            case: "a role session whose role denies the action beside a broad allow",
            principal: PrincipalKind::RoleSession,
            statements: vec![
                allow(EVERY_IOT_ACTION, ANY_RESOURCE),
                deny(PUBLISH_ACTION, &topic_arn(TOPIC)),
            ],
            boundary: None,
            served: false,
        },
        Row {
            case: "a role session whose role carries no document at all",
            principal: PrincipalKind::RoleSession,
            statements: Vec::new(),
            boundary: None,
            served: false,
        },
    ]
}

/// Provision the row's principal and answer the access key id a request presents to speak for
/// it: an IAM user's own long-term key, or the key of a session STS minted for the row's role.
async fn given_caller(emulator: &Emulator, row: &Row, index: usize) -> String {
    let name = format!("iot-authz-caller-{index}");
    match row.principal {
        PrincipalKind::User => {
            emulator
                .iam(row.case, "CreateUser", &[("UserName", &name)])
                .await;
            if !row.statements.is_empty() {
                emulator
                    .iam(
                        row.case,
                        "PutUserPolicy",
                        &[
                            ("UserName", &name),
                            ("PolicyName", "data-plane-call"),
                            ("PolicyDocument", &document(&row.statements)),
                        ],
                    )
                    .await;
            }
            if let Some(boundary) = &row.boundary {
                // A boundary is attached as the ARN of a MANAGED policy
                // (`AttachedPermissionsBoundary$PermissionsBoundaryArn`), so the document goes
                // into a policy of this account first.
                let created = emulator
                    .iam(
                        row.case,
                        "CreatePolicy",
                        &[
                            ("PolicyName", &format!("{name}-boundary")),
                            ("PolicyDocument", &document(boundary)),
                        ],
                    )
                    .await;
                let policy_arn = element(&created, "Arn");
                emulator
                    .iam(
                        row.case,
                        "PutUserPermissionsBoundary",
                        &[("UserName", &name), ("PermissionsBoundary", &policy_arn)],
                    )
                    .await;
            }
            emulator
                .minted_access_key_id(
                    row.case,
                    "iam",
                    IAM_API_VERSION,
                    "CreateAccessKey",
                    &[("UserName", &name)],
                )
                .await
        }
        PrincipalKind::RoleSession => {
            emulator
                .iam(
                    row.case,
                    "CreateRole",
                    &[
                        ("RoleName", &name),
                        ("AssumeRolePolicyDocument", &account_trust_policy()),
                    ],
                )
                .await;
            if !row.statements.is_empty() {
                emulator
                    .iam(
                        row.case,
                        "PutRolePolicy",
                        &[
                            ("RoleName", &name),
                            ("PolicyName", "data-plane-call"),
                            ("PolicyDocument", &document(&row.statements)),
                        ],
                    )
                    .await;
            }
            emulator
                .minted_access_key_id(
                    row.case,
                    "sts",
                    STS_API_VERSION,
                    "AssumeRole",
                    &[
                        ("RoleArn", &format!("arn:aws:iam::{ACCOUNT}:role/{name}")),
                        ("RoleSessionName", SESSION_NAME),
                    ],
                )
                .await
        }
    }
}

/// Drain the queue until the sentinel's message has been seen, and answer every marker the queue
/// held. Waits on the CONDITION — the last publish of the suite arriving — rather than on a
/// span, so a row that expects NOTHING is judged against a queue that has already taken
/// everything that was ever sent.
async fn drained_markers(emulator: &Emulator, case: &str, queue_url: &str) -> BTreeSet<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while Instant::now() < giving_up_at && !seen.contains(SENTINEL) {
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
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            tokio::time::sleep(LOOK_AGAIN).await;
            continue;
        }
        for message in messages {
            let body = message["Body"].as_str().unwrap_or_default().to_string();
            seen.insert(forwarded_marker(case, &body));
        }
    }
    assert!(
        seen.contains(SENTINEL),
        "case {case:?}: the rule never forwarded the last publish of the suite, so what the \
         queue holds says nothing; it held {seen:?}"
    );
    seen
}

/// The `marker` member of one forwarded message, which names the publish that sent it.
fn forwarded_marker(case: &str, body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|document| {
            document
                .get("marker")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| panic!("case {case:?}: a forwarded message carries no marker: {body}"))
}

/// RULE: a publish into the IoT data plane is decided by the shared IAM evaluation of the
/// documents in force on the principal the request's credential speaks for against
/// `iot:Publish` on the ARN of the topic the request addresses — an explicit `Deny` wins, an
/// `Allow` covering that action and resource grants, IAM's default deny refuses, and a
/// permissions boundary caps the grant — and a refused publish is answered 403
/// `ForbiddenException` in this plane's envelope while performing nothing, so the rule that
/// selects the topic forwards exactly the messages the plane took.
#[tokio::test]
async fn a_publishs_policy_shape_decides_both_its_answer_and_its_rule_dispatch() {
    let emulator = emulator();

    let queue_url = given_queue(&emulator, "setup", QUEUE).await;
    let queue_arn = given_queue_arn(&emulator, "setup", &queue_url).await;
    let role_arn = given_rule_role(&emulator, "setup", &queue_arn).await;
    emulator
        .create_topic_rule(
            "setup",
            RULE,
            &json!({
                "sql": format!("SELECT * FROM '{TOPIC}'"),
                "awsIotSqlVersion": SQL_VERSION,
                "ruleDisabled": false,
                "actions": [{
                    "sqs": {
                        "roleArn": role_arn,
                        "queueUrl": queue_url,
                        "useBase64": false,
                    },
                }],
            }),
        )
        .await;

    let mut expected_markers: BTreeSet<String> = BTreeSet::new();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let access_key_id = given_caller(&emulator, row, index).await;
        let marker = format!("authz-{index}");
        let payload = json!({ "marker": marker }).to_string();
        // A row the plane REFUSES asks for retention, which a served publish would have stored
        // in the plane's own retained-message store — so the store holding nothing afterwards is
        // the refusal having performed nothing in the plane itself, beside the queue it never
        // reached.
        let answer = emulator
            .publish_as(&access_key_id, TOPIC, !row.served, payload.as_bytes())
            .await;

        if row.served {
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: expected the plane to serve the publish with 200, got {} {}",
                answer.status,
                answer.text()
            );
            let body = answer.json();
            assert_eq!(
                body["message"],
                json!(ACCEPTED_MESSAGE),
                "case {case:?}: expected the acknowledgement {ACCEPTED_MESSAGE:?}, got {body}"
            );
            assert!(
                body["traceId"].as_str().is_some_and(|id| !id.is_empty()),
                "case {case:?}: expected the acknowledgement to carry a traceId, got {body}"
            );
            expected_markers.insert(marker);
        } else {
            assert_eq!(
                answer.status, FORBIDDEN_STATUS,
                "case {case:?}: expected the plane to refuse the publish with \
                 {FORBIDDEN_STATUS}, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.error_type.as_deref(),
                Some(FORBIDDEN_EXCEPTION),
                "case {case:?}: expected the refusal to name {FORBIDDEN_EXCEPTION} in the header \
                 a client resolves an error shape from, got {:?}",
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
                "case {case:?}: expected the refusal envelope to carry exactly \
                 {REFUSAL_MEMBERS:?}, got {body}"
            );
            assert_eq!(
                body["message"],
                Value::Null,
                "case {case:?}: expected the refusal to carry a null message, got {body}"
            );
        }
    }

    // The last publish of the suite, made as the account's own credential — the caller this gate
    // does not bound — so its forward is known to be performable: once its marker is in the
    // queue, every publish made before it has been forwarded or was never sent.
    let sentinel = emulator
        .publish_as(
            ACCOUNT_KEY_ID,
            TOPIC,
            false,
            json!({ "marker": SENTINEL }).to_string().as_bytes(),
        )
        .await;
    assert_eq!(
        sentinel.status,
        200,
        "case {SENTINEL:?}: the account's own publish must be served, got {} {}",
        sentinel.status,
        sentinel.text()
    );
    expected_markers.insert(SENTINEL.to_string());

    let forwarded = drained_markers(&emulator, "the forwarded messages", &queue_url).await;
    assert_eq!(
        forwarded, expected_markers,
        "case \"the forwarded messages\": expected the queue to hold exactly the markers of the \
         publishes the plane served, got {forwarded:?}"
    );

    // Every refused row asked for its message to be RETAINED, and no served row did, so the
    // plane's own reading of the topic's retained message reports none: a refused publish wrote
    // nothing here either.
    let retained = emulator
        .send(
            "GET",
            &format!("/retainedMessage/{}", form_encode(TOPIC)),
            "iotdata",
            "application/json",
            &[],
            Vec::new(),
        )
        .await;
    assert_eq!(
        retained.status,
        RETAINED_ABSENT_STATUS,
        "case \"the retained message\": expected no retained message for the topic \
         ({RETAINED_ABSENT_STATUS}), got {} {}",
        retained.status,
        retained.text()
    );
}
