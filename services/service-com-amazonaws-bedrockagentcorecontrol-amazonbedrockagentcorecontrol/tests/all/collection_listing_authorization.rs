// SPDX-License-Identifier: CC0-1.0

//! WHO may LIST a resource family of this control plane, driven end to end over the wire
//! path (a `restJson1` request through the multi-service router into the mounted service),
//! so what is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: a listing addresses no single resource of its own, so it is
//! authorized against its family's WHOLE COLLECTION — the family's ARN with `*` in place of
//! the resource identifier, in this account and the signed Region — for the action
//! `bedrock-agentcore:<OperationName>`. A listing the calling principal's identity-based
//! policies do not allow is answered HTTP 403 `AccessDeniedException` naming the principal,
//! the action, that collection ARN and which of IAM's rules withheld the grant, and nothing
//! of the family is disclosed; a listing a matching statement allows reaches the operation
//! and lists what the account owner lists.
//!
//! The role, its inline policy and the assumed-role session live in the IAM and STS stores of
//! the SAME shared engine — the stores those services' own handlers write — so the documents
//! under test are the ones `iam:PutRolePolicy` stores, and the principal a refusal blames is
//! the one `sts:GetCallerIdentity` answers for the session the request signed with.
//!
//! The tables span what the rule generalises over: the resource FAMILY listed (nine of them,
//! each with its own ARN type), the document in force (none, another action namespace,
//! another operation of this namespace, this action exactly, the whole namespace, the
//! collection ARN itself, one member of the family, an explicit deny beside an allow), the
//! Region the statement is scoped to, and the kind of principal (an assumed-role session, the
//! account's own published credential).

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

/// The Region every row signs for, and a second one for the rows about a statement's scope.
pub(crate) const REGION: &str = "us-east-1";
const OTHER_REGION: &str = "eu-west-3";

/// An access key id neither store holds a record for: the account's own published
/// credential, which holds no IAM policy — the account owner of the recorded check.
pub(crate) const AMBIENT_KEY_ID: &str = "AKIDEXAMPLE";

/// The assumed-role session the policy rows sign with: its access key id, the role it is
/// assumed from and the session's own name. The role name appears nowhere in the key's text,
/// so an identity synthesised from that text cannot pass a row.
pub(crate) const SESSION_KEY_ID: &str = "ASIAEXAMPLEAGENTCORE1";
pub(crate) const ROLE: &str = "agentcore-team";
pub(crate) const SESSION: &str = "team-session";

/// The TWIN session: the same policy plus one added allow of the listing under test, which
/// is the only variable between the two.
pub(crate) const TWIN_KEY_ID: &str = "ASIAEXAMPLEAGENTCORE2";
pub(crate) const TWIN_ROLE: &str = "agentcore-team-twin";

/// The name of the inline policy the rows put in force on a role.
const POLICY_NAME: &str = "inline";

/// How long a seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// The error code `restJson1` carries in `__type` and in `x-amzn-errortype`, at the status
/// the model gives it (`AccessDeniedException`, `httpError` 403).
pub(crate) const ACCESS_DENIED: &str = "AccessDeniedException";
pub(crate) const ACCESS_DENIED_STATUS: u16 = 403;

/// A `roleArn` inside `RoleArn`'s pattern, for the runtime the disclosure row creates.
const VALID_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/ProbeRuntimeRole";

/// One answer: the status, the error code, the `message` and the whole body.
///
/// Visible to the crate's other suites, which read the same four things out of an answer:
/// `tests/all/unnamed_resource_authorization.rs` drives the calls whose RESOURCE this
/// endpoint names no ARN for through this same harness rather than a second copy of it.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) body: Value,
}

