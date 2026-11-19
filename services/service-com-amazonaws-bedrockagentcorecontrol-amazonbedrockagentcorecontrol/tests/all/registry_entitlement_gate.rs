// SPDX-License-Identifier: CC0-1.0

//! WHETHER THE ACCOUNT MAY USE THE AGENTCORE REGISTRY AT ALL, driven end to end over the
//! wire path (a `restJson1` request through the multi-service router into the mounted
//! service), so what is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: the registry plane is an entitled feature. An account that does not
//! hold the AgentCore Registry entitlement is refused EVERY registry call — every modelled
//! operation bound under `/registries` (`OPERATIONS.md`) — with HTTP 403
//! `AccessDeniedException` and the sentence this endpoint denies a call in: the principal
//! that signed, as `sts:GetCallerIdentity` reports it, and the action
//! `bedrock-agentcore:<OperationName>` the route's own operation is performed under. The
//! refusal is synchronous and identical on every attempt, it records nothing, and it is
//! scoped to that plane: the same credentials keep reaching every other family of this
//! endpoint. An account that DOES hold the entitlement — granted out of band, as AWS grants
//! it, through this service's own state seam — reaches the plane and round-trips a registry.
//!
//! The tables span what the rule generalises over: the registry OPERATION (all twelve, so
//! each action is read off the route rather than from one cited call), the depth of the route
//! (the collection, one registry, a record, a sub-action on a record), the HTTP method, the
//! number of attempts, and the kind of PRINCIPAL the refusal blames (the account's own
//! published credential and an assumed-role session, whose ARNs differ in every segment).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::state as agentcore;
use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;

/// The Region every row signs for.
const REGION: &str = "us-east-1";

/// An access key id neither identity store holds a record for: the account's own published
/// credential, whose principal is the one `sts:GetCallerIdentity` answers for it.
const AMBIENT_KEY_ID: &str = "AKIDEXAMPLE";

/// The assumed-role session the principal rows sign with. The role name appears nowhere in
/// the key's text, so a principal synthesised from that text cannot pass a row.
const SESSION_KEY_ID: &str = "ASIAEXAMPLEREGISTRY01";
const ROLE: &str = "agentcore-registry-team";
const SESSION: &str = "registry-session";

/// How long a seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// The error code `restJson1` carries in `__type` and in `x-amzn-errortype`, at the status
/// the model gives it (`AccessDeniedException`, `httpError` 403).
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// A `registryId` of `RegistryId`'s shape (`^[a-zA-Z0-9]{12,16}$`) and a `recordId` of
/// `RegistryRecordId`'s (`^[a-zA-Z0-9]{12}$`), for the path labels the rows address. Neither
/// names a stored resource: a refusal that precedes the handler cannot depend on one.
const REGISTRY_ID: &str = "aaaaaaaaaaaa";
const RECORD_ID: &str = "bbbbbbbbbbbb";

/// A `name` inside `RegistryName`'s constraints (len 1..64,
/// `^[a-zA-Z0-9][a-zA-Z0-9_\-\.\/]*$`, `smithy_query operation=CreateRegistry`).
const REGISTRY_NAME: &str = "entitlement-probe";

/// One answer: the status, the error code, the `message` and the whole body.
struct Answer {
    status: u16,
    code: String,
    message: String,
    body: Value,
}

