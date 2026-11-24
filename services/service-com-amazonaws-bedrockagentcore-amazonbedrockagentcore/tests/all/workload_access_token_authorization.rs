// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a call of the AgentCore data plane do what it asks, driven end to end
//! over the wire with the services that hold the identity and its policies.
//!
//! Two surfaces, one gate ([`agentcore::state`]'s authorization module): the identity
//! plane's token mints, and the memory-record operations, whose decision AWS also scopes by
//! the namespace the request addresses.
//!
//! The rule the mint cases assert, one level above the recorded case (one principal granted
//! one workload identity, one call for another identity refused):
//!
//! > Every mint of the identity plane (`GetWorkloadAccessToken`,
//! > `GetWorkloadAccessTokenForJWT`, `GetWorkloadAccessTokenForUserId`) is authorized
//! > against the IDENTITY-BASED policies in force on the principal the request's credential
//! > names, on the action `bedrock-agentcore:<OperationName>` and on the workload-identity
//! > ARN the request's `workloadName` addresses: a call no statement allows (IAM's default
//! > deny) and one an explicit `Deny` covers are both refused `AccessDeniedException`
//! > (HTTP 403) with nothing minted and no issuance recorded, while a call a statement
//! > allows is served. The account's own published credential, which holds no IAM policies,
//! > is the position AWS account root credentials are in and is served.
//!
//! The rows span what the rule generalises over: the statements a policy can carry (an allow
//! of the exact action, a wildcard allow, an allow of one of the three operations only, an
//! allow scoped to ANOTHER workload identity, an explicit deny beside an allow, no policy at
//! all), and all three operations.
//!
//! The rule the memory-record cases assert:
//!
//! > A memory-record call (`ListMemoryRecords`, `GetMemoryRecord`, `DeleteMemoryRecord`)
//! > publishes the `namespace` its request addresses to the decision under this service's
//! > condition key `bedrock-agentcore:namespace` — the member the model documents as the
//! > one "used for IAM condition key authorization" — so a `Condition` clause over that key
//! > is evaluated: the call is served exactly when a statement whose clause the request's
//! > namespace satisfies allows it, and refused `AccessDeniedException` (HTTP 403) with the
//! > sentence naming the principal otherwise. The key is modelled on every such call, so a
//! > request addressing no single namespace carries no value for it and a clause comparing
//! > it to a value does not hold.
//!
//! Those rows span what that rule generalises over: the condition operator (`StringEquals`,
//! `StringLike`, `StringNotEquals`, a clause listing several values, and no clause at all),
//! which namespace of the container the request asks for, a request that names a namespace
//! HIERARCHY instead of one namespace, and the three operations that carry the member.

use std::sync::Arc;

use bytes::Bytes;
use engine::InMemoryEngine;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as agentcore;
use smithy_server_runtime::MultiServiceRouter;

/// The region every row's calls are signed for, which is the region segment of the ARN the
/// gate judges them on.
const REGION: &str = "us-east-1";

/// The emulator's own published long-term access key: the account credential, which holds no
/// IAM policies.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// The workload identity every row asks for a token for, and another one a policy can be
/// scoped to instead. Both are inside `WorkloadIdentityNameType` (len 3..255, pattern
/// `^[A-Za-z0-9_.-]+$`).
const WORKLOAD: &str = "permitted.agent";
const OTHER_WORKLOAD: &str = "elsewhere.agent";

/// The IAM action namespace of this service, which is its `arnNamespace`
/// (`smithy_query query="service_info"`).
const ACTION_PREFIX: &str = "bedrock-agentcore";

/// The code and status AWS refuses a call IAM does not permit with (`AccessDeniedException`,
/// `@httpError` 403, from `smithy_query query="operation"
/// operation="GetWorkloadAccessToken"`).
const DENIED_CODE: &str = "AccessDeniedException";
const DENIED_STATUS: u16 = 403;

