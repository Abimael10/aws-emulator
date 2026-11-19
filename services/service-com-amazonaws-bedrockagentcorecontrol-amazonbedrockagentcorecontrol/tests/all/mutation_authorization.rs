// SPDX-License-Identifier: CC0-1.0

//! WHO may MUTATE or READ one resource of this control plane, driven end to end over the
//! wire path (a `restJson1` request through the multi-service router into the mounted
//! service), so what is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: every request this endpoint serves is decided by the calling
//! principal's identity-based IAM policies BEFORE the request's members are validated, and
//! the resource the decision is taken against is the one the route addresses — the family's
//! whole collection (its ARN with `*` in place of the identifier) for a route that names no
//! resource, which is every CREATE and every listing, and one member's own ARN for a route
//! that carries an identifier. A request no readable statement allows is answered HTTP 403
//! `AccessDeniedException` blaming the principal and the action
//! (`bedrock-agentcore:<OperationName>`), nothing of the account is disclosed and nothing is
//! written; a request a matching statement allows reaches the operation, which then judges
//! its input.
//!
//! `tests/all/collection_listing_authorization.rs` pins the same rule for the LISTINGS; this
//! suite is about the routes beside them — the creates, which address a family the caller
//! cannot name a resource of yet, and the reads, updates and deletes, which name one.
//!
//! The role, its inline policy and the assumed-role session live in the IAM and STS stores of
//! the SAME shared engine — the stores those services' own handlers write — so the documents
//! under test are the ones `iam:PutRolePolicy` stores, and the principal a refusal blames is
//! the one `sts:GetCallerIdentity` answers for the session the request signed with.
//!
//! The tables span what the rule generalises over: the resource FAMILY (six of them, each
//! with its own ARN type), the KIND of route (a create addressing the family, a read, an
//! update and a delete naming one member), the document in force (none, another namespace,
//! another operation of this namespace, this action exactly, the whole namespace, an explicit
//! deny beside an allow), and whether the identifier the route carries is one this account
//! holds, one it does not, or one outside the member's own modelled pattern.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;

/// The Region every row signs for.
const REGION: &str = "us-east-1";

/// An access key id neither store holds a record for: the account's own published
/// credential, which holds no IAM policy — the account owner.
const AMBIENT_KEY_ID: &str = "AKIDEXAMPLE";

/// The assumed-role session the rows sign with: its access key id, the role it is assumed
/// from and the session's own name. The role name appears nowhere in the key's text, so an
/// identity synthesised from that text cannot pass a row.
const SESSION_KEY_ID: &str = "ASIAEXAMPLEAGENTCORE3";
const ROLE: &str = "agentcore-mutator";
const SESSION: &str = "mutator-session";

/// The name of the inline policy the rows put in force on the role.
const POLICY_NAME: &str = "inline";

