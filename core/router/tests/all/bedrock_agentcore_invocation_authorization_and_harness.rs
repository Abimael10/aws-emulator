// SPDX-License-Identifier: CC0-1.0

//! Who may invoke the AgentCore DATA plane, and what one harness invocation serves.
//!
//! RULE: an AWS service decides a request at the IAM layer, in FRONT of the operation, so a
//! data-plane call whose signing principal is allowed `bedrock-agentcore:<OperationName>` on
//! the resource the request addresses by the identity-based policies in force on it reaches
//! the operation, and one no such policy allows is refused `AccessDeniedException`
//! (HTTP 403) naming the principal, the action and that resource — before anything the
//! request named is resolved, so a refusal discloses nothing about what exists. A call IAM
//! permits then resolves what it named against the component that owns it: an agent
//! runtime, an endpoint alias on one and a harness all belong to the AgentCore CONTROL
//! plane, so an identifier that plane holds nothing for is the modelled
//! `ResourceNotFoundException` (404).
//!
//! RULE: an invocation of a harness that IAM permits and the control plane holds answers
//! HTTP 200 with an `application/vnd.amazon.eventstream` body: the `messageStart`,
//! `contentBlockDelta`, `contentBlockStop`, `messageStop` and `metadata` events of
//! `InvokeHarnessStreamOutput`, in that order. No language model runs in this emulator, so
//! the text says exactly that and names the model the harness is configured with, the token
//! counts are counted over the text that went in and the text that came out, and a model
//! configured with a provider this emulator cannot reach is refused rather than answered
//! with a turn nothing composed.
//!
//! Four crates have to agree for the first rule to hold — IAM holds the roles and their
//! documents, STS mints the session credential and names its principal, the AgentCore
//! control plane holds the runtimes and harnesses, and the data plane takes the decision —
//! so the suite lives here, in the assembled router's own test crate; a crate-local suite
//! can only ever see one side of it, and the AgentCore service crate still declares its
//! suites by hand.
//!
//! The rows differ along what the rules generalise over: WHICH operation is called (a
//! memory-record read, a runtime invocation, a runtime-session stop, a harness invocation),
//! what the role's own document allows (nothing of the service, the action on another
//! resource of the same kind, the action on the resource addressed, every action of the
//! service, an explicit deny over a wildcard allow), WHICH resource the request addresses,
//! and — for the harness — which model provider it is configured with and whether the
//! qualifier it names exists.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN and every session ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The action namespace every row is decided under: this service's own `arnNamespace`
/// (`smithy_query(query="service_info")` on either AgentCore model).
const AGENTCORE: &str = "bedrock-agentcore";

/// The code and status AWS refuses a request IAM does not permit with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The long-term key every emulator publishes, which is the account's own credential — the
/// position AWS account root credentials are in, which no identity-based policy bounds.
const ACCOUNT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// A `runtimeSessionId` of the modelled shape (`SessionId`, length 33..100, pattern
/// `^[a-zA-Z0-9][a-zA-Z0-9-_]*$`), long enough for `StopRuntimeSession`'s own 33-character
/// floor.
const RUNTIME_SESSION: &str = "harness-session-0123456789abcdefghij";

/// A `memoryRecordId` of the modelled shape (`MemoryRecordId`, length 40..50, pattern
/// `^mem-[a-zA-Z0-9-_]*$`) that no batch ever wrote.
const GHOST_RECORD: &str = "mem-0000000000000000000000000000000000000000";

/// A `memoryId` of the modelled shape that no create ever minted.
const GHOST_MEMORY: &str = "ghost-memory-9876543210";

/// An `agentRuntimeId` of the modelled shape that no create ever minted.
const GHOST_RUNTIME: &str = "ghost_runtime-9z8y7x6w5v";

/// A `harnessId` of the modelled shape that no create ever minted.
const GHOST_HARNESS: &str = "ghost_harness-9z8y7x6w5v";

/// The model the harnesses below are configured with: a Bedrock model id, which is the one
/// provider whose inference surface this emulator serves itself.
const BEDROCK_MODEL: &str = "anthropic.claude-3-5-sonnet-20241022-v2:0";

/// The emulator as it is ASSEMBLED, on one engine, so IAM, STS and both AgentCore planes
/// are the ones this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request and the access key id is what names the
    // caller.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential is `access_key_id` and whose scope names
