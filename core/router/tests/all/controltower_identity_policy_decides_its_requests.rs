// SPDX-License-Identifier: CC0-1.0

//! IAM authorization of the AWS Control Tower API, over its restJson1 wire and through the
//! documents IAM itself stored.
//!
//! > **RULE: a request to AWS Control Tower is authorized BEFORE the operation reads or
//! > writes anything, as the IAM action `controltower:<OperationName>` on the resource the
//! > request addresses, against the identity-based documents in force on the principal its
//! > credential speaks for. An explicit `Deny` about that request refuses it whatever an
//! > `Allow` says, and the refusal is HTTP 403 with the error code `AccessDeniedException`
//! > and IAM's own sentence — the signing principal, the action, the resource, and the
//! > clause naming the document that refused. Taking the `Deny` off again restores the
//! > service.**
//!
//! Only a cross-service suite can pin that: three crates have to agree. IAM holds the role
//! and the inline documents attached to it, the token service mints the session a request
//! signs with, and Control Tower takes the decision and words the refusal. So everything
//! here is driven the way a client reaches it — `iam:CreateRole`, `iam:PutRolePolicy`,
//! `sts:AssumeRole`, then the signed Control Tower call, and `iam:DeleteRolePolicy` for the
//! leg that lifts the deny — which makes the documents under test the ones IAM stored and
//! the principal a refusal blames the identity the credential was minted for. (This
//! service's own suite decides the same rule with the stores seeded directly, and over the
//! whole modelled surface; what is added here is that the wire path through IAM and the
//! token service decides it too.)
//!
//! The rows span what the rule generalises over: the OPERATION asked for (a listing and a
//! create, which address the collection of their resource type, a read naming a landing
//! zone by ARN and a read naming an enabled control by ARN, which address that ARN, and a
//! listing of the published baseline catalog, whose collection is of another type), and the
//! DOCUMENTS in force (an allow alone; an allow beside a deny of exactly this action, of
//! this whole service, of another action, and of another resource; and a deny with no allow
//! at all). Every row asserts the whole answer a client branches on — the status, the error
//! code and the sentence — and the write rows assert what the account holds afterwards,
//! read where a client reads it, so a refusal that still performed the call fails here.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_controltower_awscontroltowerapis::state as controltower_state;

use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for, so the ARNs the decisions are made about are
/// the ones this Region's resources carry.
const REGION: &str = "us-west-2";

/// An access key id neither the token service nor IAM holds a record for: the position the
/// account's own published credential is in, which is what every setup and read-back leg is
/// signed with.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// The wire name and status of the refusal the authorization layer answers with, which this
/// service's model declares for the operations below.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// This service's IAM action prefix: an action is `controltower:<OperationName>`.
const ACTION_PREFIX: &str = "controltower";

/// The clause IAM closes the refusal of a request one of the principal's own identity-based
/// documents denies with.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The awsQuery API versions the identity services are called at.
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// The inline policy names the legs below attach documents under, so a leg can take one off
/// again by name.
const ALLOWING_POLICY: &str = "allowing";
const DENYING_POLICY: &str = "denying";

/// The landing zone version every create below asks for.
const LANDING_ZONE_VERSION: &str = "3.3";

/// One answer, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` the error document carries.
    fn message(&self) -> String {
        self.body["message"].as_str().unwrap_or_default().to_string()
    }
}

/// A principal a call is signed as: the access key id it presents, and the ARN a refusal
/// must blame it by — what the token service reported when it minted that key.
#[derive(Debug)]
struct Principal {
    access_key_id: String,
    arn: String,
}

/// A SigV4 `Authorization` header whose credential scope names `service`. The signature is
/// never checked — the router here runs with request authentication off — and the access key
/// id is what resolves the caller's identity.
fn authorization(access_key_id: &str, service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// The emulator these rows drive: Control Tower beside the IAM and token services the role,
/// its documents and the session are made with, on ONE engine, as `core/router/src/lib.rs`
/// mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_controltower_awscontroltowerapis::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One Control Tower call: `restJson1` under the `controltower` credential scope, signed
/// with `access_key_id`.
async fn controltower(
    router: &MultiServiceRouter,
    access_key_id: &str,
    path: &str,
    body: &Value,
) -> Reply {
    let payload = serde_json::to_vec(body).expect("the request body serializes");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://{ACTION_PREFIX}.{REGION}.amazonaws.com{path}"
        ))
        .header("host", format!("{ACTION_PREFIX}.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            authorization(access_key_id, ACTION_PREFIX),
        )
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            // restJson1 may qualify the shape with a namespace or a URI.
            value
                .split('/')
                .next()
                .unwrap_or(value)
                .rsplit('#')
                .next()
                .unwrap_or(value)
                .to_string()
        });
    let payload = response.into_body();
    let body = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    Reply {
        status,
        error_code,
        body,
    }
}

