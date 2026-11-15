// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a call of the AgentCore identity plane mint a workload access token,
//! driven end to end over the wire with the services that hold the identity and its
//! policies.
//!
//! The rule these cases assert, one level above the recorded case (one principal granted
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

    /// One identity-plane call, signed with `access_key_id`.
    async fn mint(&self, access_key_id: &str, operation: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!(
                "https://bedrock-agentcore.{REGION}.amazonaws.com/identities/{operation}"
            ))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/\
                     bedrock-agentcore/aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(&body).expect("the test body serializes"),
            ))
            .expect("the test request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
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