/// `signing_name`, which is what routes the request.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, as the account's own credential: the status and the XML.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(ACCOUNT_KEY, service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One AgentCore call over its `restJson1` wire, signed as `access_key_id`; the ROUTE picks
/// which of the two planes answers it. Answers the status and the raw body, because an
/// invocation's answer is an event stream rather than JSON.
async fn agentcore(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, Vec<u8>) {
    let (status, _, raw) = agentcore_answered(router, access_key_id, method, uri, headers, body)
        .await;
    (status, raw)
}

/// The same call, with the response HEADERS the model binds members to — the session id a
/// runtime answer carries is one of them.
async fn agentcore_answered(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, http::HeaderMap, Vec<u8>) {
    let payload = body
        .map(|value| Bytes::from(value.to_string()))
        .unwrap_or_default();
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{AGENTCORE}.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(access_key_id, AGENTCORE));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .dispatch(request.body(payload).expect("a well-formed request"))
        .await;
    let status = response.status().as_u16();
    let (parts, body) = response.into_parts();
    (status, parts.headers, body.to_vec())
}

/// The same call, read as the JSON an error or a non-streaming answer carries.
async fn agentcore_json(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, Value) {
    let (status, raw) = agentcore(router, access_key_id, method, uri, headers, body).await;
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The wire error code a refusal carries.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The message a refusal carries.
fn message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Percent-encode one `@httpLabel` / `@httpQuery` value the way an AWS SDK does, so an
/// ARN's `:` and `/` reach the server as part of the value rather than as URI syntax.
fn label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The trust policy a role this suite assumes needs.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The inline document a role carries: the statement the row puts in force, and — for a
/// `Deny` row — the wildcard allow of this service's actions beneath it, so what the row
/// asks is what an explicit deny does to a grant that would otherwise cover the call.
fn role_document(effect: &str, actions: &[&str], resource: &str) -> String {
    let mut statements = Vec::new();
    if !actions.is_empty() {
        if effect == "Deny" {
            statements.push(json!({
                "Effect": "Allow",
                "Action": format!("{AGENTCORE}:*"),
                "Resource": "*",
            }));
        }
        statements.push(json!({
            "Effect": effect,
            "Action": actions,
            "Resource": resource,
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create one IAM role carrying `document` and answer the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    });
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "probe-grants"),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// Assume `role_arn` and answer the access key id and the principal ARN of the session STS
/// minted — the principal a refusal names.
async fn assume_role(router: &MultiServiceRouter, role_arn: &str) -> (String, String) {
    let (status, body) = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[("RoleArn", role_arn), ("RoleSessionName", "probe")],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: AssumeRole({role_arn:?}) must succeed, got {status} with {body}"
    );
    let key = opt_element(&body, "AccessKeyId").unwrap_or_else(|| {
        panic!("setup: AssumeRole({role_arn:?}) must answer an access key id, got {body}")
    });
    let principal = opt_element(&body, "Arn").unwrap_or_else(|| {
        panic!("setup: AssumeRole({role_arn:?}) must answer the session ARN, got {body}")
    });
    (key, principal)
}

/// The sentence a refusal carries, built here from the request rather than from the
/// service's own helper, so the wording has something to be wrong about: IAM names the
/// principal, the action, the resource the request addressed and which of its rules
/// withheld the grant.
fn refusal_message(principal: &str, action: &str, resource: &str, denied: bool) -> String {
    let reason = if denied {
        "with an explicit deny in an identity-based policy".to_string()
    } else {
        format!("because no identity-based policy allows the {action} action")
    };
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// `CreateHarness` on the control plane: `POST /harnesses`, answering the harness's ARN and
/// the ARN of the AgentCore Runtime environment the service manages for it.
async fn create_harness(
    router: &MultiServiceRouter,
    name: &str,
    model: Option<Value>,
) -> (String, String) {
    let mut body = json!({
        "harnessName": name,
        "executionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/harness-execution"),
    });
    if let Some(model) = model {
        body["model"] = model;
    }
    let (status, created) =
        agentcore_json(router, ACCOUNT_KEY, "POST", "/harnesses", &[], Some(body)).await;
    assert_eq!(
        status, 201,
        "setup: CreateHarness({name:?}) must answer 201, got {status} with {created}"
    );
    let harness_arn = created["harness"]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateHarness reported no ARN: {created}"))
        .to_string();
    let runtime_arn = created["harness"]["environment"]["agentCoreRuntimeEnvironment"]
        ["agentRuntimeArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateHarness reported no runtime environment: {created}"))
        .to_string();
    (harness_arn, runtime_arn)
}

/// A Bedrock model configuration for `CreateHarness` / `InvokeHarness`.
fn bedrock_model(model_id: &str) -> Value {
    json!({"bedrockModelConfig": {"modelId": model_id}})
}

/// `InvokeHarness` on the data plane: `POST /harnesses/invoke?harnessArn=…`.
async fn invoke_harness(
    router: &MultiServiceRouter,
    access_key_id: &str,
    harness_arn: &str,
    qualifier: Option<&str>,
    body: Value,
) -> (u16, Vec<u8>) {
    let mut uri = format!("/harnesses/invoke?harnessArn={}", label(harness_arn));
    if let Some(qualifier) = qualifier {
        uri.push_str(&format!("&qualifier={}", label(qualifier)));
    }
    agentcore(
        router,
        access_key_id,
        "POST",
        &uri,
        &[(
            "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id",
            RUNTIME_SESSION,
        )],
        Some(body),
    )
    .await
}

/// One `vnd.amazon.eventstream` frame: the member of the output union it carries and its
/// payload as the JSON it holds.
///
/// A frame is its total length, its headers length, a prelude checksum, its headers, its
/// payload and a message checksum, each length a big-endian `u32` — so the payload is the
/// span between the headers and that trailing checksum.
#[derive(Debug)]
struct Frame {
    event_type: String,
    payload: Value,
}

/// Every frame of an event-stream body, in the order the stream carries them.
fn frames(case: &str, body: &[u8]) -> Vec<Frame> {
    let mut frames = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let total: [u8; 4] = rest
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or_else(|| panic!("case {case:?}: a frame carries a prelude, got {rest:?}"));
        let headers: [u8; 4] = rest
            .get(4..8)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or_else(|| panic!("case {case:?}: a frame carries a headers length"));
        let total = usize::try_from(u32::from_be_bytes(total)).expect("a frame length");
        let headers = usize::try_from(u32::from_be_bytes(headers)).expect("a headers length");
        assert!(
            total >= headers + 16 && total <= rest.len(),
            "case {case:?}: a frame of {total} bytes does not fit the {} bytes left",
            rest.len()
        );
        let header_bytes = rest
            .get(12..headers + 12)
            .unwrap_or_else(|| panic!("case {case:?}: a frame carries {headers} header bytes"));
        let payload = rest
            .get(headers + 12..total - 4)
            .unwrap_or_else(|| panic!("case {case:?}: a frame of {total} bytes carries a payload"));
        frames.push(Frame {
            // The `:event-type` header's value is the only ASCII name in the headers that
            // is not one of the two header names themselves, and it is the one the union's
            // member is reported under: it is read as the tail of the header block.
            event_type: event_type_of(case, header_bytes),
            payload: serde_json::from_slice(payload).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: an event payload must be JSON ({error}), got {:?}",
                    String::from_utf8_lossy(payload)
                )
            }),
        });
        rest = &rest[total..];
    }
    frames
}