/// One `userToken` this suite presents: a compact JWS whose header names a signing algorithm
/// and whose claim set is inside its own window, so the JWT mint's own validation does not
/// decide any row here.
fn user_token(subject: &str) -> String {
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("the clock is past the epoch")
        .as_secs()
        + 3_600;
    format!(
        "{}.{}.{}",
        service_common::jwt::encode_base64url(br#"{"alg":"RS256","typ":"JWT","kid":"k1"}"#),
        service_common::jwt::encode_base64url(
            json!({ "sub": subject, "exp": exp }).to_string().as_bytes()
        ),
        service_common::jwt::encode_base64url(b"presented-signature")
    )
}

/// The router every case drives: this service, plus the two that own the identity a request
/// speaks for (STS) and the policies in force on it (IAM), all on ONE engine.
struct Harness {
    engine: Arc<InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { engine, router }
    }

    /// One call of this service, signed with `access_key_id`: `method` and `path` are the
    /// operation's own HTTP binding, and `body` the payload for an operation that takes one.
    async fn call(
        &self,
        access_key_id: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let payload = body.map_or_else(Bytes::new, |body| {
            Bytes::from(serde_json::to_vec(&body).expect("the test body serializes"))
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://bedrock-agentcore.{REGION}.amazonaws.com{path}"
            ))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/\
                     bedrock-agentcore/aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(payload)
            .expect("the test request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// One identity-plane call, signed with `access_key_id`.
    async fn mint(&self, access_key_id: &str, operation: &str, body: Value) -> (u16, Value) {
        self.call(
            access_key_id,
            "POST",
            &format!("/identities/{operation}"),
            Some(body),
        )
        .await
    }

    /// Create the AgentCore Memory container the memory-record cases read, the way the
    /// CONTROL plane's `CreateMemory` does — this data plane creates none of its own.
    fn create_memory(&self, memory_id: &str) {
        super::harness::create_memory(&self.engine, memory_id);
    }

    /// One awsQuery call to IAM or STS: `signing_name` picks the service, `version` its API
    /// version.
    async fn query(
        &self,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, String)],
    ) -> (u16, String) {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encoded(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{signing_name}.amazonaws.com/"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(Bytes::from(body))
            .expect("the test request builds");
        let response = self.router.dispatch(request).await;
        (
            response.status().as_u16(),
            String::from_utf8_lossy(&response.into_body()).to_string(),
        )
    }

    /// Create an IAM role carrying `policy` as its inline policy (when one is given), assume
    /// it through STS, and answer the access key id of the session together with the
    /// principal ARN STS reports for it — the credential a caller then signs with, and the
    /// principal a refusal names.
    async fn assumed_role(&self, role_name: &str, policy: Option<&str>) -> (String, String) {
        let account = engine::EMULATOR_ACCOUNT_ID;
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "AWS": format!("arn:aws:iam::{account}:root") },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        let (status, body) = self
            .query(
                "iam",
                "2010-05-08",
                "CreateRole",
                &[
                    ("RoleName", role_name.to_string()),
                    ("AssumeRolePolicyDocument", trust),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "setup: creating the role {role_name:?} must succeed, got {status} {body}"
        );
        if let Some(policy) = policy {
            let (status, body) = self
                .query(
                    "iam",
                    "2010-05-08",
                    "PutRolePolicy",
                    &[
                        ("RoleName", role_name.to_string()),
                        ("PolicyName", "inline".to_string()),
                        ("PolicyDocument", policy.to_string()),
                    ],
                )
                .await;
            assert_eq!(
                status, 200,
                "setup: putting the inline policy of {role_name:?} must succeed, got {status} \
                 {body}"
            );
        }
        let (status, body) = self
            .query(
                "sts",
                "2011-06-15",
                "AssumeRole",
                &[
                    (
                        "RoleArn",
                        format!("arn:aws:iam::{account}:role/{role_name}"),
                    ),
                    ("RoleSessionName", "probe".to_string()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "setup: assuming {role_name:?} must succeed, got {status} {body}"
        );
        let key = xml_element(&body, "AccessKeyId")
            .unwrap_or_else(|| panic!("setup: AssumeRole must answer an AccessKeyId, got {body}"));
        let arn = xml_element(&body, "Arn")
            .unwrap_or_else(|| panic!("setup: AssumeRole must answer the session Arn, got {body}"));
        (key, arn)
    }

    /// How many issuances this service holds — the check that a refused call minted nothing.
    fn issuance_count(&self) -> usize {
        let state = agentcore::state::service_state(&*self.engine).expect("the service state");
        let store = state.lock();
        store.workload_access_tokens.len()
    }
}

/// One form field, percent-encoded the way an awsQuery body carries it.
fn form_encoded(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// The text of the first `<name>` element of an awsQuery XML response.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// An identity-based policy with one statement.
fn policy(effect: &str, actions: &[&str], resources: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Action": actions,
            "Resource": resources,
        }],
    })
    .to_string()
}