/// One router over one engine: the control plane under test, plus the IAM and STS stores of
/// that same engine, which this suite seeds directly.
struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::default();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Record that this account holds the AgentCore Registry entitlement, as AWS granting it
    /// out of band does — the only thing that gives an account one.
    fn given_registry_entitlement(&self) {
        assert!(
            agentcore::grant_registry_entitlement(&*self.engine),
            "setup: the shared engine must carry this service's store"
        );
    }

    /// Send one call signed with `access_key_id`.
    async fn send_as(
        &self,
        access_key_id: &str,
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
            // The SigV4 credential scope routes the request, names its Region and names the
            // identity it speaks for; the signature itself is not checked here.
            .header("authorization", authorization(access_key_id))
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

    /// Send one call with the account's own published credential.
    async fn send(&self, method: &str, uri: &str, body: Option<&Value>) -> Answer {
        self.send_as(AMBIENT_KEY_ID, method, uri, body).await
    }

    /// Record the role `ROLE` and the assumed-role session `access_key_id` speaks for, as
    /// `iam:CreateRole` and `sts:AssumeRole` record them, and answer the session's principal
    /// ARN — what a refusal of its calls must name.
    fn given_session(&self, access_key_id: &str) -> String {
        let role_arn = format!(
            "arn:aws:iam::{}:role/{ROLE}",
            ::engine::emulator_account_id()
        );
        let iam_store = iam::service_state(&*self.engine).expect("the IAM store");
        iam_store.lock().roles.insert(
            iam::name_key(ROLE),
            Role {
                path: "/".to_string(),
                role_name: ROLE.to_string(),
                role_id: sts::role_id(&role_arn),
                arn: role_arn.clone(),
                create_date: ::aws_smithy_types::DateTime::from_secs(0),
                assume_role_policy_document: None,
                description: None,
                max_session_duration: None,
                permissions_boundary: None,
                tags: None,
                role_last_used: None,
                source_role_template: None,
            },
        );
        // The session's own policies allow every action of this namespace, so nothing the IAM
        // gate decides can be what refuses the calls below.
        iam_store.lock().role_inline_policies.insert(
            iam::name_key(ROLE),
            BTreeMap::from([(
                iam::name_key("inline"),
                iam::InlinePolicy {
                    policy_name: "inline".to_string(),
                    policy_document: r#"{"Version":"2012-10-17","Statement":[
                     {"Effect":"Allow","Action":"bedrock-agentcore:*","Resource":"*"}]}"#
                        .to_string(),
                },
            )]),
        );
        let principal = format!(
            "arn:aws:sts::{}:assumed-role/{ROLE}/{SESSION}",
            ::engine::emulator_account_id()
        );
        let vend = sts::NewSession {
            role_arn: Some(role_arn.clone()),
            ..sts::NewSession::new(
                sts::Identity {
                    user_id: sts::role_id(&role_arn),
                    account: ::engine::emulator_account_id().to_string(),
                    arn: principal.clone(),
                },
                sts::SessionKind::AssumedRole,
                SESSION_SECONDS,
            )
        };
        let store = sts::service_state(&*self.engine).expect("the STS store");
        store
            .lock()
            .insert(vend.into_session(access_key_id, SystemTime::now()))
            .expect("the session is recorded");
        principal
    }
}

/// A SigV4 `Authorization` header for this service, signed with `access_key_id`.
fn authorization(access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// The principal the account's own published credential speaks for, as
/// `sts:GetCallerIdentity` reports it.
fn ambient_principal() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// The sentence this endpoint denies a call in: the principal that signed and the action it
/// asked for, and nothing further.
fn not_authorized(principal: &str, operation: &str) -> String {
    format!("User: {principal} is not authorized to perform: bedrock-agentcore:{operation}")
}

/// One registry-plane call: the operation its route names, the route, and a body for the
/// routes that take one.
struct RegistryCall {
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
}

/// Every modelled operation of the registry plane, each with the route it is bound to
/// (`OPERATIONS.md`: `/registries`, the three routes on `/registries/{registryId}` and the
/// seven record routes nested under it). The bodies carry only what each route's `@required`
/// members are, so a request that reached validation would pass it — what is asserted below
/// is therefore the entitlement decision and not a constraint.
fn registry_calls() -> Vec<RegistryCall> {
    let records = format!("/registries/{REGISTRY_ID}/records");
    let record = format!("{records}/{RECORD_ID}");
    vec![
        RegistryCall {
            operation: "CreateRegistry",
            method: "POST",
            uri: "/registries".to_string(),
            body: Some(json!({"name": REGISTRY_NAME})),
        },
        RegistryCall {
            operation: "ListRegistries",
            method: "GET",
            uri: "/registries".to_string(),
            body: None,
        },
        RegistryCall {
            operation: "GetRegistry",
            method: "GET",
            uri: format!("/registries/{REGISTRY_ID}"),
            body: None,
        },
        RegistryCall {
            operation: "UpdateRegistry",
            method: "PATCH",
            uri: format!("/registries/{REGISTRY_ID}"),
            body: Some(json!({"name": REGISTRY_NAME})),
        },
        RegistryCall {
            operation: "DeleteRegistry",
            method: "DELETE",
            uri: format!("/registries/{REGISTRY_ID}"),
            body: None,
        },
        RegistryCall {
            operation: "CreateRegistryRecord",
            method: "POST",
            uri: records.clone(),
            body: Some(json!({"name": "probe-record", "descriptorType": "CUSTOM"})),
        },
        RegistryCall {
            operation: "ListRegistryRecords",
            method: "GET",
            uri: records,
            body: None,
        },
        RegistryCall {
            operation: "GetRegistryRecord",
            method: "GET",
            uri: record.clone(),
            body: None,
        },
        RegistryCall {
            operation: "UpdateRegistryRecord",
            method: "PATCH",
            uri: record.clone(),
            body: Some(json!({"name": "probe-record"})),
        },
        RegistryCall {
            operation: "DeleteRegistryRecord",
            method: "DELETE",
            uri: record.clone(),
            body: None,
        },
        RegistryCall {
            operation: "SubmitRegistryRecordForApproval",
            method: "POST",
            uri: format!("{record}/submit-for-approval"),
            body: None,
        },
        RegistryCall {
            operation: "UpdateRegistryRecordStatus",
            method: "PATCH",
            uri: format!("{record}/status"),
            body: Some(json!({"status": "APPROVED", "statusReason": "probe"})),
        },
    ]
}

/// Assert `answer` is the entitlement refusal of `operation`, blaming `principal`.
fn assert_refused(answer: &Answer, case: &str, principal: &str, operation: &str) {
    let expected = not_authorized(principal, operation);
    assert_eq!(
        (
            answer.status,
            answer.code.as_str(),
            answer.message.as_str()
        ),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED, expected.as_str()),
        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED} {expected:?}, got {} \
         {:?} with {:?}",
        answer.status,
        answer.code,
        answer.body
    );
}