/// One router over one engine: the control plane under test, plus the IAM and STS stores of
/// that same engine, which this suite seeds directly.
pub(crate) struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Harness {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // One family in the table below — the registry — is an ENTITLED plane, refused
        // before any handler runs for an account that does not hold the AgentCore Registry
        // entitlement (`tests/all/registry_entitlement_gate.rs` pins that rule). This suite
        // is about what IAM decides once the account may use the feature, so the account
        // holds the entitlement here, granted out of band as AWS grants it.
        assert!(
            agentcore::grant_registry_entitlement(&*engine),
            "setup: the shared engine must carry this service's store"
        );
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
            // The SigV4 credential scope routes the request, names its Region and names the
            // identity it speaks for; the signature itself is not checked here.
            .header("authorization", authorization(access_key_id, region))
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

    /// Create a runtime named `name` with the account's own credential, and answer the
    /// `agentRuntimeId` the service minted for it.
    async fn create_runtime(&self, name: &str) -> String {
        let body = json!({
            "agentRuntimeName": name,
            "agentRuntimeArtifact": {"containerConfiguration": {
                "containerUri": "public.ecr.aws/agentcore/probe:v1"
            }},
            "networkConfiguration": {"networkMode": "PUBLIC"},
            "roleArn": VALID_ROLE_ARN,
        });
        let answer = self
            .send_as(AMBIENT_KEY_ID, REGION, "PUT", "/runtimes/", Some(&body))
            .await;
        assert_eq!(
            answer.status, 202,
            "case \"the setup create of {name}\": expected 202, got {} with {:?}",
            answer.status, answer.body
        );
        answer.body["agentRuntimeId"]
            .as_str()
            .expect("a create answers the id it minted")
            .to_string()
    }

    /// Record the role `name` and the assumed-role session `access_key_id` speaks for, as
    /// `iam:CreateRole` and `sts:AssumeRole` record them, and answer the session's principal
    /// ARN — what a refusal of its calls must name.
    pub(crate) fn given_session(
        &self,
        name: &str,
        session_name: &str,
        access_key_id: &str,
    ) -> String {
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

    /// Put `document` in force as that role's inline policy, replacing whatever it carried —
    /// what `iam:PutRolePolicy` does. `None` takes every policy off it, as the matching
    /// delete does.
    pub(crate) fn given_role_policy(&self, role: &str, document: Option<&str>) {
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
}

/// A SigV4 `Authorization` header for this service, signed with `access_key_id` in `region`.
pub(crate) fn authorization(access_key_id: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// The ARN the account holds the role under — the identity whose policies an assumed-role
/// session's request is judged on.
pub(crate) fn role_arn(role: &str) -> String {
    format!(
        "arn:aws:iam::{}:role/{role}",
        ::engine::emulator_account_id()
    )
}

/// The principal ARN a session of `role` named `session` speaks for.
pub(crate) fn session_arn(role: &str, session: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role}/{session}",
        ::engine::emulator_account_id()
    )
}

/// The collection a family addressed under the ARN type `resource_type` is listed from: the
/// family's ARN with `*` in place of the resource identifier.
fn collection(region: &str, resource_type: &str) -> String {
    format!(
        "arn:aws:bedrock-agentcore:{region}:{}:{resource_type}/*",
        ::engine::emulator_account_id()
    )
}

/// The IAM action a listing is performed under: this endpoint's own namespace
/// (`sigv4Name` / `awsService.arnNamespace`, `smithy_query(query="service_info")`).
pub(crate) fn action_for(operation: &str) -> String {
    format!("bedrock-agentcore:{operation}")
}

/// The reason clause of a listing no readable statement allows — IAM's default deny.
pub(crate) fn no_policy_allows(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The reason clause of a listing an explicit `Deny` in the principal's own document covers.
pub(crate) const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The sentence a refused listing states.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// A policy document granting `actions` on `resources`, as `iam:PutRolePolicy` takes one.
pub(crate) fn allow(actions: &str, resources: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[
         {{"Effect":"Allow","Action":{actions},"Resource":{resources}}}]}}"#
    )
}

/// The agent policy the recorded check grants its team role: reads of ANOTHER action
/// namespace (Agents for Bedrock), which grants nothing on this endpoint.
pub(crate) fn another_namespace_only() -> String {
    allow(
        r#"["bedrock-agent:ListAgents","bedrock-agent:GetAgent"]"#,
        r#""*""#,
    )
}