/// One awsQuery call to IAM or the token service, signed with the account's own credential,
/// which must succeed: these are the suite's setup legs.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(ACCOUNT_KEY, signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {action:?}: the setup call must succeed, got {status} with {text}"
    );
    text
}

/// Create the role `role_name`, trusted by this account, as `iam:CreateRole` records it.
async fn given_role(router: &MultiServiceRouter, role_name: &str) {
    let account = engine::emulator_account_id();
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{account}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
}

/// Attach `document` to `role_name` as the inline policy `policy_name`, as
/// `iam:PutRolePolicy` does.
async fn attach(router: &MultiServiceRouter, role_name: &str, policy_name: &str, document: &Value) {
    query(
        router,
        "iam",
        IAM_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", policy_name.to_string()),
            ("PolicyDocument", document.to_string()),
        ],
    )
    .await;
}

/// Take the inline policy `policy_name` off `role_name`, as `iam:DeleteRolePolicy` does.
async fn detach(router: &MultiServiceRouter, role_name: &str, policy_name: &str) {
    query(
        router,
        "iam",
        IAM_VERSION,
        "DeleteRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", policy_name.to_string()),
        ],
    )
    .await;
}

/// Assume `role_name` through the token service and answer the session's access key id with
/// the principal ARN the token service reported for it.
async fn assume(router: &MultiServiceRouter, role_name: &str, session_name: &str) -> Principal {
    let account = engine::emulator_account_id();
    let assumed = query(
        router,
        "sts",
        STS_VERSION,
        "AssumeRole",
        &[
            (
                "RoleArn",
                format!("arn:aws:iam::{account}:role/{role_name}"),
            ),
            ("RoleSessionName", session_name.to_string()),
        ],
    )
    .await;
    Principal {
        access_key_id: element(&assumed, "AccessKeyId"),
        arn: element(&assumed, "Arn"),
    }
}

/// A statement of `effect` about `action` on `resource`, as one inline document.
fn statement(effect: &str, action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": action, "Resource": resource }],
    })
}

/// A well-formed landing zone manifest governing the signed Region, carrying the parameters
/// the user guide's example manifest for `CreateLandingZone` names, so a create below is
/// never refused for its manifest.
fn manifest() -> Value {
    json!({
        "governedRegions": [REGION],
        "organizationStructure": {
            "security": { "name": "Security" },
            "sandbox": { "name": "Sandbox" }
        },
        "centralizedLogging": {
            "accountId": "222222222222",
            "configurations": {
                "loggingBucket": { "retentionDays": 60 },
                "accessLoggingBucket": { "retentionDays": 3650 }
            },
            "enabled": true
        },
        "securityRoles": { "accountId": "333333333333" },
        "accessManagement": { "enabled": true }
    })
}

/// The sentence IAM words a refusal with: the signing principal, the action the request
/// asked for, the resource it addressed and the clause naming the document that refused.
fn refusal(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {EXPLICIT_DENY}"
    )
}

/// How many landing zones the account holds, read back as the account itself.
async fn landing_zones_held(router: &MultiServiceRouter, case: &str) -> usize {
    let listed = controltower(router, ACCOUNT_KEY, "/list-landingzones", &json!({})).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: the account's own listing must be served, got {listed:?}"
    );
    listed.body["landingZones"].as_array().map_or(0, Vec::len)
}