/// The `:event-type` of one frame, read out of its header block: each header is a
/// length-prefixed name, a type byte and — for the string type the event headers use — a
/// big-endian `u16` length and the value.
fn event_type_of(case: &str, mut headers: &[u8]) -> String {
    while !headers.is_empty() {
        let name_len = usize::from(headers[0]);
        let name = headers
            .get(1..1 + name_len)
            .unwrap_or_else(|| panic!("case {case:?}: a header carries its name"));
        let value_len = headers
            .get(2 + name_len..4 + name_len)
            .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
            .map(|bytes| usize::from(u16::from_be_bytes(bytes)))
            .unwrap_or_else(|| panic!("case {case:?}: a string header carries its length"));
        let value = headers
            .get(4 + name_len..4 + name_len + value_len)
            .unwrap_or_else(|| panic!("case {case:?}: a header carries its value"));
        if name == b":event-type" {
            return String::from_utf8_lossy(value).to_string();
        }
        headers = &headers[4 + name_len + value_len..];
    }
    panic!("case {case:?}: every event frame carries a :event-type header")
}

/// WHICH data-plane call a row makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// `GetMemoryRecord`, which addresses one memory container.
    GetMemoryRecord,
    /// `DeleteMemoryRecord`, which addresses the same container — a WRITE of it, so a row
    /// over it is also a row about a refusal leaving the container untouched.
    DeleteMemoryRecord,
    /// `ListMemoryRecords`, the listing of that container's records.
    ListMemoryRecords,
    /// `BatchCreateMemoryRecords`, the write that fills it.
    BatchCreateMemoryRecords,
    /// `InvokeAgentRuntime`, which addresses one agent runtime.
    InvokeAgentRuntime,
    /// `StopRuntimeSession`, which addresses one agent runtime too.
    StopRuntimeSession,
    /// `InvokeHarness`, which addresses one harness.
    InvokeHarness,
}

