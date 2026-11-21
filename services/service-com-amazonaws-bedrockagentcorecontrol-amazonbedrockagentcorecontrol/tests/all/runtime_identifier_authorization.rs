// SPDX-License-Identifier: CC0-1.0

//! WHO may read and change an agent runtime of this control plane, driven end to end over
//! the wire path (a `restJson1` request through the multi-service router into the mounted
//! service), so what is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: every request this endpoint serves is authorized against the
//! identity-based IAM policies in force on the principal its credentials name, for the
//! action `bedrock-agentcore:<OperationName>` on the ARN of the resource it addresses,
//! BEFORE the model's constraints are validated and before any handler runs. A request no
//! readable statement allows (IAM's default deny), or one an explicit `Deny` covers, or one
//! the permissions boundary does not also allow, is answered HTTP 403
//! `AccessDeniedException` naming the principal and the action — whatever else is wrong with
//! the request, including an `agentRuntimeId` outside its member's `@pattern`, which a
//! denied caller therefore never hears about. A request a matching `Allow` covers reaches
//! the operation's own verdicts, where that same malformed identifier IS the modelled
//! `ValidationException`.
//!
//! Which callers the gate lets through UNJUDGED is the subject of the two tests at the end,
//! and `unnameable_identifier_authorization.rs` holds the one request those exemptions do
//! not reach: a call addressing its runtime by an identifier no runtime can carry.
//!
//! The user, its access key and its inline policy live in the IAM store of the SAME shared
//! engine — the store IAM's own handlers write — so the documents under test are the ones
//! `iam:PutUserPolicy` stores, and the principal a refusal blames is the one
//! `sts:GetCallerIdentity` answers for the key the request signed with.
//!
//! The tables span what the rule generalises over: the operation (all nine that address a
//! runtime by `{agentRuntimeId}`), the document in force (none, another service, another
//! operation of this namespace, this action exactly, the whole namespace, an explicit deny
//! beside an allow), the resource a statement is scoped to (every resource, the runtime
//! addressed, another runtime, the same id in another Region), the permissions boundary that
//! caps a grant, the kind of principal (an IAM user's long-term key, the account's own
//! published credential) and the state of the runtime the refusal says nothing about (held,
//! absent, named by an identifier no runtime can carry).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;

/// The region every row signs for, and a second one for the rows about a statement's scope.
pub(crate) const REGION: &str = "us-east-1";
const OTHER_REGION: &str = "eu-west-3";

/// An access key id neither store holds a record for: the account's own published
/// credential, which holds no IAM policy.
pub(crate) const AMBIENT_KEY_ID: &str = "AKIDEXAMPLE";

/// The access key id of the IAM user every policy row signs with, and the user it is held
/// for. The user's name appears nowhere in the key's text, so an identity synthesised from
/// that text cannot pass a row.
pub(crate) const USER_KEY_ID: &str = "AKIAEXAMPLEAGENTCORE1";
const USER: &str = "agentcore-operator";

/// The name of the inline policy the rows put in force.
const POLICY_NAME: &str = "inline";

/// The error code `restJson1` carries in `__type` and in `x-amzn-errortype`.
const ACCESS_DENIED: &str = "AccessDeniedException";
/// See [`ACCESS_DENIED`].
const VALIDATION_EXCEPTION: &str = "ValidationException";
/// See [`ACCESS_DENIED`].
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// A `roleArn` inside `RoleArn`'s pattern, for the rows whose request carries a body.
pub(crate) const VALID_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/ProbeRuntimeRole";

/// An `agentRuntimeId` inside `AgentRuntimeId`'s pattern that no row creates.
pub(crate) const ABSENT_RUNTIME_ID: &str = "noSuchRuntime-abcdefghij";

/// An `agentRuntimeId` OUTSIDE that pattern (no `-` plus ten alphanumerics), of the form a
/// caller derives from a runtime's log-group name.
pub(crate) const UNPATTERNED_RUNTIME_ID: &str = "probe_runtime_from_log_group";

/// One answer: the status, the error code, the `message` the body carries and the body
/// itself.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) body: Value,
}