/// RULE, on the recorded shape of it: with one inline policy ALLOWING the action the call is
/// served; attaching a SECOND inline policy that DENIES the same action turns the identical
/// signed call into HTTP 403 `AccessDeniedException` with IAM's explicit-deny sentence, and
/// deleting that policy again restores the service — so the refusal is the deny's and not an
/// absent grant's.
#[tokio::test]
async fn an_explicit_deny_iam_wrote_refuses_the_call_the_allow_served() {
    let router = build_router();
    let role = "controltower-reader";
    let action = format!("{ACTION_PREFIX}:ListLandingZones");
    let collection = controltower_state::landing_zone_arn(REGION, "*");

    // The account sets itself up with its own credential, so the listing under test has
    // something to report.
    let created = controltower(
        &router,
        ACCOUNT_KEY,
        "/create-landingzone",
        &json!({ "version": LANDING_ZONE_VERSION, "manifest": manifest() }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case \"the account's own create\": expected 200, got {created:?}"
    );
    let landing_zone = created.body["arn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    given_role(&router, role).await;
    attach(
        &router,
        role,
        ALLOWING_POLICY,
        &statement("Allow", &action, "*"),
    )
    .await;

    // The positive control: with the allow alone the session is served, and the listing is
    // the one the account holds.
    let allowed = assume(&router, role, "probe-allowed").await;
    let served = controltower(
        &router,
        &allowed.access_key_id,
        "/list-landingzones",
        &json!({}),
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case \"the allow alone\": expected the listing to be served, got {served:?}"
    );
    assert_eq!(
        served.body["landingZones"][0]["arn"].as_str(),
        Some(landing_zone.as_str()),
        "case \"the allow alone\": expected the account's landing zone, got {served:?}"
    );

    // The deny, attached as a second inline policy and asked under a freshly assumed
    // session, exactly as the recorded probe asks it.
    attach(
        &router,
        role,
        DENYING_POLICY,
        &statement("Deny", &action, "*"),
    )
    .await;
    let denied = assume(&router, role, "probe-denied").await;
    let refused = controltower(
        &router,
        &denied.access_key_id,
        "/list-landingzones",
        &json!({}),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_code.as_deref()),
        (ACCESS_DENIED_STATUS, Some(ACCESS_DENIED)),
        "case \"the deny attached\": expected HTTP {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got \
         {refused:?}"
    );
    assert_eq!(
        refused.message(),
        refusal(&denied.arn, &action, &collection),
        "case \"the deny attached\": expected IAM's wording, got {:?}",
        refused.message()
    );
    assert!(
        refused.body["landingZones"].is_null(),
        "case \"the deny attached\": a refused call reports nothing of the listing, got \
         {refused:?}"
    );

    // The deny deleted: the identical call is served again.
    detach(&router, role, DENYING_POLICY).await;
    let restored_as = assume(&router, role, "probe-restored").await;
    let restored = controltower(
        &router,
        &restored_as.access_key_id,
        "/list-landingzones",
        &json!({}),
    )
    .await;
    assert_eq!(
        restored.status, 200,
        "case \"the deny deleted\": expected the listing to be served again, got {restored:?}"
    );
    assert_eq!(
        restored.body["landingZones"][0]["arn"].as_str(),
        Some(landing_zone.as_str()),
        "case \"the deny deleted\": expected the account's landing zone, got {restored:?}"
    );
}

/// The error code an operation of this service answers a request addressing a resource no
/// call of this account minted with: "The request references a resource that does not exist"
/// (HTTP 404), which is what a GRANTED read of such a resource reaches.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// What a request the gate lets THROUGH is then answered by the operation itself — asserted
/// per row, so an allowed row exercises more than the absence of a refusal.
#[derive(Debug, Clone, Copy)]
enum Allowed {
    /// The operation serves it: HTTP 200.
    Served,
    /// The operation reaches its own verdict for a resource this account never minted:
    /// [`RESOURCE_NOT_FOUND`].
    NotFound,
    /// The operation's own answer depends on what an earlier row of the same table left in
    /// the account, so only the absence of an authorization refusal is asserted.
    NotRefused,
}

/// One request a decision is taken about: the call itself, the operation whose action IAM
/// judges it by, the resource the request addresses, and what the operation answers when the
/// gate lets it through.
struct Gated {
    case: &'static str,
    path: &'static str,
    body: Value,
    operation: &'static str,
    resource: String,
    allowed: Allowed,
}

impl Gated {
    /// The IAM action this request asks for.
    fn action(&self) -> String {
        format!("{ACTION_PREFIX}:{}", self.operation)
    }
}

/// What the documents in force on a row's role say, as the rows vary them.
#[derive(Debug, Clone, Copy)]
enum Documents {
    /// One policy allowing every action of this service on every resource.
    AllowOnly,
    /// That allow, beside a second policy denying exactly the action under test.
    AllowAndDenyThisAction,
    /// That allow, beside a second policy denying every action of this service.
    AllowAndDenyThisService,
    /// That allow, beside a second policy denying ANOTHER action of this service.
    AllowAndDenyAnotherAction,
    /// That allow, beside a second policy denying this service's actions on ANOTHER
    /// resource of the same type.
    AllowAndDenyAnotherResource,
    /// A deny of the action with no allow anywhere: the deny is still what refuses.
    DenyOnly,
}

impl Documents {
    /// Whether a request the gate judges under these documents is refused.
    fn refuses(self) -> bool {
        match self {
            Self::AllowOnly
            | Self::AllowAndDenyAnotherAction
            | Self::AllowAndDenyAnotherResource => false,
            Self::AllowAndDenyThisAction | Self::AllowAndDenyThisService | Self::DenyOnly => true,
        }
    }

    /// The inline policies to attach to the row's role, each with the name it is attached
    /// under. `action` is the action under test, `other` another action of this service and
    /// `elsewhere` another resource of the type the request addresses.
    fn policies(self, action: &str, other: &str, elsewhere: &str) -> Vec<(&'static str, Value)> {
        let everything = statement("Allow", &format!("{ACTION_PREFIX}:*"), "*");
        match self {
            Self::AllowOnly => vec![(ALLOWING_POLICY, everything)],
            Self::AllowAndDenyThisAction => vec![
                (ALLOWING_POLICY, everything),
                (DENYING_POLICY, statement("Deny", action, "*")),
            ],
            Self::AllowAndDenyThisService => vec![
                (ALLOWING_POLICY, everything),
                (
                    DENYING_POLICY,
                    statement("Deny", &format!("{ACTION_PREFIX}:*"), "*"),
                ),
            ],
            Self::AllowAndDenyAnotherAction => vec![
                (ALLOWING_POLICY, everything),
                (DENYING_POLICY, statement("Deny", other, "*")),
            ],
            Self::AllowAndDenyAnotherResource => vec![
                (ALLOWING_POLICY, everything),
                (DENYING_POLICY, statement("Deny", action, elsewhere)),
            ],
            Self::DenyOnly => vec![(DENYING_POLICY, statement("Deny", action, "*"))],
        }
    }
}

/// RULE over the class: a request is judged by its own `controltower:<OperationName>` action
/// on the resource it addresses, and an explicit `Deny` ABOUT that request — of the action
/// itself or of this whole service, with or without an allow beside it — refuses it with
/// HTTP 403 `AccessDeniedException` and IAM's sentence, while a `Deny` about another action
/// or another resource refuses nothing.
#[tokio::test]
async fn a_deny_about_the_request_refuses_it_and_one_about_another_does_not() {
    let router = build_router();
    let landing_zone = controltower_state::landing_zone_arn(REGION, "1a2b3c4d5e");
    let another_landing_zone = controltower_state::landing_zone_arn(REGION, "f6g7h8i9j0");
    let enabled_control = controltower_state::enabled_control_arn(REGION, "9z8y7x6w5v");
    let another_enabled_control = controltower_state::enabled_control_arn(REGION, "5v4u3t2s1r");

    // One row per kind of request: two that address the collection of their resource type (a
    // listing and a create), two that address the ARN their request names, and one whose
    // collection is of another resource type altogether.
    let gates = [
        Gated {
            case: "a listing of the landing zones",
            path: "/list-landingzones",
            body: json!({}),
            operation: "ListLandingZones",
            resource: controltower_state::landing_zone_arn(REGION, "*"),
            allowed: Allowed::Served,
        },
        Gated {
            case: "a create of a landing zone",
            path: "/create-landingzone",
            body: json!({ "version": LANDING_ZONE_VERSION, "manifest": manifest() }),
            operation: "CreateLandingZone",
            resource: controltower_state::landing_zone_arn(REGION, "*"),
            // One create of the table's rows sets the account up and the next is answered on
            // that account, so this row asserts the gate alone.
            allowed: Allowed::NotRefused,
        },
        Gated {
            case: "a read naming a landing zone by ARN",
            path: "/get-landingzone",
            body: json!({ "landingZoneIdentifier": landing_zone }),
            operation: "GetLandingZone",
            resource: landing_zone.clone(),
            allowed: Allowed::NotFound,
        },
        Gated {
            case: "a read naming an enabled control by ARN",
            path: "/get-enabled-control",
            body: json!({ "enabledControlIdentifier": enabled_control }),
            operation: "GetEnabledControl",
            resource: enabled_control.clone(),
            allowed: Allowed::NotFound,
        },
        Gated {
            case: "a listing of the published baseline catalog",
            path: "/list-baselines",
            body: json!({}),
            operation: "ListBaselines",
            resource: controltower_state::baseline_arn(REGION, "*"),
            allowed: Allowed::Served,
        },
    ];

    let documents = [
        Documents::AllowOnly,
        Documents::AllowAndDenyThisAction,
        Documents::AllowAndDenyThisService,
        Documents::AllowAndDenyAnotherAction,
        Documents::AllowAndDenyAnotherResource,
        Documents::DenyOnly,
    ];

    for (gate_index, gate) in gates.iter().enumerate() {
        let action = gate.action();
        // Another action of this service, which one row denies instead of the one under
        // test, and another resource of the type this request addresses.
        let other = gates
            .iter()
            .map(Gated::action)
            .find(|candidate| *candidate != action)
            .expect("this service has more than one operation");
        let elsewhere = if gate.resource == enabled_control {
            another_enabled_control.clone()
        } else {
            another_landing_zone.clone()
        };

        for (row, in_force) in documents.iter().enumerate() {
            let case = format!("{} under {in_force:?}", gate.case);
            // A role of its own per row, so no row is decided by another's documents.
            let role = format!("r-{gate_index}-{row}");
            given_role(&router, &role).await;
            for (policy_name, document) in in_force.policies(&action, &other, &elsewhere) {
                attach(&router, &role, policy_name, &document).await;
            }
            let principal = assume(&router, &role, "probe").await;

            let before = landing_zones_held(&router, &case).await;
            let reply = controltower(&router, &principal.access_key_id, gate.path, &gate.body).await;

            if !in_force.refuses() {
                assert_ne!(
                    reply.error_code.as_deref(),
                    Some(ACCESS_DENIED),
                    "case {case:?}: an allowed request reaches the operation's own answer, got \
                     {reply:?}"
                );
                assert_ne!(
                    reply.status, ACCESS_DENIED_STATUS,
                    "case {case:?}: an allowed request must not be refused, got {reply:?}"
                );
                // What the operation itself then answers, where that answer does not depend
                // on what an earlier row left in the account.
                match gate.allowed {
                    Allowed::Served => assert_eq!(
                        reply.status, 200,
                        "case {case:?}: expected the operation to serve the call, got {reply:?}"
                    ),
                    Allowed::NotFound => assert_eq!(
                        reply.error_code.as_deref(),
                        Some(RESOURCE_NOT_FOUND),
                        "case {case:?}: expected the operation's own {RESOURCE_NOT_FOUND} for a \
                         resource this account never minted, got {reply:?}"
                    ),
                    Allowed::NotRefused => {}
                }
                continue;
            }

            assert_eq!(
                (reply.status, reply.error_code.as_deref()),
                (ACCESS_DENIED_STATUS, Some(ACCESS_DENIED)),
                "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got \
                 {reply:?}"
            );
            assert_eq!(
                reply.message(),
                refusal(&principal.arn, &action, &gate.resource),
                "case {case:?}: expected IAM's wording, got {:?}",
                reply.message()
            );
            // A refused call performs nothing: whatever the account held before it, it holds
            // after — which for the create row is what says the refusal came first.
            let after = landing_zones_held(&router, &case).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused call leaves the account as it was, expected {before} \
                 landing zones, got {after}"
            );
        }
    }
}