impl Probe {
    /// The IAM action the call performs: `<namespace>:<OperationName>`.
    fn action(self) -> &'static str {
        match self {
            Self::GetMemoryRecord => "bedrock-agentcore:GetMemoryRecord",
            Self::DeleteMemoryRecord => "bedrock-agentcore:DeleteMemoryRecord",
            Self::ListMemoryRecords => "bedrock-agentcore:ListMemoryRecords",
            Self::BatchCreateMemoryRecords => "bedrock-agentcore:BatchCreateMemoryRecords",
            Self::InvokeAgentRuntime => "bedrock-agentcore:InvokeAgentRuntime",
            Self::StopRuntimeSession => "bedrock-agentcore:StopRuntimeSession",
            Self::InvokeHarness => "bedrock-agentcore:InvokeHarness",
        }
    }

    /// The resource ARN the call is judged against, as the request addresses it.
    fn resource(self) -> String {
        match self {
            Self::GetMemoryRecord
            | Self::DeleteMemoryRecord
            | Self::ListMemoryRecords
            | Self::BatchCreateMemoryRecords => {
                format!("arn:aws:{AGENTCORE}:{REGION}:{ACCOUNT}:memory/{GHOST_MEMORY}")
            }
            Self::InvokeAgentRuntime | Self::StopRuntimeSession => {
                format!("arn:aws:{AGENTCORE}:{REGION}:{ACCOUNT}:runtime/{GHOST_RUNTIME}")
            }
            Self::InvokeHarness => {
                format!("arn:aws:{AGENTCORE}:{REGION}:{ACCOUNT}:harness/{GHOST_HARNESS}")
            }
        }
    }

    /// Another resource of the same kind, which the scoped rows name INSTEAD of the one the
    /// call addresses.
    fn other_resource(self) -> String {
        self.resource().replace("ghost", "other")
    }

    /// Make the call as `access_key_id`.
    async fn send(self, router: &MultiServiceRouter, access_key_id: &str) -> (u16, Value) {
        match self {
            Self::GetMemoryRecord => {
                agentcore_json(
                    router,
                    access_key_id,
                    "GET",
                    &format!(
                        "/memories/{}/memoryRecord/{}",
                        label(GHOST_MEMORY),
                        label(GHOST_RECORD)
                    ),
                    &[],
                    None,
                )
                .await
            }
            Self::DeleteMemoryRecord => {
                agentcore_json(
                    router,
                    access_key_id,
                    "DELETE",
                    &format!(
                        "/memories/{}/memoryRecords/{}",
                        label(GHOST_MEMORY),
                        label(GHOST_RECORD)
                    ),
                    &[],
                    None,
                )
                .await
            }
            Self::ListMemoryRecords => {
                agentcore_json(
                    router,
                    access_key_id,
                    "POST",
                    &format!("/memories/{}/memoryRecords", label(GHOST_MEMORY)),
                    &[],
                    Some(json!({"maxResults": 10})),
                )
                .await
            }
            Self::BatchCreateMemoryRecords => {
                agentcore_json(
                    router,
                    access_key_id,
                    "POST",
                    &format!("/memories/{}/memoryRecords/batchCreate", label(GHOST_MEMORY)),
                    &[],
                    Some(json!({"records": [{
                        "requestIdentifier": "req-1",
                        "namespaces": ["support/facts/actor-1"],
                        "content": {"text": "a fact"},
                        "timestamp": 1_700_000_000,
                    }]})),
                )
                .await
            }
            Self::InvokeAgentRuntime => {
                agentcore_json(
                    router,
                    access_key_id,
                    "POST",
                    &format!("/runtimes/{}/invocations", label(&self.resource())),
                    &[(
                        "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id",
                        RUNTIME_SESSION,
                    )],
                    Some(json!({"prompt": "hello"})),
                )
                .await
            }
            Self::StopRuntimeSession => {
                agentcore_json(
                    router,
                    access_key_id,
                    "POST",
                    &format!("/runtimes/{}/stopruntimesession", label(&self.resource())),
                    &[(
                        "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id",
                        RUNTIME_SESSION,
                    )],
                    None,
                )
                .await
            }
            Self::InvokeHarness => {
                let (status, raw) = invoke_harness(
                    router,
                    access_key_id,
                    &self.resource(),
                    None,
                    json!({
                        "messages": [{"role": "user", "content": [{"text": "hello"}]}],
                        "model": bedrock_model(BEDROCK_MODEL),
                    }),
                )
                .await;
                let parsed = if raw.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&raw).unwrap_or(Value::Null)
                };
                (status, parsed)
            }
        }
    }

    /// The whole `ResourceNotFoundException` message a PERMITTED call is refused with when
    /// what it addressed does not exist.
    ///
    /// The runtime plane names the alias it looked for and the agent it looked on — one
    /// sentence for an invocation, a session stop and a harness invocation alike. The
    /// memory-records API words its container level after the item store the container is
    /// held in, with no variable part at all.
    fn absent_resource_message(self) -> String {
        match self {
            Self::GetMemoryRecord
            | Self::DeleteMemoryRecord
            | Self::ListMemoryRecords
            | Self::BatchCreateMemoryRecords => "MemoryItem does not exist".to_string(),
            Self::InvokeAgentRuntime | Self::StopRuntimeSession | Self::InvokeHarness => format!(
                "No endpoint or agent found with qualifier 'DEFAULT' for agent '{}'",
                self.resource()
            ),
        }
    }
}