/// One router over one engine: the control plane under test, plus the IAM and STS stores of
/// that same engine, which this suite — and `unnameable_identifier_authorization.rs`, which
/// drives the same decision over the identifier a route addresses its resource by — seed
/// directly.
pub(crate) struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Harness {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::default();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Send one call signed with `access_key_id` for `region`.
    pub(crate) async fn send_as(
        &self,
        access_key_id: &str,
        region: &str,
        method: &str,
        uri: &str,
        body: Option<&Value>,
    ) -> Answer {
        let payload = body.map_or_else(Bytes::new, |value| {
            Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
        });
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            // The SigV4 credential scope routes the request, names its region and names the
            // identity it speaks for; the signature itself is not checked here.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/\
                     bedrock-agentcore/aws4_request, SignedHeaders=host, Signature=abc"
                ),
            )
            .header("host", "bedrock-agentcore-control.amazonaws.com");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request.body(payload).expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let parsed: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer {
            status,
            code,
            message: parsed
                .pointer("/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            body: parsed,
        }
    }

    /// Send one call in [`REGION`] signed with the account's own published credential.
    pub(crate) async fn send(&self, method: &str, uri: &str, body: Option<&Value>) -> Answer {
        self.send_as(AMBIENT_KEY_ID, REGION, method, uri, body).await
    }

    /// Create a runtime named `name` with the account's own credential, and answer the
    /// `agentRuntimeId` the service minted for it.
    pub(crate) async fn create_runtime(&self, name: &str) -> String {
        let body = json!({
            "agentRuntimeName": name,
            "agentRuntimeArtifact": {"containerConfiguration": {
                "containerUri": "public.ecr.aws/agentcore/probe:v1"
            }},
            "networkConfiguration": {"networkMode": "PUBLIC"},
            "roleArn": VALID_ROLE_ARN,
        });
        let payload = Bytes::from(serde_json::to_vec(&body).expect("the body serializes"));
        let request = http::Request::builder()
            .method("PUT")
            .uri("/runtimes/")
            .header("authorization", authorization(AMBIENT_KEY_ID, REGION))
            .header("host", "bedrock-agentcore-control.amazonaws.com")
            .header("content-type", "application/json")
            .body(payload)
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        assert_eq!(
            status, 202,
            "case \"the setup create of {name}\": expected 202, got {status} ({parsed})"
        );
        parsed["agentRuntimeId"]
            .as_str()
            .expect("a create answers the id it minted")
            .to_string()
    }

    /// Record the IAM user [`USER_KEY_ID`] belongs to, as `iam:CreateUser` and
    /// `iam:CreateAccessKey` record one, and answer the ARN the account holds it under.
    pub(crate) fn given_user(&self) -> String {
        let arn = user_arn();
        let minted = ::aws_smithy_types::DateTime::from_secs(0);
        let store = iam::service_state(&*self.engine).expect("the IAM store");
        let mut store = store.lock();
        store.users.insert(
            iam::name_key(USER),
            User {
                path: "/".to_string(),
                user_name: USER.to_string(),
                user_id: sts::unique_id("AIDA", &arn),
                arn: arn.clone(),
                create_date: minted,
                password_last_used: None,
                permissions_boundary: None,
                tags: None,
            },
        );
        store.access_keys.insert(
            USER_KEY_ID.to_string(),
            iam::AccessKeyRecord {
                user_key: iam::name_key(USER),
                status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
                create_date: minted,
            },
        );
        arn
    }

    /// Put `document` in force as that user's inline policy, replacing whatever it carried
    /// — what `iam:PutUserPolicy` does. `None` takes every policy off it, as the matching
    /// delete does.
    pub(crate) fn given_policy(&self, document: Option<&str>) {
        let store = iam::service_state(&*self.engine).expect("the IAM store");
        let mut store = store.lock();
        let key = iam::name_key(USER);
        match document {
            None => {
                store.user_inline_policies.remove(&key);
            }
            Some(document) => {
                store.user_inline_policies.insert(
                    key,
                    BTreeMap::from([(
                        iam::name_key(POLICY_NAME),
                        iam::InlinePolicy {
                            policy_name: POLICY_NAME.to_string(),
                            policy_document: document.to_string(),
                        },
                    )]),
                );
            }
        }
    }

    /// Attach `policy_arn` to that user as a MANAGED policy, as `iam:AttachUserPolicy`
    /// does. A policy of AWS's own catalog is one this emulator holds no document for: a
    /// grant it cannot read, and so cannot make the reason a call is refused.
    pub(crate) fn given_attached_policy(&self, policy_arn: &str) {
        let store = iam::service_state(&*self.engine).expect("the IAM store");
        store
            .lock()
            .user_attached_policies
            .insert(iam::name_key(USER), BTreeSet::from([policy_arn.to_string()]));
    }
}