/// The workload-identity ARN a `workloadName` addresses, which is the resource the gate
/// judges a call on and the one a refusal names.
fn workload_identity_arn(workload: &str) -> String {
    agentcore::state::workload_identity_arn(REGION, workload)
}

/// The error shape a failure answered with.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|code| code.rsplit(['#', '/']).next().unwrap_or(code).to_string())
        .unwrap_or_else(|| format!("no __type in {body}"))
}

/// The three mints of this plane, with the request body each takes for `workload`.
fn mints(workload: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "GetWorkloadAccessToken",
            json!({ "workloadName": workload }),
        ),
        (
            "GetWorkloadAccessTokenForJWT",
            json!({ "workloadName": workload, "userToken": user_token("user-1") }),
        ),
        (
            "GetWorkloadAccessTokenForUserId",
            json!({ "workloadName": workload, "userId": "user-1" }),
        ),
    ]
}

/// One row: the policy a principal holds, and what each of the three mints owes it.
struct Row {
    case: &'static str,
    role: &'static str,
    /// The inline policy the role carries; `None` for a principal the account grants
    /// nothing.
    policy: Option<String>,
    /// The operations this principal may mint with; every other one is refused.
    allowed: &'static [&'static str],
    /// Whether a document of this principal's EXPLICITLY denies the call, which is the rule
    /// a refusal names — as against IAM's default deny, where no document refused it and
    /// none allowed it.
    explicitly_denied: bool,
}