/// Every data-plane call this suite probes, in one place so each table spans them all.
const PROBES: [Probe; 7] = [
    Probe::GetMemoryRecord,
    Probe::DeleteMemoryRecord,
    Probe::ListMemoryRecords,
    Probe::BatchCreateMemoryRecords,
    Probe::InvokeAgentRuntime,
    Probe::StopRuntimeSession,
    Probe::InvokeHarness,
];

/// One policy a row puts in force, and what it makes of the call. A struct rather than a
/// tuple so each field is read by name at the assertion.
struct Grant<'a> {
    case: &'static str,
    /// The `Effect` of the statement the role carries.
    effect: &'static str,
    /// The actions it states; empty for a role granted nothing of this service.
    actions: Vec<&'a str>,
    /// The resource it names.
    named: String,
    /// Whether the refusal is IAM's explicit-deny one rather than its default deny.
    denied: bool,
}

/// A principal no identity-based policy allows the action for is refused 403 with IAM's own
/// sentence, whatever the call addresses — and the resource the statement names decides it,
/// so the same action allowed on ANOTHER resource of the same kind is refused too and an
/// explicit deny beats a wildcard allow. Its second half
/// ([`a_permitted_call_reaches_the_resource_lookup_it_names`]) is what the same principals
/// reach once the policy allows them.
#[tokio::test]
async fn the_data_plane_refuses_a_call_no_identity_based_policy_allows() {
    let router = build_router();

    for probe in PROBES {
        let action = probe.action();
        let resource = probe.resource();
        let rows = [
            Grant {
                case: "a role granted nothing of this service",
                effect: "Allow",
                actions: Vec::new(),
                named: "*".to_string(),
                denied: false,
            },
            Grant {
                case: "the action on another resource of the same kind",
                effect: "Allow",
                actions: vec![action],
                named: probe.other_resource(),
                denied: false,
            },
            Grant {
                case: "another action of this service on the resource addressed",
                effect: "Allow",
                actions: vec!["bedrock-agentcore:ListActors"],
                named: resource.clone(),
                denied: false,
            },
            Grant {
                case: "an explicit deny over a wildcard allow",
                effect: "Deny",
                actions: vec![action],
                named: resource.clone(),
                denied: true,
            },
        ];
        for (
            index,
            Grant {
                case,
                effect,
                actions,
                named,
                denied,
            },
        ) in rows.into_iter().enumerate()
        {
            let role = format!("agc_gate_{index}_{}", probe.action().replace([':', '-'], "_"));
            let role_arn =
                create_role(&router, &role, &role_document(effect, &actions, &named)).await;
            let (key, principal) = assume_role(&router, &role_arn).await;

            let (status, refused) = probe.send(&router, &key).await;
            assert_eq!(
                status, ACCESS_DENIED_STATUS,
                "case {case:?} on {probe:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {status} \
                 with {refused}"
            );
            assert_eq!(
                error_code(&refused),
                ACCESS_DENIED,
                "case {case:?} on {probe:?}: expected {ACCESS_DENIED}, got {refused}"
            );
            assert_eq!(
                message(&refused),
                refusal_message(&principal, action, &resource, denied),
                "case {case:?} on {probe:?}: expected IAM's sentence, got {:?}",
                message(&refused)
            );
        }
    }

    // The other half of the same rule, over the same assembled emulator: what a principal
    // the policy DOES allow reaches.
    a_permitted_call_reaches_the_resource_lookup_it_names(&router).await;
}