/// RULE, on the least-privilege shape: a deny of ONE action withholds exactly that action,
/// so the same principal is refused the denied call and served a sibling call of the same
/// service under the same documents.
#[tokio::test]
async fn a_deny_of_one_action_withholds_only_that_action() {
    let router = build_router();
    let role = "controltower-operator";
    let denied_action = format!("{ACTION_PREFIX}:CreateLandingZone");

    given_role(&router, role).await;
    attach(
        &router,
        role,
        ALLOWING_POLICY,
        &statement("Allow", &format!("{ACTION_PREFIX}:*"), "*"),
    )
    .await;
    attach(
        &router,
        role,
        DENYING_POLICY,
        &statement("Deny", &denied_action, "*"),
    )
    .await;
    let principal = assume(&router, role, "probe").await;

    let case = "a create denied, a listing allowed";
    let before = landing_zones_held(&router, case).await;
    let refused = controltower(
        &router,
        &principal.access_key_id,
        "/create-landingzone",
        &json!({ "version": LANDING_ZONE_VERSION, "manifest": manifest() }),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_code.as_deref()),
        (ACCESS_DENIED_STATUS, Some(ACCESS_DENIED)),
        "case {case:?}: expected the create to be refused, got {refused:?}"
    );
    assert_eq!(
        refused.message(),
        refusal(
            &principal.arn,
            &denied_action,
            &controltower_state::landing_zone_arn(REGION, "*")
        ),
        "case {case:?}: expected IAM's wording, got {:?}",
        refused.message()
    );
    assert_eq!(
        landing_zones_held(&router, case).await,
        before,
        "case {case:?}: the refused create mints no landing zone"
    );

    let served = controltower(
        &router,
        &principal.access_key_id,
        "/list-landingzones",
        &json!({}),
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: the sibling listing is still granted, got {served:?}"
    );
}