/// A mint is served only to a caller IAM permits, and a refused one mints nothing.
#[tokio::test]
async fn a_workload_access_token_is_minted_only_for_a_caller_iam_permits() {
    let harness = Harness::new();
    let every: &[&str] = &[
        "GetWorkloadAccessToken",
        "GetWorkloadAccessTokenForJWT",
        "GetWorkloadAccessTokenForUserId",
    ];
    let rows = vec![
        Row {
            case: "a policy allowing every action of this service on this identity",
            role: "every-action",
            policy: Some(policy(
                "Allow",
                &[&format!("{ACTION_PREFIX}:*")],
                &[&workload_identity_arn(WORKLOAD)],
            )),
            allowed: every,
            explicitly_denied: false,
        },
        Row {
            case: "a wildcard allow of every action on every resource",
            role: "wildcard",
            policy: Some(policy("Allow", &[&format!("{ACTION_PREFIX}:*")], &["*"])),
            allowed: every,
            explicitly_denied: false,
        },
        Row {
            case: "an allow of one operation only",
            role: "one-operation",
            policy: Some(policy(
                "Allow",
                &[&format!("{ACTION_PREFIX}:GetWorkloadAccessToken")],
                &["*"],
            )),
            allowed: &["GetWorkloadAccessToken"],
            explicitly_denied: false,
        },
        Row {
            case: "an allow scoped to another workload identity",
            role: "other-identity",
            policy: Some(policy(
                "Allow",
                &[&format!("{ACTION_PREFIX}:*")],
                &[&workload_identity_arn(OTHER_WORKLOAD)],
            )),
            allowed: &[],
            explicitly_denied: false,
        },
        Row {
            case: "an explicit deny beside no allow",
            role: "explicit-deny",
            policy: Some(policy("Deny", &[&format!("{ACTION_PREFIX}:*")], &["*"])),
            allowed: &[],
            explicitly_denied: true,
        },
        Row {
            case: "an explicit deny of one operation beside an allow of every action",
            role: "deny-wins",
            policy: Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Action": format!("{ACTION_PREFIX}:*"),
                            "Resource": "*",
                        },
                        {
                            "Effect": "Deny",
                            "Action": format!("{ACTION_PREFIX}:GetWorkloadAccessToken"),
                            "Resource": workload_identity_arn(WORKLOAD),
                        },
                    ],
                })
                .to_string(),
            ),
            allowed: &[
                "GetWorkloadAccessTokenForJWT",
                "GetWorkloadAccessTokenForUserId",
            ],
            explicitly_denied: true,
        },
        Row {
            case: "no policy at all, which is IAM's default deny",
            role: "no-policy",
            policy: None,
            allowed: &[],
            explicitly_denied: false,
        },
    ];

    let mut minted = 0usize;
    for row in &rows {
        let case = row.case;
        let (key, principal) = harness.assumed_role(row.role, row.policy.as_deref()).await;

        for (operation, body) in mints(WORKLOAD) {
            let permitted = row.allowed.contains(&operation);
            let (status, answered) = harness.mint(&key, operation, body).await;
            let expected_status = if permitted { 200 } else { DENIED_STATUS };
            assert_eq!(
                status, expected_status,
                "case {case:?}/{operation}: expected HTTP {expected_status}, got {status} with \
                 {answered}"
            );
            if permitted {
                let token = answered
                    .get("workloadAccessToken")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert!(
                    !token.is_empty(),
                    "case {case:?}/{operation}: expected a minted token, got {answered}"
                );
                minted += 1;
                continue;
            }
            assert_eq!(
                error_code(&answered),
                DENIED_CODE,
                "case {case:?}/{operation}: expected {DENIED_CODE}, got {answered}"
            );
            // The sentence AWS words this refusal in: the principal that signed, the action
            // it asked for, the workload-identity ARN the request addressed, and which of
            // IAM's rules refused it — an explicit deny is NAMED, and IAM's default deny
            // states the action no policy allows.
            let action = format!("{ACTION_PREFIX}:{operation}");
            let reason = if row.explicitly_denied {
                "with an explicit deny in an identity-based policy".to_string()
            } else {
                format!("because no identity-based policy allows the {action} action")
            };
            let expected = format!(
                "User: {principal} is not authorized to perform: {action} on resource: {} {reason}",
                workload_identity_arn(WORKLOAD)
            );
            let message = answered
                .get("message")
                .or_else(|| answered.get("Message"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                message, expected,
                "case {case:?}/{operation}: expected {expected:?}, got {message:?}"
            );
            assert!(
                answered.get("workloadAccessToken").is_none(),
                "case {case:?}/{operation}: a refused call must mint nothing, got {answered}"
            );
            // Nothing was recorded for it either: the only issuances held are the ones the
            // permitted rows above minted.
            assert_eq!(
                harness.issuance_count(),
                minted,
                "case {case:?}/{operation}: a refused call must record no issuance"
            );
        }
    }

    // The account's own published credential holds no IAM policies — the position AWS
    // account root credentials are in — and is served.
    for (operation, body) in mints(OTHER_WORKLOAD) {
        let (status, answered) = harness.mint(ACCOUNT_KEY, operation, body).await;
        assert_eq!(
            status, 200,
            "case \"the account's own credential\"/{operation}: expected HTTP 200, got {status} \
             with {answered}"
        );
        minted += 1;
    }
    assert_eq!(
        harness.issuance_count(),
        minted,
        "expected one recorded issuance per minted token"
    );
}

/// The memory container the namespace-scoped cases read. `MemoryId` admits
/// `[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}` with a minimum length of 12.
const MEMORY: &str = "scoped-memory-0123456789";