/// A principal the policy DOES allow reaches the operation, which then resolves what the
/// request named against the component that owns it: an identifier nothing created is the
/// modelled 404, worded as that API words an absent resource. Both the statement scoped to
/// the resource addressed and the service-wide one permit the call.
///
/// Driven from the same test as the refusals above, over the same router: standing the
/// assembled emulator up is what a case here costs, so the two halves of one rule — refused
/// before the lookup, served into the lookup — share one.
async fn a_permitted_call_reaches_the_resource_lookup_it_names(router: &MultiServiceRouter) {
    for probe in PROBES {
        let resource = probe.resource();
        // (case, the actions the document allows, the resource it names)
        let rows = [
            (
                "the action on the resource addressed",
                vec![probe.action()],
                resource.clone(),
            ),
            (
                "every action of this service on every resource",
                vec!["bedrock-agentcore:*"],
                "*".to_string(),
            ),
        ];
        for (index, (case, actions, named)) in rows.into_iter().enumerate() {
            let role = format!(
                "agc_allow_{index}_{}",
                probe.action().replace([':', '-'], "_")
            );
            let role_arn =
                create_role(router, &role, &role_document("Allow", &actions, &named)).await;
            let (key, _) = assume_role(router, &role_arn).await;

            let (status, answered) = probe.send(router, &key).await;
            assert_eq!(
                status, 404,
                "case {case:?} on {probe:?}: expected the operation's own 404, got {status} with \
                 {answered}"
            );
            assert_eq!(
                error_code(&answered),
                "ResourceNotFoundException",
                "case {case:?} on {probe:?}: expected ResourceNotFoundException, got {answered}"
            );
            assert_eq!(
                message(&answered),
                probe.absent_resource_message(),
                "case {case:?} on {probe:?}: expected the documented sentence, got {:?}",
                message(&answered)
            );
        }
    }
}

/// An invocation of a harness the control plane holds answers the five events of the
/// output union, in order: the assistant message, its one text block, the stop reason and
/// the metadata of the invocation. The text says no model ran and names the model the
/// harness is configured with; the counts are counted.
///
/// The three harness tests share ONE assembled emulator for the reason the authorization
/// halves do: standing it up is what a case here costs.
#[tokio::test]
async fn a_harness_invocation_is_served_resolved_and_refused_as_the_model_declares() {
    let router = build_router();
    invoking_a_harness_streams_the_turn_it_served(&router).await;
    a_harness_invocation_establishes_the_session_a_stop_ends(&router).await;
    an_invocation_that_cannot_be_served_is_refused_in_its_own_class(&router).await;
}