/// A SigV4 `Authorization` header for this service, signed with `access_key_id` in `region`.
fn authorization(access_key_id: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// The ARN the account holds the IAM user under, which a refusal names as that caller.
pub(crate) fn user_arn() -> String {
    format!(
        "arn:aws:iam::{}:user/{USER}",
        engine::emulator_account_id()
    )
}

/// The sentence this endpoint refuses an unauthorized call with.
pub(crate) fn not_authorized(principal: &str, operation: &str) -> String {
    format!("User: {principal} is not authorized to perform: bedrock-agentcore:{operation}")
}

/// A policy document granting `actions` on `resources`, as `iam:PutUserPolicy` takes one.
pub(crate) fn allow(actions: &str, resources: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[
         {{"Effect":"Allow","Action":{actions},"Resource":{resources}}}]}}"#
    )
}

/// One operation of the runtime family, addressing its runtime by `id`.
struct Addressed {
    /// The operation's own shape name, which is the action it is performed under.
    operation: &'static str,
    method: &'static str,
    /// The route with the `{agentRuntimeId}` label filled in.
    uri: String,
    body: Option<Value>,
}

/// Every operation whose modelled route carries `{agentRuntimeId}`, addressing its runtime
/// by `id`. Source: this crate's `OPERATIONS.md` (the `@http` bindings `smithy_query`
/// reports) — `GetAgentRuntime`, `UpdateAgentRuntime`, `DeleteAgentRuntime`,
/// `ListAgentRuntimeVersions` and the five runtime-endpoint routes.
fn addressing(id: &str) -> Vec<Addressed> {
    let update = json!({
        "agentRuntimeArtifact": {"containerConfiguration": {
            "containerUri": "public.ecr.aws/agentcore/probe:v1"
        }},
        "roleArn": VALID_ROLE_ARN,
    });
    vec![
        Addressed {
            operation: "GetAgentRuntime",
            method: "GET",
            uri: format!("/runtimes/{id}/"),
            body: None,
        },
        Addressed {
            operation: "UpdateAgentRuntime",
            method: "PUT",
            uri: format!("/runtimes/{id}/"),
            body: Some(update),
        },
        Addressed {
            operation: "DeleteAgentRuntime",
            method: "DELETE",
            uri: format!("/runtimes/{id}/"),
            body: None,
        },
        Addressed {
            operation: "ListAgentRuntimeVersions",
            method: "POST",
            uri: format!("/runtimes/{id}/versions/"),
            body: None,
        },
        Addressed {
            operation: "ListAgentRuntimeEndpoints",
            method: "POST",
            uri: format!("/runtimes/{id}/runtime-endpoints/"),
            body: None,
        },
        Addressed {
            operation: "CreateAgentRuntimeEndpoint",
            method: "PUT",
            uri: format!("/runtimes/{id}/runtime-endpoints/"),
            body: Some(json!({"name": "probeEndpoint"})),
        },
        Addressed {
            operation: "GetAgentRuntimeEndpoint",
            method: "GET",
            uri: format!("/runtimes/{id}/runtime-endpoints/probeEndpoint/"),
            body: None,
        },
        Addressed {
            operation: "UpdateAgentRuntimeEndpoint",
            method: "PUT",
            uri: format!("/runtimes/{id}/runtime-endpoints/probeEndpoint/"),
            body: Some(json!({"agentRuntimeVersion": "1"})),
        },
        Addressed {
            operation: "DeleteAgentRuntimeEndpoint",
            method: "DELETE",
            uri: format!("/runtimes/{id}/runtime-endpoints/probeEndpoint/"),
            body: None,
        },
    ]
}

/// Assert `answer` is the authorization refusal for `operation`, blaming `principal`.
fn assert_denied(answer: &Answer, case: &str, principal: &str, operation: &str) {
    assert_eq!(
        (answer.status, answer.code.as_str()),
        (403, ACCESS_DENIED),
        "case {case:?}: expected 403 {ACCESS_DENIED}, got {} {:?} with {:?}",
        answer.status,
        answer.code,
        answer.message
    );
    let expected = not_authorized(principal, operation);
    assert_eq!(
        answer.message, expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        answer.message
    );
}