/// Two sibling namespaces of that container, one record stored under each, and the parent
/// hierarchy both sit under — all inside `Namespace` (len 1..1024, pattern
/// `^[a-zA-Z0-9/*][a-zA-Z0-9-_/*]*(?::[a-zA-Z0-9-_/*]+)*[a-zA-Z0-9-_/*]*$`).
const ALPHA_NAMESPACE: &str = "/t/tenant-7/actor/alpha/facts/";
const BETA_NAMESPACE: &str = "/t/tenant-7/actor/beta/facts/";
const ACTOR_PREFIX: &str = "/t/tenant-7/actor/";

/// The condition key this service publishes the namespace a memory-record request addresses
/// under, which is what a scoped policy conditions its Allow on.
const NAMESPACE_KEY: &str = "bedrock-agentcore:namespace";

/// An identity-based policy with ONE Allow of `actions` on `resource`, conditioned on
/// `clauses` when the row carries any.
fn conditioned_policy(actions: &[String], resource: &str, clauses: Option<Value>) -> String {
    let mut statement = json!({
        "Effect": "Allow",
        "Action": actions,
        "Resource": resource,
    });
    if let Some(clauses) = clauses {
        statement["Condition"] = clauses;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// This service's action for one operation name.
fn action(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// The sentence this plane refuses a memory-record call IAM does not permit with: the
/// principal that signed, and nothing further.
fn refusal_for(principal: &str) -> String {
    format!("User: {principal} is not authorized to perform this operation")
}

/// The message a refusal carried.
fn message_of(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Write one record under each sibling namespace as the account's own credential (which
/// holds no IAM policies), and answer the `memoryRecordId` of each, keyed by its
/// `requestIdentifier`.
async fn seed_sibling_records(harness: &Harness) -> Vec<(String, String)> {
    let (status, created) = harness
        .call(
            ACCOUNT_KEY,
            "POST",
            &format!(
                "/memories/{}/memoryRecords/batchCreate",
                super::harness::label(MEMORY)
            ),
            Some(json!({"records": [
                {
                    "requestIdentifier": "alpha-1",
                    "namespaces": [ALPHA_NAMESPACE],
                    "content": {"text": "alpha-fact"},
                    "timestamp": 1_700_000_000,
                },
                {
                    "requestIdentifier": "beta-1",
                    "namespaces": [BETA_NAMESPACE],
                    "content": {"text": "beta-fact"},
                    "timestamp": 1_700_000_001,
                },
            ]})),
        )
        .await;
    assert_eq!(
        status, 201,
        "setup: seeding the two sibling records must succeed, got {status} with {created}"
    );
    created["successfulRecords"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|outcome| {
            (
                outcome["requestIdentifier"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                outcome["memoryRecordId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The `text` of every record one listing reported, in the order it reported them.
fn listed_texts(body: &Value) -> Vec<String> {
    body["memoryRecordSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| summary["content"]["text"].as_str().unwrap_or("").to_string())
        .collect()
}

/// One listing row: the namespace scope the request carries, and the records a permitted
/// call then reports.
struct Scope {
    case: &'static str,
    /// The body `ListMemoryRecords` is called with.
    body: Value,
    /// The `content.text` of the records this scope selects, most recent first.
    selects: &'static [&'static str],
}

/// A memory-record LISTING is decided by the namespace the request addresses, through the
/// `bedrock-agentcore:namespace` condition key the call publishes: a conditioned Allow
/// grants exactly the namespaces its clause covers and refuses the rest.
#[tokio::test]
async fn a_memory_record_listing_is_decided_by_the_namespace_condition_key() {
    let harness = Harness::new();
    harness.create_memory(MEMORY);
    seed_sibling_records(&harness).await;
    let memory_arn = agentcore::state::memory_arn(REGION, MEMORY);
    let list_path = format!("/memories/{}/memoryRecords", super::harness::label(MEMORY));

    let scopes = [
        Scope {
            case: "the namespace the scoped policies name",
            body: json!({ "namespace": ALPHA_NAMESPACE }),
            selects: &["alpha-fact"],
        },
        Scope {
            case: "its sibling namespace of the same container",
            body: json!({ "namespace": BETA_NAMESPACE }),
            selects: &["beta-fact"],
        },
        Scope {
            // `namespacePath` asks for "all memory records where namespace falls under the
            // same parent hierarchy", so the request addresses no ONE namespace and carries
            // no value for the key.
            case: "the parent hierarchy, which names no single namespace",
            body: json!({ "namespacePath": ACTOR_PREFIX }),
            selects: &["beta-fact", "alpha-fact"],
        },
    ];

    // (case, the Condition of the role's only Allow, which scopes above it may read)
    let rows: Vec<(&str, &str, Option<Value>, &[bool])> = vec![
        (
            "StringEquals on one namespace",
            "ns-equals",
            Some(json!({ "StringEquals": { NAMESPACE_KEY: ALPHA_NAMESPACE } })),
            &[true, false, false],
        ),
        (
            "StringLike on the parent prefix",
            "ns-like",
            Some(json!({ "StringLike": { NAMESPACE_KEY: format!("{ACTOR_PREFIX}*") } })),
            &[true, true, false],
        ),
        (
            "StringEquals listing both siblings",
            "ns-either",
            Some(json!({
                "StringEquals": { NAMESPACE_KEY: [ALPHA_NAMESPACE, BETA_NAMESPACE] }
            })),
            &[true, true, false],
        ),
        (
            // A negated comparison is the one clause a request carrying no value for the
            // key still satisfies, which is why the hierarchy row is permitted here.
            "StringNotEquals on one namespace",
            "ns-not-equals",
            Some(json!({ "StringNotEquals": { NAMESPACE_KEY: ALPHA_NAMESPACE } })),
            &[false, true, true],
        ),
        (
            "no condition at all, which scopes the grant to the container alone",
            "ns-unconditioned",
            None,
            &[true, true, true],
        ),
    ];

    for (case, role, clauses, permitted) in rows {
        let policy = conditioned_policy(
            &[action("ListMemoryRecords")],
            &memory_arn,
            clauses.clone(),
        );
        let (key, principal) = harness.assumed_role(role, Some(&policy)).await;
        for (scope, allowed) in scopes.iter().zip(permitted.iter()) {
            let (status, answered) = harness
                .call(&key, "POST", &list_path, Some(scope.body.clone()))
                .await;
            let name = format!("{case:?}/{:?}", scope.case);
            if *allowed {
                assert_eq!(
                    status, 200,
                    "case {name}: expected HTTP 200, got {status} with {answered}"
                );
                assert_eq!(
                    listed_texts(&answered),
                    scope.selects,
                    "case {name}: expected exactly the records of that scope, got {answered}"
                );
                continue;
            }
            assert_eq!(
                status, DENIED_STATUS,
                "case {name}: expected HTTP {DENIED_STATUS}, got {status} with {answered}"
            );
            assert_eq!(
                error_code(&answered),
                DENIED_CODE,
                "case {name}: expected {DENIED_CODE}, got {answered}"
            );
            assert_eq!(
                message_of(&answered),
                refusal_for(&principal),
                "case {name}: expected this plane's refusal sentence, got {answered}"
            );
            assert!(
                answered.get("memoryRecordSummaries").is_none(),
                "case {name}: a refused listing must report no record, got {answered}"
            );
        }
    }
}

/// The same key decides the two operations that carry the namespace as a query parameter:
/// `GetMemoryRecord` and `DeleteMemoryRecord`. The decision is a function of the namespace
/// the REQUEST names, and a refused delete removes nothing.
#[tokio::test]
async fn a_memory_record_read_and_delete_are_decided_by_the_same_key() {
    let harness = Harness::new();
    harness.create_memory(MEMORY);
    let seeded = seed_sibling_records(&harness).await;
    let record_id = |identifier: &str| -> String {
        seeded
            .iter()
            .find(|(sent, _)| sent == identifier)
            .map(|(_, id)| id.clone())
            .unwrap_or_else(|| panic!("setup: no record was created for {identifier:?}"))
    };
    let alpha_record = record_id("alpha-1");
    let beta_record = record_id("beta-1");
    let memory_arn = agentcore::state::memory_arn(REGION, MEMORY);
    // The only Allow this principal holds: both operations on this container, for the alpha
    // namespace alone.
    let policy = conditioned_policy(
        &[action("GetMemoryRecord"), action("DeleteMemoryRecord")],
        &memory_arn,
        Some(json!({ "StringEquals": { NAMESPACE_KEY: ALPHA_NAMESPACE } })),
    );
    let (key, principal) = harness.assumed_role("ns-record", Some(&policy)).await;

    // The two URIs the model binds these operations to — `/memoryRecord/{id}` for the read
    // and `/memoryRecords/{id}` for the delete — each carrying `namespace` as the query
    // parameter the model binds it to.
    let record_path = |collection: &str, record: &str, namespace: Option<&str>| -> String {
        let mut path = format!(
            "/memories/{}/{collection}/{}",
            super::harness::label(MEMORY),
            super::harness::label(record)
        );
        if let Some(namespace) = namespace {
            path.push_str(&format!("?namespace={}", super::harness::label(namespace)));
        }
        path
    };
    let read =
        |record: &str, namespace: Option<&str>| record_path("memoryRecord", record, namespace);
    let remove =
        |record: &str, namespace: Option<&str>| record_path("memoryRecords", record, namespace);

    // (case, method, path, whether the clause the request satisfies permits it)
    let rows: Vec<(&str, &str, String, bool)> = vec![
        (
            "a read naming the allowed namespace",
            "GET",
            read(&alpha_record, Some(ALPHA_NAMESPACE)),
            true,
        ),
        (
            "a read naming the sibling namespace",
            "GET",
            read(&alpha_record, Some(BETA_NAMESPACE)),
            false,
        ),
        (
            "a read naming no namespace at all",
            "GET",
            read(&alpha_record, None),
            false,
        ),
        (
            "a delete naming the sibling namespace",
            "DELETE",
            remove(&beta_record, Some(BETA_NAMESPACE)),
            false,
        ),
        (
            "a delete naming the allowed namespace",
            "DELETE",
            remove(&alpha_record, Some(ALPHA_NAMESPACE)),
            true,
        ),
    ];

    for (case, method, path, allowed) in rows {
        let (status, answered) = harness.call(&key, method, &path, None).await;
        if allowed {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {answered}"
            );
            continue;
        }
        assert_eq!(
            status, DENIED_STATUS,
            "case {case:?}: expected HTTP {DENIED_STATUS}, got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            DENIED_CODE,
            "case {case:?}: expected {DENIED_CODE}, got {answered}"
        );
        assert_eq!(
            message_of(&answered),
            refusal_for(&principal),
            "case {case:?}: expected this plane's refusal sentence, got {answered}"
        );
        assert!(
            answered.get("memoryRecord").is_none() && answered.get("memoryRecordId").is_none(),
            "case {case:?}: a refused call must report nothing of the record, got {answered}"
        );
    }

    // The refused delete removed nothing and the permitted one removed its own record: both
    // read back through the account's own credential, which no condition scopes.
    let (status, beta) = harness
        .call(ACCOUNT_KEY, "GET", &read(&beta_record, None), None)
        .await;
    assert_eq!(
        status, 200,
        "case \"the record a refused delete addressed\": expected it to still be readable, got \
         {status} with {beta}"
    );
    let (status, alpha) = harness
        .call(ACCOUNT_KEY, "GET", &read(&alpha_record, None), None)
        .await;
    assert_eq!(
        status, 404,
        "case \"the record a permitted delete removed\": expected HTTP 404, got {status} with \
         {alpha}"
    );
}