/// The stream a served invocation carries (see the test above).
async fn invoking_a_harness_streams_the_turn_it_served(router: &MultiServiceRouter) {
    let (harness_arn, _) = create_harness(
        router,
        "streamed_harness",
        Some(bedrock_model(BEDROCK_MODEL)),
    )
    .await;

    // (case, the messages the invocation carries, the model it configures)
    let rows = [
        (
            "one user message, the harness's own model",
            json!([{"role": "user", "content": [{"text": "plan the trip"}]}]),
            None,
        ),
        (
            "a conversation of three turns",
            json!([
                {"role": "user", "content": [{"text": "plan the trip"}]},
                {"role": "assistant", "content": [{"text": "where to?"}]},
                {"role": "user", "content": [{"text": "to Dublin, next week"}]},
            ]),
            None,
        ),
        (
            "a model the invocation names instead",
            json!([{"role": "user", "content": [{"text": "plan the trip"}]}]),
            Some(bedrock_model("amazon.titan-text-express-v1")),
        ),
    ];
    for (case, messages, model) in rows {
        let expected_model = model
            .as_ref()
            .and_then(|model| model["bedrockModelConfig"]["modelId"].as_str())
            .unwrap_or(BEDROCK_MODEL)
            .to_string();
        let mut body = json!({"messages": messages});
        if let Some(model) = model {
            body["model"] = model;
        }
        let (status, raw) = invoke_harness(router, ACCOUNT_KEY, &harness_arn, None, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from InvokeHarness, got {status} with {:?}",
            String::from_utf8_lossy(&raw)
        );
        let streamed = frames(case, &raw);
        let order: Vec<&str> = streamed
            .iter()
            .map(|frame| frame.event_type.as_str())
            .collect();
        assert_eq!(
            order,
            vec![
                "messageStart",
                "contentBlockDelta",
                "contentBlockStop",
                "messageStop",
                "metadata"
            ],
            "case {case:?}: expected the turn's events in order, got {order:?}"
        );

        let start = &streamed[0].payload;
        assert_eq!(
            start["role"].as_str(),
            Some("assistant"),
            "case {case:?}: expected the assistant's own turn, got {start}"
        );
        let text = streamed[1].payload["delta"]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            text.contains("No language model runs in this emulator."),
            "case {case:?}: expected the turn to say that no model ran, got {text:?}"
        );
        assert!(
            text.contains(&expected_model),
            "case {case:?}: expected the turn to name {expected_model:?}, got {text:?}"
        );
        assert_eq!(
            streamed[2].payload["contentBlockIndex"].as_i64(),
            Some(0),
            "case {case:?}: expected the one content block to be stopped, got {}",
            streamed[2].payload
        );
        assert_eq!(
            streamed[3].payload["stopReason"].as_str(),
            Some("end_turn"),
            "case {case:?}: expected an uncapped turn to end on end_turn, got {}",
            streamed[3].payload
        );

        let usage = &streamed[4].payload["usage"];
        let input = usage["inputTokens"].as_i64().unwrap_or_default();
        let output = usage["outputTokens"].as_i64().unwrap_or_default();
        let total = usage["totalTokens"].as_i64().unwrap_or_default();
        assert!(
            input > 0 && output > 0 && total == input + output,
            "case {case:?}: expected counted token usage, got {usage}"
        );
        assert!(
            streamed[4].payload["metrics"]["latencyMs"].as_i64().is_some(),
            "case {case:?}: expected the invocation's latency, got {}",
            streamed[4].payload
        );
    }
}

/// The session an invocation establishes in the harness's own runtime environment is the
/// one `StopRuntimeSession` stops: the stop answers the session it ended, and a second stop
/// of it finds nothing left to stop.
async fn a_harness_invocation_establishes_the_session_a_stop_ends(router: &MultiServiceRouter) {
    let (harness_arn, runtime_arn) = create_harness(
        router,
        "stopped_harness",
        Some(bedrock_model(BEDROCK_MODEL)),
    )
    .await;

    // Nothing has been invoked yet, so there is no session in that environment to stop.
    let stop_uri = format!("/runtimes/{}/stopruntimesession", label(&runtime_arn));
    let (status, refused) = agentcore_json(
        router,
        ACCOUNT_KEY,
        "POST",
        &stop_uri,
        &[(
            "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id",
            RUNTIME_SESSION,
        )],
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "case \"a session never invoked\": expected HTTP 404, got {status} with {refused}"
    );
    assert_eq!(
        message(&refused),
        format!("No session found with id '{RUNTIME_SESSION}' for agent '{runtime_arn}'"),
        "case \"a session never invoked\": expected the session's own sentence, got {:?}",
        message(&refused)
    );

    let (status, _) = invoke_harness(
        router,
        ACCOUNT_KEY,
        &harness_arn,
        None,
        json!({"messages": [{"role": "user", "content": [{"text": "plan the trip"}]}]}),
    )
    .await;
    assert_eq!(status, 200, "the invocation must be served, got {status}");

    let (status, answered, stopped) = agentcore_answered(
        router,
        ACCOUNT_KEY,
        "POST",
        &stop_uri,
        &[(
            "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id",
            RUNTIME_SESSION,
        )],
        None,
    )
    .await;
    // `statusCode` is bound `@httpResponseCode` and `runtimeSessionId` to a response
    // header, so what the stop answers IS the status line and that header.
    assert_eq!(
        status,
        200,
        "case \"the invoked session\": expected HTTP 200, got {status} with {:?}",
        String::from_utf8_lossy(&stopped)
    );
    assert_eq!(
        answered
            .get("X-Amzn-Bedrock-AgentCore-Runtime-Session-Id")
            .and_then(|value| value.to_str().ok()),
        Some(RUNTIME_SESSION),
        "case \"the invoked session\": expected the stopped session to be named back, got {:?}",
        answered.get("X-Amzn-Bedrock-AgentCore-Runtime-Session-Id")
    );

    let (status, again) = agentcore_json(
        router,
        ACCOUNT_KEY,
        "POST",
        &stop_uri,
        &[(
            "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id",
            RUNTIME_SESSION,
        )],
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "case \"a session already stopped\": expected HTTP 404, got {status} with {again}"
    );
}