/// RULE: every call of the registry plane made by an account without the entitlement is
/// refused 403 `AccessDeniedException`, naming the signing principal and the action the
/// ROUTE's own operation is performed under.
#[tokio::test]
async fn every_registry_call_of_an_unentitled_account_is_refused() {
    let harness = Harness::new();
    let principal = ambient_principal();

    for call in registry_calls() {
        let answer = harness
            .send(call.method, &call.uri, call.body.as_ref())
            .await;
        assert_refused(&answer, call.operation, &principal, call.operation);
        // A refusal that precedes every handler discloses nothing of the plane: no registry,
        // no record and no listing comes back with it.
        for member in ["registryArn", "registries", "recordArn", "registryRecords"] {
            assert!(
                answer.body.get(member).is_none(),
                "case {:?}: a refused call reports no {member}, got {:?}",
                call.operation,
                answer.body
            );
        }
    }
}

/// RULE: the refusal is deterministic — the same call refused the same way on every attempt,
/// never a first success and never a transient failure — and it records nothing, which the
/// entitled account's own listing then shows.
#[tokio::test]
async fn the_refusal_is_identical_on_every_attempt_and_records_nothing() {
    let harness = Harness::new();
    let principal = ambient_principal();
    let body = json!({"name": REGISTRY_NAME, "description": "probing the entitlement wall"});

    let mut answered: Vec<(u16, String, String)> = Vec::new();
    for attempt in 1..=3 {
        let answer = harness.send("POST", "/registries", Some(&body)).await;
        assert_refused(
            &answer,
            &format!("CreateRegistry attempt {attempt}"),
            &principal,
            "CreateRegistry",
        );
        answered.push((answer.status, answer.code.clone(), answer.message.clone()));
    }
    let first = answered[0].clone();
    for (attempt, seen) in answered.iter().enumerate() {
        assert_eq!(
            *seen, first,
            "case \"attempt {}\": every attempt is refused identically, got {seen:?} against \
             {first:?}",
            attempt + 1
        );
    }

    // Nothing was recorded by any of them: once the account holds the entitlement, the
    // IDENTICAL request is served — the name is free, which it would not be had any refused
    // attempt minted a registry under it ("The name must be unique within your account", so a
    // second registry of that name is a `ConflictException`).
    harness.given_registry_entitlement();
    let created = harness.send("POST", "/registries", Some(&body)).await;
    assert_eq!(
        created.status, 202,
        "case \"the identical create once the account is entitled\": expected HTTP 202, so no \
         refused attempt had recorded the name, got {} with {:?}",
        created.status, created.body
    );
}