/// One family's listing: the operation, its modelled route, and the ARN type the family is
/// addressed under.
struct Listing {
    operation: &'static str,
    method: &'static str,
    uri: &'static str,
    /// The resource type of the family's ARN.
    resource_type: &'static str,
    /// A body for a route whose input members are bound to the payload rather than the query.
    body: bool,
}

/// Every listing of this endpoint that addresses a family this layer names one ARN for.
/// Source: the routes are this crate's `OPERATIONS.md` (the `@http` bindings `smithy_query`
/// reports) and the ARN types are each family's own `*Arn` pattern as `crate::state` states
/// it (`AgentRuntimeArn` `runtime/…`, `GatewayArn` `gateway/…`, `MemoryArn` `memory/…`,
/// `RegistryArn` `registry/…`, `DatasetArn` `dataset/…`, `HarnessArn` `harness/…`,
/// `ConfigurationBundleArn` `configuration-bundle/…`, `BrowserProfileArn`
/// `browser-profile/…`, `PolicyEngineArn` `policy-engine/…`).
const LISTINGS: &[Listing] = &[
    Listing {
        operation: "ListAgentRuntimes",
        method: "POST",
        uri: "/runtimes/",
        resource_type: "runtime",
        body: false,
    },
    Listing {
        operation: "ListGateways",
        method: "GET",
        uri: "/gateways/",
        resource_type: "gateway",
        body: false,
    },
    Listing {
        operation: "ListMemories",
        method: "POST",
        uri: "/memories/",
        resource_type: "memory",
        body: true,
    },
    Listing {
        operation: "ListRegistries",
        method: "GET",
        uri: "/registries",
        resource_type: "registry",
        body: false,
    },
    Listing {
        operation: "ListDatasets",
        method: "GET",
        uri: "/datasets",
        resource_type: "dataset",
        body: false,
    },
    Listing {
        operation: "ListHarnesses",
        method: "GET",
        uri: "/harnesses",
        resource_type: "harness",
        body: false,
    },
    Listing {
        operation: "ListConfigurationBundles",
        method: "POST",
        uri: "/configuration-bundles",
        resource_type: "configuration-bundle",
        body: false,
    },
    Listing {
        operation: "ListBrowserProfiles",
        method: "POST",
        uri: "/browser-profiles",
        resource_type: "browser-profile",
        body: false,
    },
    Listing {
        operation: "ListPolicyEngines",
        method: "GET",
        uri: "/policy-engines",
        resource_type: "policy-engine",
        body: false,
    },
];

/// Assert `answer` is the authorization refusal of `listing`, blaming `principal` and naming
/// the family's collection in `region` — the Region the refused call was signed for — with
/// `reason`.
fn assert_denied(
    answer: &Answer,
    case: &str,
    principal: &str,
    listing: &Listing,
    region: &str,
    reason: &str,
) {
    assert_eq!(
        (answer.status, answer.code.as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?} with {:?}",
        answer.status,
        answer.code,
        answer.message
    );
    let expected = not_authorized(
        principal,
        &action_for(listing.operation),
        &collection(region, listing.resource_type),
        reason,
    );
    assert_eq!(
        answer.message, expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        answer.message
    );
}

/// Drive `listing` as the session `access_key_id` speaks for, in `region`.
async fn list_as(harness: &Harness, access_key_id: &str, region: &str, listing: &Listing) -> Answer {
    let body = json!({});
    harness
        .send_as(
            access_key_id,
            region,
            listing.method,
            listing.uri,
            listing.body.then_some(&body),
        )
        .await
}