/// What an invocation cannot be served as: a qualifier naming no endpoint on the harness, a
/// harness with no model configured anywhere, and a model served by a provider this
/// emulator cannot reach — each in its own modelled class.
async fn an_invocation_that_cannot_be_served_is_refused_in_its_own_class(
    router: &MultiServiceRouter,
) {
    let (configured, _) = create_harness(
        router,
        "refused_harness",
        Some(bedrock_model(BEDROCK_MODEL)),
    )
    .await;
    let (unconfigured, _) = create_harness(router, "modelless_harness", None).await;

    let conversation = json!([{"role": "user", "content": [{"text": "plan the trip"}]}]);

    // A qualifier naming no endpoint on a harness that DOES exist: a harness is created
    // with no endpoints, so only `CreateHarnessEndpoint` makes an alias addressable.
    let (status, refused) = invoke_harness(
        router,
        ACCOUNT_KEY,
        &configured,
        Some("prod"),
        json!({"messages": conversation}),
    )
    .await;
    let refused: Value = serde_json::from_slice(&refused).unwrap_or(Value::Null);
    assert_eq!(
        status, 404,
        "case \"a qualifier naming no endpoint\": expected HTTP 404, got {status} with {refused}"
    );
    assert_eq!(
        message(&refused),
        format!("No endpoint or agent found with qualifier 'prod' for agent '{configured}'"),
        "case \"a qualifier naming no endpoint\": expected the alias to be named, got {:?}",
        message(&refused)
    );

    // Neither the harness nor the invocation configures a model, so there is no model to
    // run a turn against.
    let (status, refused) = invoke_harness(
        router,
        ACCOUNT_KEY,
        &unconfigured,
        None,
        json!({"messages": conversation}),
    )
    .await;
    let refused: Value = serde_json::from_slice(&refused).unwrap_or(Value::Null);
    assert_eq!(
        status, 400,
        "case \"no model configured\": expected HTTP 400, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "case \"no model configured\": expected ValidationException, got {refused}"
    );

    // A model on a provider this emulator cannot reach, one row per provider the union
    // declares beside Bedrock (source: smithy_query query="shape"
    // shape="HarnessModelConfiguration").
    let vault = format!(
        "arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:token-vault/default/\
         apikeycredentialprovider/provider"
    );
    let rows = [
        (
            "openAiModelConfig",
            json!({"openAiModelConfig": {"modelId": "gpt-4o-mini", "apiKeyArn": vault}}),
        ),
        (
            "geminiModelConfig",
            json!({"geminiModelConfig": {"modelId": "gemini-2.0-flash", "apiKeyArn": vault}}),
        ),
        (
            "liteLlmModelConfig",
            json!({"liteLlmModelConfig": {"modelId": "ollama/llama3"}}),
        ),
    ];
    for (case, model) in rows {
        let (status, refused) = invoke_harness(
            router,
            ACCOUNT_KEY,
            &configured,
            None,
            json!({"messages": conversation, "model": model}),
        )
        .await;
        let refused: Value = serde_json::from_slice(&refused).unwrap_or(Value::Null);
        assert_eq!(
            status, 424,
            "case {case:?}: expected HTTP 424, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "RuntimeClientError",
            "case {case:?}: expected RuntimeClientError, got {refused}"
        );
    }
}