/// RULE: the sentence's one variable part is the SIGNING PRINCIPAL, so two callers of the
/// same unentitled account are each blamed by their own principal ARN — never a literal.
#[tokio::test]
async fn each_caller_is_blamed_by_its_own_principal() {
    let harness = Harness::new();
    let session_principal = harness.given_session(SESSION_KEY_ID);

    // (case, the access key id signing, the principal the refusal must name)
    let callers = [
        (
            "the account's own published credential",
            AMBIENT_KEY_ID,
            ambient_principal(),
        ),
        (
            "an assumed-role session allowed every action of this namespace",
            SESSION_KEY_ID,
            session_principal,
        ),
    ];
    for (case, access_key_id, principal) in callers {
        // Two calls of the plane, differing in operation, method and whether they carry a
        // body: the principal is the caller's in each, and the action is the route's own.
        for call in registry_calls()
            .into_iter()
            .filter(|call| matches!(call.operation, "CreateRegistry" | "ListRegistries"))
        {
            let answer = harness
                .send_as(access_key_id, call.method, &call.uri, call.body.as_ref())
                .await;
            assert_refused(
                &answer,
                &format!("{case} calling {}", call.operation),
                &principal,
                call.operation,
            );
        }
    }
}

/// RULE's SCOPE: the wall is the registry plane's. The same credentials that are refused
/// there keep reaching every other family of this endpoint.
#[tokio::test]
async fn the_refusal_is_scoped_to_the_registry_plane() {
    let harness = Harness::new();

    let refused = harness
        .send("POST", "/registries", Some(&json!({"name": REGISTRY_NAME})))
        .await;
    assert_refused(
        &refused,
        "the registry create of an unentitled account",
        &ambient_principal(),
        "CreateRegistry",
    );

    // (case, method, uri, body, the status the call answers)
    let gateways = harness.send("GET", "/gateways/", None).await;
    assert_eq!(
        gateways.status, 200,
        "case \"ListGateways with the same credentials\": expected HTTP 200, got {} with {:?}",
        gateways.status, gateways.body
    );
    let browsers = harness.send("POST", "/browsers", Some(&json!({}))).await;
    assert_eq!(
        browsers.status, 200,
        "case \"ListBrowsers with the same credentials\": expected HTTP 200, got {} with {:?}",
        browsers.status, browsers.body
    );

    // And a write of another plane round-trips: the workload identity is created and read
    // back with those same credentials.
    let created = harness
        .send(
            "POST",
            "/identities/CreateWorkloadIdentity",
            Some(&json!({"name": "registry-probe-identity"})),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "case \"CreateWorkloadIdentity with the same credentials\": expected HTTP 201, got {} \
         with {:?}",
        created.status, created.body
    );
    let read_back = harness
        .send(
            "POST",
            "/identities/GetWorkloadIdentity",
            Some(&json!({"name": "registry-probe-identity"})),
        )
        .await;
    assert_eq!(
        (read_back.status, &read_back.body["workloadIdentityArn"]),
        (200, &created.body["workloadIdentityArn"]),
        "case \"the workload identity reads back\": expected the created ARN, got {} with {:?}",
        read_back.status,
        read_back.body
    );
}

/// RULE's other side: an account that HOLDS the entitlement reaches the plane — the identical
/// request that was refused is served, and the registry it mints reads back.
#[tokio::test]
async fn an_entitled_account_reaches_the_registry_plane() {
    let harness = Harness::new();
    let body = json!({"name": REGISTRY_NAME});

    let refused = harness.send("POST", "/registries", Some(&body)).await;
    assert_refused(
        &refused,
        "before the entitlement is granted",
        &ambient_principal(),
        "CreateRegistry",
    );

    harness.given_registry_entitlement();
    let created = harness.send("POST", "/registries", Some(&body)).await;
    assert_eq!(
        created.status, 202,
        "case \"the identical request once the account is entitled\": expected HTTP 202, got \
         {} with {:?}",
        created.status, created.body
    );
    let arn = created.body["registryArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let id = arn.rsplit('/').next().unwrap_or_default().to_string();
    assert!(
        !id.is_empty(),
        "case \"the created registry's ARN\": expected an id in {arn:?}"
    );

    let read_back = harness.send("GET", &format!("/registries/{id}"), None).await;
    assert_eq!(
        (read_back.status, read_back.body["registryArn"].as_str()),
        (200, Some(arn.as_str())),
        "case \"the entitled account reads its registry back\": expected the created ARN, got \
         {} with {:?}",
        read_back.status,
        read_back.body
    );
    assert_eq!(
        read_back.body["name"].as_str(),
        Some(REGISTRY_NAME),
        "case \"the entitled account reads its registry back\": expected the submitted name, \
         got {:?}",
        read_back.body
    );
}