/// RULE: whether the calling principal's identity-based documents allow
/// `bedrock-agentcore:<OperationName>` on the addressed runtime's ARN decides the request,
/// and a document that does not allow it is answered 403 `AccessDeniedException` naming that
/// principal and that action — whatever the state of the runtime it addressed.
#[tokio::test]
async fn a_document_that_does_not_allow_the_action_refuses_the_call() {
    let harness = Harness::new();
    let principal = harness.given_user();
    let held = harness.create_runtime("authorizedProbe").await;
    let other = harness.create_runtime("otherProbe").await;
    let held_arn = format!(
        "arn:aws:bedrock-agentcore:{REGION}:{}:runtime/{held}",
        engine::emulator_account_id()
    );
    let other_arn = format!(
        "arn:aws:bedrock-agentcore:{REGION}:{}:runtime/{other}",
        engine::emulator_account_id()
    );

    // (case, the document in force on the user, whether the call is allowed)
    let rows: Vec<(&str, Option<String>, bool)> = vec![
        ("no document at all: IAM's default deny", None, false),
        (
            "another service entirely",
            Some(allow(r#""bedrock:*""#, r#""*""#)),
            false,
        ),
        (
            "another operation of this namespace",
            Some(allow(r#""bedrock-agentcore:ListAgentRuntimes""#, r#""*""#)),
            false,
        ),
        (
            "exactly this action, over every resource",
            Some(allow(r#""bedrock-agentcore:GetAgentRuntime""#, r#""*""#)),
            true,
        ),
        (
            "the whole namespace, over every resource",
            Some(allow(r#""bedrock-agentcore:*""#, r#""*""#)),
            true,
        ),
        (
            "this action scoped to the runtime addressed",
            Some(allow(
                r#""bedrock-agentcore:GetAgentRuntime""#,
                &format!(r#""{held_arn}""#),
            )),
            true,
        ),
        (
            "this action scoped to ANOTHER runtime of the account",
            Some(allow(
                r#""bedrock-agentcore:GetAgentRuntime""#,
                &format!(r#""{other_arn}""#),
            )),
            false,
        ),
        (
            "an explicit deny of this action beside an allow of the namespace",
            Some(
                r#"{"Version":"2012-10-17","Statement":[
                 {"Effect":"Allow","Action":"bedrock-agentcore:*","Resource":"*"},
                 {"Effect":"Deny","Action":"bedrock-agentcore:GetAgentRuntime",
                   "Resource":"*"}]}"#
                    .to_string(),
            ),
            false,
        ),
    ];

    for (case, document, allowed) in rows {
        harness.given_policy(document.as_deref());
        let answer = harness
            .send_as(USER_KEY_ID, REGION, "GET", &format!("/runtimes/{held}/"), None)
            .await;
        if allowed {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: an allowed call reaches the operation, got {} {:?} with {:?}",
                answer.status, answer.code, answer.message
            );
        } else {
            assert_denied(&answer, case, &principal, "GetAgentRuntime");
        }
    }
}

/// RULE: the decision is taken against the ARN of the runtime the request addresses, so a
/// statement scoped to one Region's runtime covers that Region's and no other.
#[tokio::test]
async fn a_scoped_statement_covers_only_the_runtime_arn_addressed() {
    let harness = Harness::new();
    let principal = harness.given_user();
    let held = harness.create_runtime("regionScopedProbe").await;
    harness.given_policy(Some(&allow(
        r#""bedrock-agentcore:GetAgentRuntime""#,
        &format!(
            r#""arn:aws:bedrock-agentcore:{REGION}:{}:runtime/*""#,
            engine::emulator_account_id()
        ),
    )));

    let signed_for_the_region = harness
        .send_as(USER_KEY_ID, REGION, "GET", &format!("/runtimes/{held}/"), None)
        .await;
    assert_eq!(
        signed_for_the_region.status, 200,
        "case \"the Region the statement names\": expected the operation's own answer, got \
         {} {:?} with {:?}",
        signed_for_the_region.status,
        signed_for_the_region.code,
        signed_for_the_region.message
    );

    let signed_for_another_region = harness
        .send_as(
            USER_KEY_ID,
            OTHER_REGION,
            "GET",
            &format!("/runtimes/{held}/"),
            None,
        )
        .await;
    assert_denied(
        &signed_for_another_region,
        "another Region, which the statement does not name",
        &principal,
        "GetAgentRuntime",
    );
}

/// RULE's ORDER: the authorization decision is reached BEFORE the model's constraints are
/// validated, so a denied principal is refused 403 whatever else is wrong with the request —
/// an `agentRuntimeId` outside its member's `@pattern` included — and never hears the
/// constraint clause. The same identifier from a principal the policies DO allow is the
/// modelled `ValidationException`.
#[tokio::test]
async fn the_authorization_decision_precedes_the_constraint_check() {
    let harness = Harness::new();
    let principal = harness.given_user();
    harness.given_policy(Some(&allow(r#""bedrock:*""#, r#""*""#)));

    // (case, the identifier the request addresses its runtime by)
    let identifiers: [(&str, &str); 3] = [
        ("an identifier no runtime can carry", UNPATTERNED_RUNTIME_ID),
        (
            "an identifier inside the member's format that names nothing",
            ABSENT_RUNTIME_ID,
        ),
        ("an identifier with a leading digit", "9probe-abcdefghij"),
    ];
    for (case, id) in identifiers {
        let answer = harness
            .send_as(USER_KEY_ID, REGION, "GET", &format!("/runtimes/{id}/"), None)
            .await;
        assert_denied(&answer, case, &principal, "GetAgentRuntime");
        assert!(
            !answer.message.contains("failed to satisfy constraint")
                && !answer.message.contains(id),
            "case {case:?}: a denied caller hears no constraint clause and no echo of what \
             it sent, got {:?}",
            answer.message
        );
    }

    // The same two identifiers from a principal the policies allow: the operation's own
    // verdicts, which is where the modelled ValidationException and the modelled
    // ResourceNotFoundException live.
    harness.given_policy(Some(&allow(r#""bedrock-agentcore:*""#, r#""*""#)));
    let malformed = harness
        .send_as(
            USER_KEY_ID,
            REGION,
            "GET",
            &format!("/runtimes/{UNPATTERNED_RUNTIME_ID}/"),
            None,
        )
        .await;
    let case = "an identifier no runtime can carry, from an authorized principal";
    assert_eq!(
        (malformed.status, malformed.code.as_str()),
        (400, VALIDATION_EXCEPTION),
        "case {case:?}: expected 400 {VALIDATION_EXCEPTION}, got {} {:?} with {:?}",
        malformed.status,
        malformed.code,
        malformed.message
    );
    assert!(
        malformed.message.contains("agentRuntimeId"),
        "case {case:?}: expected the clause to name the member, got {:?}",
        malformed.message
    );

    let absent = harness
        .send_as(
            USER_KEY_ID,
            REGION,
            "GET",
            &format!("/runtimes/{ABSENT_RUNTIME_ID}/"),
            None,
        )
        .await;
    let case = "an addressable runtime the account does not hold, from an authorized principal";
    assert_eq!(
        (absent.status, absent.code.as_str()),
        (404, RESOURCE_NOT_FOUND),
        "case {case:?}: expected 404 {RESOURCE_NOT_FOUND}, got {} {:?} with {:?}",
        absent.status,
        absent.code,
        absent.message
    );
}

/// RULE over the whole class: every operation that addresses a runtime by
/// `{agentRuntimeId}` is judged, and the refusal names ITS own action — the operation's
/// shape name under this endpoint's namespace.
#[tokio::test]
async fn every_runtime_addressed_operation_is_judged_under_its_own_action() {
    let harness = Harness::new();
    let principal = harness.given_user();
    let held = harness.create_runtime("classProbe").await;
    harness.given_policy(Some(&allow(r#""bedrock:*""#, r#""*""#)));

    for addressed in addressing(&held) {
        let answer = harness
            .send_as(
                USER_KEY_ID,
                REGION,
                addressed.method,
                &addressed.uri,
                addressed.body.as_ref(),
            )
            .await;
        assert_denied(&answer, addressed.operation, &principal, addressed.operation);
    }
}

/// The two callers the decision cannot refuse: the account's own published credential, which
/// holds no IAM policy, and a principal carrying a policy attachment this emulator holds no
/// document for (an AWS-managed policy), which only IAM's default deny would have refused.
#[tokio::test]
async fn a_caller_this_gate_does_not_bound_reaches_the_operation() {
    let harness = Harness::new();
    harness.given_user();
    let held = harness.create_runtime("unboundedProbe").await;

    let ambient = harness.send("GET", &format!("/runtimes/{held}/"), None).await;
    assert_eq!(
        ambient.status, 200,
        "case \"the account's own published credential\": expected the operation's own \
         answer, got {} {:?} with {:?}",
        ambient.status, ambient.code, ambient.message
    );

    // An attachment whose document this emulator does not hold: an entry of AWS's own
    // catalog, which cannot be read and so cannot be the reason a call is refused.
    harness.given_attached_policy("arn:aws:iam::aws:policy/AmazonBedrockAgentCoreFullAccess");
    let unreadable = harness
        .send_as(USER_KEY_ID, REGION, "GET", &format!("/runtimes/{held}/"), None)
        .await;
    assert_eq!(
        unreadable.status, 200,
        "case \"a grant this emulator cannot read\": expected the operation's own answer, \
         got {} {:?} with {:?}",
        unreadable.status, unreadable.code, unreadable.message
    );
}