/// RULE: whether the calling principal's identity-based documents allow
/// `bedrock-agentcore:ListAgentRuntimes` on the family's COLLECTION decides the listing, and
/// a document that does not allow it is answered 403 `AccessDeniedException` naming that
/// principal, that action, that collection and the rule that withheld the grant.
#[tokio::test]
async fn a_document_that_does_not_allow_the_listing_refuses_it() {
    let harness = Harness::new();
    let principal = harness.given_session(ROLE, SESSION, SESSION_KEY_ID);
    let runtimes = &LISTINGS[0];
    let collection_arn = collection(REGION, runtimes.resource_type);
    let one_member = format!(
        "arn:aws:bedrock-agentcore:{REGION}:{}:runtime/probeRuntime-abcdefghij",
        ::engine::emulator_account_id()
    );

    // (case, the document in force on the role, the refusal expected — `None` when allowed)
    let rows: Vec<(&str, Option<String>, Option<String>)> = vec![
        (
            "no document at all: IAM's default deny",
            None,
            Some(no_policy_allows(&action_for(runtimes.operation))),
        ),
        (
            "another action namespace entirely",
            Some(another_namespace_only()),
            Some(no_policy_allows(&action_for(runtimes.operation))),
        ),
        (
            "another operation of this namespace",
            Some(allow(r#""bedrock-agentcore:ListGateways""#, r#""*""#)),
            Some(no_policy_allows(&action_for(runtimes.operation))),
        ),
        (
            "exactly this action, over every resource",
            Some(allow(r#""bedrock-agentcore:ListAgentRuntimes""#, r#""*""#)),
            None,
        ),
        (
            "the whole namespace, over every resource",
            Some(allow(r#""bedrock-agentcore:*""#, r#""*""#)),
            None,
        ),
        (
            "this action scoped to the family's collection",
            Some(allow(
                r#""bedrock-agentcore:ListAgentRuntimes""#,
                &format!(r#""{collection_arn}""#),
            )),
            None,
        ),
        (
            "this action scoped to ONE member of the family, which is not the collection",
            Some(allow(
                r#""bedrock-agentcore:ListAgentRuntimes""#,
                &format!(r#""{one_member}""#),
            )),
            Some(no_policy_allows(&action_for(runtimes.operation))),
        ),
        (
            "an explicit deny of this action beside an allow of the namespace",
            Some(
                r#"{"Version":"2012-10-17","Statement":[
                 {"Effect":"Allow","Action":"bedrock-agentcore:*","Resource":"*"},
                 {"Effect":"Deny","Action":"bedrock-agentcore:ListAgentRuntimes",
                   "Resource":"*"}]}"#
                    .to_string(),
            ),
            Some(EXPLICIT_DENY.to_string()),
        ),
    ];

    for (case, document, refusal) in rows {
        harness.given_role_policy(ROLE, document.as_deref());
        let answer = list_as(&harness, SESSION_KEY_ID, REGION, runtimes).await;
        match refusal {
            Some(reason) => assert_denied(&answer, case, &principal, runtimes, REGION, &reason),
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: an allowed listing reaches the operation, got {} {:?} \
                     with {:?}",
                    answer.status, answer.code, answer.message
                );
                assert!(
                    answer.body["agentRuntimes"].is_array(),
                    "case {case:?}: expected the modelled listing, got {:?}",
                    answer.body
                );
            }
        }
    }
}

/// RULE over the whole class: every listing of this endpoint is judged under ITS OWN action
/// and against ITS OWN family's collection, and one a matching statement allows reaches its
/// operation.
#[tokio::test]
async fn every_family_listing_is_judged_under_its_own_action_and_collection() {
    let harness = Harness::new();
    let principal = harness.given_session(ROLE, SESSION, SESSION_KEY_ID);

    harness.given_role_policy(ROLE, Some(&another_namespace_only()));
    for listing in LISTINGS {
        let answer = list_as(&harness, SESSION_KEY_ID, REGION, listing).await;
        assert_denied(
            &answer,
            listing.operation,
            &principal,
            listing,
            REGION,
            &no_policy_allows(&action_for(listing.operation)),
        );
    }

    // The same sessions' calls once the namespace is granted: the gate refuses none of them,
    // and each listing answers its own verdict.
    harness.given_role_policy(ROLE, Some(&allow(r#""bedrock-agentcore:*""#, r#""*""#)));
    for listing in LISTINGS {
        let answer = list_as(&harness, SESSION_KEY_ID, REGION, listing).await;
        assert_ne!(
            (answer.status, answer.code.as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {:?}: a granted listing is not refused, got {:?}",
            listing.operation,
            answer.message
        );
    }
}

/// RULE: the collection is named in the SIGNED Region, so a statement scoped to one Region's
/// collection covers that Region's listing and no other — and the refusal names the Region
/// the request was signed for.
#[tokio::test]
async fn a_scoped_statement_covers_only_the_collection_of_the_region_signed_for() {
    let harness = Harness::new();
    let principal = harness.given_session(ROLE, SESSION, SESSION_KEY_ID);
    let runtimes = &LISTINGS[0];
    harness.given_role_policy(
        ROLE,
        Some(&allow(
            r#""bedrock-agentcore:ListAgentRuntimes""#,
            &format!(r#""{}""#, collection(REGION, runtimes.resource_type)),
        )),
    );

    let signed_for_the_region = list_as(&harness, SESSION_KEY_ID, REGION, runtimes).await;
    assert_eq!(
        signed_for_the_region.status, 200,
        "case \"the Region the statement names\": expected the operation's own answer, got \
         {} {:?} with {:?}",
        signed_for_the_region.status,
        signed_for_the_region.code,
        signed_for_the_region.message
    );

    let elsewhere = list_as(&harness, SESSION_KEY_ID, OTHER_REGION, runtimes).await;
    assert_denied(
        &elsewhere,
        "another Region, which the statement does not name",
        &principal,
        runtimes,
        OTHER_REGION,
        &no_policy_allows(&action_for(runtimes.operation)),
    );
}

/// RULE's CONSEQUENCE: a refused listing discloses nothing of the family, while the granted
/// twin session — the same policy plus one added allow of the listing — and the account's own
/// published credential list the same resources.
#[tokio::test]
async fn a_refused_listing_discloses_nothing_while_the_granted_twin_lists() {
    let harness = Harness::new();
    let principal = harness.given_session(ROLE, SESSION, SESSION_KEY_ID);
    harness.given_session(TWIN_ROLE, SESSION, TWIN_KEY_ID);
    let runtimes = &LISTINGS[0];
    let held = harness.create_runtime("disclosureProbe").await;

    harness.given_role_policy(ROLE, Some(&another_namespace_only()));
    // The twin carries that same agent policy plus ONE added allow of this listing.
    harness.given_role_policy(
        TWIN_ROLE,
        Some(
            r#"{"Version":"2012-10-17","Statement":[
             {"Effect":"Allow","Action":["bedrock-agent:ListAgents","bedrock-agent:GetAgent"],
              "Resource":"*"},
             {"Effect":"Allow","Action":"bedrock-agentcore:ListAgentRuntimes",
              "Resource":"*"}]}"#,
        ),
    );

    let owner = list_as(&harness, AMBIENT_KEY_ID, REGION, runtimes).await;
    assert_eq!(
        owner.status, 200,
        "case \"the account's own published credential\": expected the operation's own \
         answer, got {} {:?} with {:?}",
        owner.status, owner.code, owner.message
    );
    let listed = ids(&owner.body);
    assert!(
        listed.contains(&held),
        "case \"the owner's listing\": expected it to hold {held:?}, got {listed:?}"
    );

    let twin = list_as(&harness, TWIN_KEY_ID, REGION, runtimes).await;
    assert_eq!(
        twin.status, 200,
        "case \"the granted twin session\": expected the operation's own answer, got {} {:?} \
         with {:?}",
        twin.status, twin.code, twin.message
    );
    assert_eq!(
        ids(&twin.body),
        listed,
        "case \"the granted twin session\": expected what the owner lists, got {:?}",
        twin.body
    );

    let denied = list_as(&harness, SESSION_KEY_ID, REGION, runtimes).await;
    assert_denied(
        &denied,
        "the session granted only another namespace",
        &principal,
        runtimes,
        REGION,
        &no_policy_allows(&action_for(runtimes.operation)),
    );
    assert!(
        !denied.message.contains(&held) && denied.body.get("agentRuntimes").is_none(),
        "case \"the denied session\": expected no runtime disclosed, got {:?}",
        denied.body
    );
}

/// The `agentRuntimeId`s a `ListAgentRuntimes` body reports, in the order it reports them.
fn ids(body: &Value) -> Vec<String> {
    body["agentRuntimes"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item["agentRuntimeId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}