/// How long a seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// The error code `restJson1` carries in `__type` and in `x-amzn-errortype`, at the status
/// the model gives it (`AccessDeniedException`, `httpError` 403).
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

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

    /// Send one call signed with `access_key_id` for `REGION`.
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

    /// Record the role `name` and the assumed-role session `access_key_id` speaks for, as
    /// `iam:CreateRole` and `sts:AssumeRole` record them, and answer the session's principal
    /// ARN — what a refusal of its calls must name.
    fn given_session(&self, name: &str, session_name: &str, access_key_id: &str) -> String {
        let role_arn = role_arn(name);
        let iam_store = iam::service_state(&*self.engine).expect("the IAM store");
        iam_store.lock().roles.insert(
            iam::name_key(name),
            Role {
                path: "/".to_string(),
                role_name: name.to_string(),
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
        let principal = session_arn(name, session_name);
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

    /// Put `document` in force as the role's inline policy, replacing whatever it carried —
    /// what `iam:PutRolePolicy` does. `None` takes every policy off it.
    fn given_role_policy(&self, role: &str, document: Option<&str>) {
        let store = iam::service_state(&*self.engine).expect("the IAM store");
        let mut store = store.lock();
        let key = iam::name_key(role);
        store.role_inline_policies.remove(&key);
        if let Some(document) = document {
            store.role_inline_policies.insert(
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

    /// Create a gateway with the account's own credential and answer its `gatewayId`.
    async fn create_gateway(&self, name: &str) -> String {
        let body = json!({
            "name": name,
            "roleArn": "arn:aws:iam::000000000000:role/ProbeGatewayRole",
            "protocolType": "MCP",
            "authorizerType": "CUSTOM_JWT",
            "authorizerConfiguration": {"customJWTAuthorizer": {
                "discoveryUrl": "https://example.com/.well-known/openid-configuration",
                "allowedClients": ["probe-client"],
            }},
        });
        let answer = self
            .send_as(AMBIENT_KEY_ID, "POST", "/gateways/", Some(&body))
            .await;
        assert_eq!(
            answer.status, 202,
            "setup: creating the gateway {name:?} must succeed, got {} with {:?}",
            answer.status, answer.body
        );
        answer.body["gatewayId"]
            .as_str()
            .expect("a create answers the id it minted")
            .to_string()
    }
}

/// A SigV4 `Authorization` header for this service, signed with `access_key_id` in [`REGION`].
fn authorization(access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// The ARN the account holds the role under — the identity whose policies an assumed-role
/// session's request is judged on.
fn role_arn(role: &str) -> String {
    format!(
        "arn:aws:iam::{}:role/{role}",
        ::engine::emulator_account_id()
    )
}

/// The principal ARN a session of `role` named `session` speaks for.
fn session_arn(role: &str, session: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role}/{session}",
        ::engine::emulator_account_id()
    )
}

/// The collection a family addressed under the ARN type `resource_type` is judged against:
/// the family's ARN with `*` in place of the resource identifier.
fn collection(resource_type: &str) -> String {
    member(resource_type, "*")
}

/// The ARN of ONE member of the family addressed under the ARN type `resource_type`.
fn member(resource_type: &str, id: &str) -> String {
    format!(
        "arn:aws:bedrock-agentcore:{REGION}:{}:{resource_type}/{id}",
        ::engine::emulator_account_id()
    )
}

/// The IAM action an operation is performed under: this endpoint's own namespace
/// (`sigv4Name` / `awsService.arnNamespace`, `smithy_query(query="service_info")`).
fn action_for(operation: &str) -> String {
    format!("bedrock-agentcore:{operation}")
}

/// The reason clause of a call no readable statement allows — IAM's default deny.
fn no_policy_allows(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The reason clause of a call an explicit `Deny` in the principal's own document covers.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The sentence a refused call of a route addressing a family's COLLECTION states: the
/// principal, the action, that collection and the rule that withheld the grant.
fn not_authorized_on(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// The sentence a refused call of a route naming ONE MEMBER states. It closes at the action:
/// a member's ARN embeds an identifier the caller carried and the gate does not validate.
fn not_authorized(principal: &str, action: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action}")
}

/// A policy document granting `actions` on `resources`, as `iam:PutRolePolicy` takes one.
fn allow(actions: &str, resources: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[
         {{"Effect":"Allow","Action":{actions},"Resource":{resources}}}]}}"#
    )
}

/// One route under test: the operation, its modelled route and the ARN type of the family it
/// addresses.
struct Route {
    operation: &'static str,
    method: &'static str,
    uri: &'static str,
    /// The resource type of the family's ARN.
    resource_type: &'static str,
    /// A body for a route whose input members are bound to the payload.
    body: bool,
}

/// Every CREATE of this endpoint whose family this layer names one ARN for. A create mints a
/// resource the request cannot name yet, so each is judged against its family's COLLECTION.
///
/// Source: the routes are this crate's `OPERATIONS.md` (the `@http` bindings `smithy_query`
/// reports) and the ARN types are each family's own `*Arn` pattern (`AgentRuntimeArn`
/// `runtime/…`, `GatewayArn` `gateway/…`, `MemoryArn` `memory/…`, `HarnessArn` `harness/…`,
/// `BrowserProfileArn` `browser-profile/…`, `PolicyEngineArn` `policy-engine/…`).
const CREATES: &[Route] = &[
    Route {
        operation: "CreateAgentRuntime",
        method: "PUT",
        uri: "/runtimes/",
        resource_type: "runtime",
        body: true,
    },
    Route {
        operation: "CreateGateway",
        method: "POST",
        uri: "/gateways/",
        resource_type: "gateway",
        body: true,
    },
    Route {
        operation: "CreateMemory",
        method: "POST",
        uri: "/memories/create",
        resource_type: "memory",
        body: true,
    },
    Route {
        operation: "CreateHarness",
        method: "POST",
        uri: "/harnesses",
        resource_type: "harness",
        body: true,
    },
    Route {
        operation: "CreateBrowserProfile",
        method: "PUT",
        uri: "/browser-profiles",
        resource_type: "browser-profile",
        body: true,
    },
    Route {
        operation: "CreatePolicyEngine",
        method: "POST",
        uri: "/policy-engines",
        resource_type: "policy-engine",
        body: true,
    },
];

/// Drive `route` as the session `access_key_id` speaks for. The body is EMPTY on purpose:
/// the authorization decision precedes the constraint walk, so a refused row cannot be an
/// artefact of the body and an allowed row reaches the operation, which judges it.
async fn call_as(harness: &Harness, access_key_id: &str, route: &Route, uri: &str) -> Answer {
    let body = json!({});
    harness
        .send_as(
            access_key_id,
            route.method,
            uri,
            route.body.then_some(&body),
        )
        .await
}

/// Assert `answer` is NOT an authorization refusal: the call got past the gate and was
/// answered by the operation (which, for an empty body, states what the input is missing).
fn assert_not_denied(answer: &Answer, case: &str) {
    assert_ne!(
        (answer.status, answer.code.as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case {case:?}: an allowed call reaches the operation rather than the gate, got {} \
         {:?} with {:?}",
        answer.status,
        answer.code,
        answer.message
    );
}

/// RULE: a CREATE addresses a family the caller cannot name a resource of yet, so it is
/// judged against the family's collection ARN, and one no readable statement allows is
/// refused 403 `AccessDeniedException` naming the principal, the action, that collection and
/// the rule that withheld the grant — with nothing minted.
#[tokio::test]
async fn a_create_is_judged_against_its_family_collection() {
    let harness = Harness::new();
    let principal = harness.given_session(ROLE, SESSION, SESSION_KEY_ID);

    for route in CREATES {
        let action = action_for(route.operation);
        let resource = collection(route.resource_type);

        // (case, the document in force on the role, the refusal expected — `None` when the
        // call is allowed through to the operation)
        let rows: Vec<(&str, Option<String>, Option<String>)> = vec![
            (
                "no document at all: IAM's default deny",
                None,
                Some(no_policy_allows(&action)),
            ),
            (
                "another action namespace entirely",
                Some(allow(r#""bedrock-agent:CreateAgent""#, r#""*""#)),
                Some(no_policy_allows(&action)),
            ),
            (
                "another operation of this namespace",
                Some(allow(r#""bedrock-agentcore:GetAgentRuntime""#, r#""*""#)),
                Some(no_policy_allows(&action)),
            ),
            (
                "exactly this action, over every resource",
                Some(allow(&format!(r#""{action}""#), r#""*""#)),
                None,
            ),
            (
                "this action scoped to the family's collection",
                Some(allow(
                    &format!(r#""{action}""#),
                    &format!(r#""{resource}""#),
                )),
                None,
            ),
            (
                "the whole namespace, over every resource",
                Some(allow(r#""bedrock-agentcore:*""#, r#""*""#)),
                None,
            ),
            (
                "an explicit deny of this action beside an allow of the namespace",
                Some(format!(
                    r#"{{"Version":"2012-10-17","Statement":[
                     {{"Effect":"Allow","Action":"bedrock-agentcore:*","Resource":"*"}},
                     {{"Effect":"Deny","Action":"{action}","Resource":"*"}}]}}"#
                )),
                Some(EXPLICIT_DENY.to_string()),
            ),
        ];

        for (row, document, refusal) in rows {
            let case = format!("{} — {row}", route.operation);
            harness.given_role_policy(ROLE, document.as_deref());
            let answer = call_as(&harness, SESSION_KEY_ID, route, route.uri).await;
            match refusal {
                None => assert_not_denied(&answer, &case),
                Some(reason) => {
                    assert_eq!(
                        (answer.status, answer.code.as_str()),
                        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} \
                         {:?} with {:?}",
                        answer.status,
                        answer.code,
                        answer.message
                    );
                    let expected = not_authorized_on(&principal, &action, &resource, &reason);
                    assert_eq!(
                        answer.message, expected,
                        "case {case:?}: expected {expected:?}, got {:?}",
                        answer.message
                    );
                }
            }
        }
    }

    // Nothing the denied rows asked for was minted: the account's own credential lists an
    // empty runtime family, which the refusals above were not allowed to grow.
    harness.given_role_policy(ROLE, None);
    let listed = harness
        .send_as(AMBIENT_KEY_ID, "POST", "/runtimes/", Some(&json!({})))
        .await;
    assert_eq!(
        (listed.status, listed.body["agentRuntimes"].clone()),
        (200, json!([])),
        "a refused create mints nothing, got {} with {:?}",
        listed.status,
        listed.body
    );
}

/// RULE: a route that NAMES one resource is judged against that resource's own ARN, and the
/// decision precedes the constraint walk — so a call no statement allows is refused 403
/// `AccessDeniedException` whether the identifier names a resource of the account, names
/// none, or is outside the member's own modelled pattern. The sentence is the backend's own:
/// every family but the runtime one states the member's ARN and the rule that withheld the
/// grant, and the runtime family closes at the action.
#[tokio::test]
async fn a_call_naming_one_resource_is_judged_before_its_input_is_validated() {
    let harness = Harness::new();
    let principal = harness.given_session(ROLE, SESSION, SESSION_KEY_ID);
    let gateway_id = harness.create_gateway("heldgateway").await;

    // (case, operation, method, the URI the row addresses, the resource ARN the refusal
    // states — `None` for the family whose sentence closes at the action). The identifiers
    // span one the account HOLDS, one inside `GatewayIdentifier`'s pattern
    // (`^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$`) that names nothing, one OUTSIDE it — which
    // the constraint walk would refuse 400 if it ran first — and members of three other
    // families, so the rule is not about gateways.
    let rows: Vec<(String, &str, &str, String, Option<String>)> = vec![
        (
            "DeleteGateway on a gateway this account holds".to_string(),
            "DeleteGateway",
            "DELETE",
            format!("/gateways/{gateway_id}/"),
            Some(member("gateway", &gateway_id)),
        ),
        (
            "DeleteGateway on an id inside the pattern that names nothing".to_string(),
            "DeleteGateway",
            "DELETE",
            "/gateways/absentgateway-abcdefghij/".to_string(),
            Some(member("gateway", "absentgateway-abcdefghij")),
        ),
        (
            "DeleteGateway on an id outside the member's pattern".to_string(),
            "DeleteGateway",
            "DELETE",
            "/gateways/NOT_AN_ID/".to_string(),
            Some(member("gateway", "NOT_AN_ID")),
        ),
        (
            "GetGateway on a gateway this account holds".to_string(),
            "GetGateway",
            "GET",
            format!("/gateways/{gateway_id}/"),
            Some(member("gateway", &gateway_id)),
        ),
        (
            "GetAgentRuntime on an id that names nothing".to_string(),
            "GetAgentRuntime",
            "GET",
            "/runtimes/absent_runtime-abcdefghij/".to_string(),
            None,
        ),
        (
            "GetMemory on an id that names nothing".to_string(),
            "GetMemory",
            "GET",
            "/memories/absentmemory-abcdefghij/details".to_string(),
            Some(member("memory", "absentmemory-abcdefghij")),
        ),
        (
            "DeleteHarness on an id that names nothing".to_string(),
            "DeleteHarness",
            "DELETE",
            "/harnesses/absentHarness-abcdefghij".to_string(),
            Some(member("harness", "absentHarness-abcdefghij")),
        ),
    ];

    for (case, operation, method, uri, resource) in &rows {
        harness.given_role_policy(ROLE, None);
        let answer = harness.send_as(SESSION_KEY_ID, method, uri, None).await;
        assert_eq!(
            (answer.status, answer.code.as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?} with \
             {:?}",
            answer.status,
            answer.code,
            answer.message
        );
        let action = action_for(operation);
        let expected = match resource {
            None => not_authorized(&principal, &action),
            Some(resource) => {
                not_authorized_on(&principal, &action, resource, &no_policy_allows(&action))
            }
        };
        assert_eq!(
            answer.message, expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            answer.message
        );
    }

    // The gateway the denied deletes addressed is untouched: the account's own credential
    // still reads it.
    let read = harness
        .send_as(AMBIENT_KEY_ID, "GET", &format!("/gateways/{gateway_id}/"), None)
        .await;
    assert_eq!(
        (read.status, read.body["gatewayId"].clone()),
        (200, json!(gateway_id)),
        "a denied delete leaves the gateway exactly as it was, got {} with {:?}",
        read.status,
        read.body
    );

    // And a statement that DOES allow the action on that gateway's own ARN lets the same
    // call through to the operation.
    let gateway_arn = member("gateway", &gateway_id);
    harness.given_role_policy(
        ROLE,
        Some(&allow(
            r#""bedrock-agentcore:GetGateway""#,
            &format!(r#""{gateway_arn}""#),
        )),
    );
    let allowed = harness
        .send_as(SESSION_KEY_ID, "GET", &format!("/gateways/{gateway_id}/"), None)
        .await;
    assert_eq!(
        (allowed.status, allowed.body["gatewayId"].clone()),
        (200, json!(gateway_id)),
        "a statement scoped to the gateway's own ARN allows the read, got {} with {:?}",
        allowed.status,
        allowed.body
    );

    // A statement scoped to ANOTHER gateway of the same family does not.
    let other_arn = member("gateway", "othergateway-abcdefghij");
    harness.given_role_policy(
        ROLE,
        Some(&allow(
            r#""bedrock-agentcore:GetGateway""#,
            &format!(r#""{other_arn}""#),
        )),
    );
    let refused = harness
        .send_as(SESSION_KEY_ID, "GET", &format!("/gateways/{gateway_id}/"), None)
        .await;
    assert_eq!(
        (refused.status, refused.code.as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "a statement scoped to another gateway grants nothing here, got {} {:?} with {:?}",
        refused.status,
        refused.code,
        refused.message
    );
}

/// RULE: the account's own published credential holds no IAM policy, so it is not a
/// principal this gate bounds — every call it signs reaches the operation.
#[tokio::test]
async fn the_accounts_own_credential_is_not_bounded_by_this_gate() {
    let harness = Harness::new();
    harness.given_session(ROLE, SESSION, SESSION_KEY_ID);
    harness.given_role_policy(ROLE, None);

    for route in CREATES {
        let case = format!("{} signed with the account's own credential", route.operation);
        let answer = call_as(&harness, AMBIENT_KEY_ID, route, route.uri).await;
        assert_not_denied(&answer, &case);
    }
}
