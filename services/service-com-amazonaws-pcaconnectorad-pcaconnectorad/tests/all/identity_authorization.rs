// SPDX-License-Identifier: CC0-1.0

//! WHO may call this service, driven end to end over the wire (a restJson1 request through
//! the multi-service router into the mounted service), so what is asserted is what a client
//! holding those credentials sees.
//!
//! THE RULE: IAM decides authorization ahead of the service's own code, so every operation
//! evaluates the policies in force on the principal its credentials name against
//! `pca-connector-ad:<OperationName>` on the resource the request NAMES — the ARN its own
//! members carry, or the collection a call that addresses no single resource adds to or
//! reads over — BEFORE it reads or writes anything. A request no statement allows, or one
//! an explicit `Deny` covers, is answered `AccessDeniedException` (HTTP 403) in IAM's own
//! wording and performs nothing; a request a matching `Allow` covers reaches the
//! operation's own answer.
//!
//! The role, its policies and the session live in the IAM and STS stores of the SAME shared
//! engine, so the documents under test are the ones `iam:PutRolePolicy` stores and the
//! principal a refusal blames is the one `sts:GetCallerIdentity` answers for the key the
//! request signed with.
//!
//! The tables span what the rule generalises over: the OPERATION (a create, a read, a
//! listing, a tag write, a delete, across all four resource kinds this service holds), the
//! document in force (none, an allow of every OTHER action, an allow of exactly the action,
//! an allow of the whole service), and the resource a statement is scoped to (the resource
//! the request names, another resource of the same kind, every resource).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{encode_label, ACCOUNT, ACCOUNT_KEY_ID, REGION};

/// The access key id of the assumed-role session the rows below sign with.
const SESSION_KEY_ID: &str = "ASIAEXAMPLESESSION01";

/// The role that session is assumed from, the session's own name, and the name of the
/// inline policy the role carries.
const ROLE: &str = "pca-connector-ad-caller";
const SESSION: &str = "connector-session";
const POLICY_NAME: &str = "inline";

/// How long the seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// This service's IAM action prefix, which is its ARN namespace.
const ACTION_PREFIX: &str = "pca-connector-ad";

/// A router serving this service beside the IAM and STS services over ONE shared engine, so
/// the policies and the session the cases seed are the ones those services hold.
fn router_with_identity() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_pcaconnectorad_pcaconnectorad::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
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
    (router, engine)
}

/// One call to this service signed with `access_key_id`.
async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    target: &str,
    body: Option<&Value>,
) -> (u16, String, Value) {
    super::harness::call_as(router, access_key_id, method, target, body).await
}

/// Call with the account's own credential, requiring success: how every set-up step in this
/// suite is driven, since that credential holds no IAM policies and this gate does not
/// bound it.
async fn ok_as_account(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    target: &str,
    body: Option<&Value>,
) -> Value {
    let (status, code, parsed) = call_as(router, ACCOUNT_KEY_ID, method, target, body).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: expected {method} {target} to be served for the account's own \
         credential, got {status} / {code:?} / {parsed}"
    );
    parsed
}

/// The principal ARN the seeded session speaks for — what a refusal must name.
fn session_arn() -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/{SESSION}")
}

/// The ARN the account holds the role under, which is the identity whose policies an
/// assumed-role session's request is judged on.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE}")
}

/// Record the role and the assumed-role session [`SESSION_KEY_ID`] speaks for, as
/// `iam:CreateRole` and `sts:AssumeRole` record them.
fn given_session(engine: &Arc<engine::InMemoryEngine>) {
    let role_arn = role_arn();
    let iam_store = iam::service_state(&**engine).expect("the IAM store");
    iam_store.lock().roles.insert(
        iam::name_key(ROLE),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: ROLE.to_string(),
            role_id: "AROAEXAMPLEADROLE0001".to_string(),
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
    let vend = sts::NewSession {
        role_arn: Some(role_arn.clone()),
        ..sts::NewSession::new(
            sts::Identity {
                user_id: sts::role_id(&role_arn),
                account: ACCOUNT.to_string(),
                arn: session_arn(),
            },
            sts::SessionKind::AssumedRole,
            SESSION_SECONDS,
        )
    };
    let store = sts::service_state(&**engine).expect("the STS store");
    store
        .lock()
        .insert(vend.into_session(SESSION_KEY_ID, SystemTime::now()))
        .expect("the session is recorded");
}

/// Attach `document` to the role as its inline policy, replacing whatever it carried — what
/// `iam:PutRolePolicy` does. `None` takes every policy off it, as `iam:DeleteRolePolicy`
/// does.
fn given_role_policy(engine: &Arc<engine::InMemoryEngine>, document: Option<&Value>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    match document {
        None => {
            store.role_inline_policies.remove(&iam::name_key(ROLE));
        }
        Some(document) => {
            store.role_inline_policies.insert(
                iam::name_key(ROLE),
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

/// A policy allowing `actions` on every resource.
fn allow(actions: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }]
    })
}

/// The clause IAM's denial carries when an explicit `Deny` in one of the principal's OWN
/// identity-based documents refuses the call. Spelled out here rather than read off the
/// constant the gate renders, so the wording itself is what these cases assert.
const EXPLICIT_DENY_IN_IDENTITY_POLICY: &str = "with an explicit deny in an identity-based policy";

/// The same clause for an explicit `Deny` in one of the SESSION POLICIES the credential was
/// minted with.
const EXPLICIT_DENY_IN_SESSION_POLICY: &str = "with an explicit deny in a session policy";

/// The same clause for an explicit `Deny` in the principal's PERMISSIONS BOUNDARY.
const EXPLICIT_DENY_IN_PERMISSIONS_BOUNDARY: &str = "with an explicit deny in a permissions boundary";

/// IAM's clause for a call no readable identity-based document allows — its default deny,
/// which names the action nothing granted.
fn no_identity_policy_clause(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The same for a grant the SESSION POLICIES the credential was minted with do not also
/// allow.
fn no_session_policy_clause(action: &str) -> String {
    format!("because no session policy allows the {action} action")
}

/// The same for a grant the principal's PERMISSIONS BOUNDARY does not also allow.
fn no_permissions_boundary_clause(action: &str) -> String {
    format!("because no permissions boundary allows the {action} action")
}

/// IAM's whole denial sentence: the principal that signed, the action and resource it asked
/// for, and the clause naming which of IAM's rules withheld the grant.
fn refusal_sentence(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// Assert the answer is IAM's refusal of `action` on `resource` for the seeded session, with
/// `reason` naming which of IAM's rules withheld the grant.
fn assert_refused(
    case: &str,
    status: u16,
    code: &str,
    parsed: &Value,
    action: &str,
    resource: &str,
    reason: &str,
) {
    assert_eq!(
        (status, code),
        (403, "AccessDeniedException"),
        "case {case:?}: expected HTTP 403 AccessDeniedException, got {status} / {code:?} / \
         {parsed}"
    );
    let expected = refusal_sentence(&session_arn(), action, resource, reason);
    assert_eq!(
        parsed["Message"].as_str().or(parsed["message"].as_str()),
        Some(expected.as_str()),
        "case {case:?}: expected IAM's wording {expected:?}, got {parsed}"
    );
}

/// One operation of this service, with the request that drives it and the resource the
/// decision is taken against.
struct Judged {
    /// The operation's own modelled name, which is also the action's.
    operation: &'static str,
    method: &'static str,
    target: String,
    body: Option<Value>,
    /// The ARN the refusal must name.
    resource: String,
}

/// Every connector of this account in this Region, which is what a call addressing no
/// connector of its own is judged against — the resource type's ARN with a wildcard
/// resource id, as IAM writes such a resource.
fn connector_collection() -> String {
    format!("arn:aws:pca-connector-ad:{REGION}:{ACCOUNT}:connector/*")
}

/// The same for the directory registrations.
fn registration_collection() -> String {
    format!("arn:aws:pca-connector-ad:{REGION}:{ACCOUNT}:directory-registration/*")
}

/// The body a `CreateConnector` carries, for a directory already registered.
fn connector_body(directory_id: &str, token: &str) -> Value {
    json!({
        "DirectoryId": directory_id,
        "CertificateAuthorityArn": format!(
            "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
             11111111-2222-3333-4444-555555555555"
        ),
        "VpcInformation": { "SecurityGroupIds": ["sg-0123456789abcdef0"] },
        "ClientToken": token,
    })
}

/// A `CreateTemplate` body under `connector_arn`.
fn template_body(connector_arn: &str, name: &str) -> Value {
    json!({
        "ConnectorArn": connector_arn,
        "Name": name,
        "ClientToken": name,
        "Definition": {
            "TemplateV2": {
                "CertificateValidity": {
                    "ValidityPeriod": { "PeriodType": "YEARS", "Period": 1 },
                    "RenewalPeriod": { "PeriodType": "WEEKS", "Period": 6 },
                },
                "PrivateKeyAttributes": { "MinimalKeyLength": 2048, "KeySpec": "KEY_EXCHANGE" },
                "PrivateKeyFlags": { "ClientVersion": "WINDOWS_SERVER_2016" },
                "EnrollmentFlags": { "NoSecurityExtension": true },
                "SubjectNameFlags": { "SanRequireDirectoryGuid": true },
                "GeneralFlags": { "MachineType": true },
                "Extensions": { "KeyUsage": { "UsageFlags": { "KeyEncipherment": true } } },
            }
        },
    })
}

/// Register a directory with the account's own credential and answer the registration ARN.
async fn given_registration(
    router: &MultiServiceRouter,
    directory_id: &str,
) -> String {
    let body = json!({ "DirectoryId": directory_id, "ClientToken": format!("reg-{directory_id}") });
    let parsed = ok_as_account(
        router,
        directory_id,
        "POST",
        "/directoryRegistrations",
        Some(&body),
    )
    .await;
    parsed["DirectoryRegistrationArn"]
        .as_str()
        .expect("the registration ARN")
        .to_string()
}

/// Create a connector with the account's own credential and answer its ARN.
async fn given_connector(router: &MultiServiceRouter, directory_id: &str, token: &str) -> String {
    given_registration(router, directory_id).await;
    let parsed = ok_as_account(
        router,
        token,
        "POST",
        "/connectors",
        Some(&connector_body(directory_id, token)),
    )
    .await;
    parsed["ConnectorArn"]
        .as_str()
        .expect("the connector ARN")
        .to_string()
}

/// THE RULE across the operations of this service: each is judged against its OWN action on
/// the resource the request names, so a document granting one grants no other, and a
/// refused call performs nothing.
#[tokio::test]
async fn every_operation_is_judged_against_its_own_action_and_resource() {
    let (router, engine) = router_with_identity();
    given_session(&engine);

    // The resources the non-destructive rows address, plus one connector and one
    // registration for the two deletes, all created with the account's own credential.
    let read_connector = given_connector(&router, "d-00000000a1", "read-connector").await;
    let doomed_connector = given_connector(&router, "d-00000000a2", "doomed-connector").await;
    let doomed_registration = given_registration(&router, "d-00000000a3").await;
    let read_registration = format!(
        "arn:aws:pca-connector-ad:{REGION}:{ACCOUNT}:directory-registration/d-00000000a1"
    );
    let template = ok_as_account(
        &router,
        "the template the read rows address",
        "POST",
        "/templates",
        Some(&template_body(&read_connector, "read-template")),
    )
    .await["TemplateArn"]
        .as_str()
        .expect("the template ARN")
        .to_string();
    // A directory the creates below register and name, so an ALLOWED create succeeds.
    let create_directory = "d-00000000b1";
    given_registration(&router, create_directory).await;

    let judged: Vec<Judged> = vec![
        Judged {
            operation: "ListConnectors",
            method: "GET",
            target: "/connectors".to_string(),
            body: None,
            resource: connector_collection(),
        },
        Judged {
            operation: "GetConnector",
            method: "GET",
            target: format!("/connectors/{}", encode_label(&read_connector)),
            body: None,
            resource: read_connector.clone(),
        },
        Judged {
            operation: "CreateConnector",
            method: "POST",
            target: "/connectors".to_string(),
            body: Some(connector_body(create_directory, "allowed-connector")),
            resource: connector_collection(),
        },
        Judged {
            operation: "ListDirectoryRegistrations",
            method: "GET",
            target: "/directoryRegistrations".to_string(),
            body: None,
            resource: registration_collection(),
        },
        Judged {
            operation: "GetDirectoryRegistration",
            method: "GET",
            target: format!(
                "/directoryRegistrations/{}",
                encode_label(&read_registration)
            ),
            body: None,
            resource: read_registration.clone(),
        },
        Judged {
            operation: "CreateDirectoryRegistration",
            method: "POST",
            target: "/directoryRegistrations".to_string(),
            body: Some(json!({ "DirectoryId": "d-00000000b2", "ClientToken": "allowed-reg" })),
            resource: registration_collection(),
        },
        Judged {
            operation: "GetTemplate",
            method: "GET",
            target: format!("/templates/{}", encode_label(&template)),
            body: None,
            resource: template.clone(),
        },
        Judged {
            operation: "CreateTemplate",
            method: "POST",
            target: "/templates".to_string(),
            body: Some(template_body(&read_connector, "allowed-template")),
            resource: read_connector.clone(),
        },
        Judged {
            operation: "CreateServicePrincipalName",
            method: "POST",
            target: format!(
                "/directoryRegistrations/{}/servicePrincipalNames/{}",
                encode_label(&read_registration),
                encode_label(&read_connector)
            ),
            body: Some(json!({ "ClientToken": "allowed-spn" })),
            resource: read_registration.clone(),
        },
        Judged {
            operation: "ListTagsForResource",
            method: "GET",
            target: format!("/tags/{}", encode_label(&read_connector)),
            body: None,
            resource: read_connector.clone(),
        },
        Judged {
            operation: "UntagResource",
            method: "DELETE",
            target: format!("/tags/{}?tagKeys=owner", encode_label(&read_connector)),
            body: None,
            resource: read_connector.clone(),
        },
        // The destructive rows last: an ALLOWED call on each really does delete, so each
        // addresses a resource of its own.
        Judged {
            operation: "DeleteConnector",
            method: "DELETE",
            target: format!("/connectors/{}", encode_label(&doomed_connector)),
            body: None,
            resource: doomed_connector.clone(),
        },
        Judged {
            operation: "DeleteDirectoryRegistration",
            method: "DELETE",
            target: format!(
                "/directoryRegistrations/{}",
                encode_label(&doomed_registration)
            ),
            body: None,
            resource: doomed_registration.clone(),
        },
    ];

    for row in &judged {
        let action = format!("{ACTION_PREFIX}:{}", row.operation);

        // With no policy at all, IAM's default deny refuses it ...
        given_role_policy(&engine, None);
        let case = format!("{} with no policy", row.operation);
        let (status, code, parsed) =
            call_as(&router, SESSION_KEY_ID, row.method, &row.target, row.body.as_ref()).await;
        assert_refused(
            &case,
            status,
            &code,
            &parsed,
            &action,
            &row.resource,
            &no_identity_policy_clause(&action),
        );

        // ... and so does an allow of every OTHER action of this service: each operation is
        // judged against its own.
        let others: Vec<String> = judged
            .iter()
            .filter(|other| other.operation != row.operation)
            .map(|other| format!("{ACTION_PREFIX}:{}", other.operation))
            .collect();
        given_role_policy(&engine, Some(&allow(json!(others))));
        let case = format!("{} with every other action allowed", row.operation);
        let (status, code, parsed) =
            call_as(&router, SESSION_KEY_ID, row.method, &row.target, row.body.as_ref()).await;
        assert_refused(
            &case,
            status,
            &code,
            &parsed,
            &action,
            &row.resource,
            &no_identity_policy_clause(&action),
        );

        // An allow of exactly this action reaches the operation's own answer.
        given_role_policy(&engine, Some(&allow(json!(action))));
        let case = format!("{} allowed", row.operation);
        let (status, code, parsed) =
            call_as(&router, SESSION_KEY_ID, row.method, &row.target, row.body.as_ref()).await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: an allowed request must be served, got {status} / {code:?} / \
             {parsed}"
        );
    }
}

/// A refused call performs nothing: the connectors the account can see after a run of
/// refused creates are exactly the ones created before them.
#[tokio::test]
async fn a_refused_create_persists_nothing() {
    let (router, engine) = router_with_identity();
    given_session(&engine);
    let directory_id = "d-00000000c1";
    given_registration(&router, directory_id).await;

    let before = ok_as_account(&router, "the connectors before", "GET", "/connectors", None).await
        ["Connectors"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();

    // Three refusals: no policy, an allow of another action, and an explicit deny beside an
    // allow of the whole service — each with the clause naming the rule that withheld it.
    let action = format!("{ACTION_PREFIX}:CreateConnector");
    let documents: Vec<(&str, Option<Value>, String)> = vec![
        ("no policy at all", None, no_identity_policy_clause(&action)),
        (
            "an allow of another action",
            Some(allow(json!(format!("{ACTION_PREFIX}:GetConnector")))),
            no_identity_policy_clause(&action),
        ),
        (
            "an explicit deny beside an allow of the whole service",
            Some(json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
                    {
                        "Effect": "Deny",
                        "Action": format!("{ACTION_PREFIX}:CreateConnector"),
                        "Resource": "*",
                    },
                ]
            })),
            EXPLICIT_DENY_IN_IDENTITY_POLICY.to_string(),
        ),
    ];

    for (index, (case, document, reason)) in documents.iter().enumerate() {
        given_role_policy(&engine, document.as_ref());
        // A fresh `ClientToken` each time (its expression admits no space), so no retry is
        // answered from the idempotency record of an earlier call.
        let (status, code, parsed) = call_as(
            &router,
            SESSION_KEY_ID,
            "POST",
            "/connectors",
            Some(&connector_body(directory_id, &format!("refused-token-{index}"))),
        )
        .await;
        assert_refused(
            case,
            status,
            &code,
            &parsed,
            &action,
            &connector_collection(),
            reason,
        );
        assert!(
            parsed.get("ConnectorArn").is_none(),
            "case {case:?}: a refused create must mint no connector, got {parsed}"
        );
    }

    let after = ok_as_account(&router, "the connectors after", "GET", "/connectors", None).await
        ["Connectors"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        after, before,
        "case \"the store after three refused creates\": expected {before} connector(s), \
         got {after}"
    );
}

/// The RESOURCE half of the decision: a statement scoped to one connector does not reach
/// another, and the collection a create is judged against is not one connector's ARN.
#[tokio::test]
async fn a_statement_scoped_to_one_connector_does_not_reach_another() {
    let (router, engine) = router_with_identity();
    given_session(&engine);
    let granted = given_connector(&router, "d-00000000d1", "granted").await;
    let withheld = given_connector(&router, "d-00000000d2", "withheld").await;
    let action = format!("{ACTION_PREFIX}:GetConnector");
    given_role_policy(
        &engine,
        Some(&json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": action, "Resource": granted }]
        })),
    );

    let case = "the connector the statement names";
    let (status, code, parsed) = call_as(
        &router,
        SESSION_KEY_ID,
        "GET",
        &format!("/connectors/{}", encode_label(&granted)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the named connector to be served, got {status} / {code:?} / \
         {parsed}"
    );
    assert_eq!(
        parsed["Connector"]["Arn"].as_str(),
        Some(granted.as_str()),
        "case {case:?}: expected the connector the request named, got {parsed}"
    );

    let case = "another connector of the same account";
    let (status, code, parsed) = call_as(
        &router,
        SESSION_KEY_ID,
        "GET",
        &format!("/connectors/{}", encode_label(&withheld)),
        None,
    )
    .await;
    assert_refused(
        case,
        status,
        &code,
        &parsed,
        &action,
        &withheld,
        &no_identity_policy_clause(&action),
    );
    assert!(
        parsed.get("Connector").is_none(),
        "case {case:?}: a refused read must report no connector, got {parsed}"
    );
}

/// The account's own published credential is not a principal the IAM store holds policies
/// for, so it is not bounded by this gate — which is what lets every set-up call above run.
#[tokio::test]
async fn the_accounts_own_credential_is_not_bounded_by_the_gate() {
    let (router, engine) = router_with_identity();
    given_session(&engine);
    // A deny of the whole service on the ROLE changes nothing for the account's credential.
    given_role_policy(
        &engine,
        Some(&json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Deny",
                "Action": format!("{ACTION_PREFIX}:*"),
                "Resource": "*",
            }]
        })),
    );
    let (status, code, parsed) =
        call_as(&router, ACCOUNT_KEY_ID, "GET", "/connectors", None).await;
    assert_eq!(
        status, 200,
        "case \"the account's own credential\": expected 200, got {status} / {code:?} / \
         {parsed}"
    );
    let (status, code, parsed) =
        call_as(&router, SESSION_KEY_ID, "GET", "/connectors", None).await;
    assert_refused(
        "the denied session",
        status,
        &code,
        &parsed,
        &format!("{ACTION_PREFIX}:ListConnectors"),
        &connector_collection(),
        EXPLICIT_DENY_IN_IDENTITY_POLICY,
    );
}

/// THE RULE through the APIs that WRITE the documents, rather than through the stores the
/// cases above seed: a role `iam:CreateRole` created, carrying the inline policy
/// `iam:PutRolePolicy` embedded in it, assumed through `sts:AssumeRole`, is judged on
/// exactly that document — and a policy rewritten between two calls decides the second one
/// differently from the first.
#[tokio::test]
async fn a_session_assumed_through_sts_is_judged_on_the_policy_iam_wrote() {
    let (router, _engine) = router_with_identity();
    let role = "assumed-through-sts";
    let case = "the role IAM's own API created";
    let role_arn = create_role(&router, case, role).await;

    // A policy that allows another action of this service and nothing else, which is the
    // position a caller "without the required permissions" is in.
    put_role_policy(
        &router,
        case,
        role,
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": format!("{ACTION_PREFIX}:GetConnector"),
                "Resource": "*",
            }]
        })
        .to_string(),
    )
    .await;
    let (session_key, session_name) = assume_role(&router, case, &role_arn).await;
    let directory_id = "d-00000000e1";
    given_registration(&router, directory_id).await;

    let (status, code, parsed) = call_as(
        &router,
        &session_key,
        "POST",
        "/connectors",
        Some(&connector_body(directory_id, "sts-session-refused")),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (403, "AccessDeniedException"),
        "case {case:?}: a session whose role does not allow the action is refused, \
         got {status} / {code:?} / {parsed}"
    );
    let action = format!("{ACTION_PREFIX}:CreateConnector");
    let expected = refusal_sentence(
        &format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{session_name}"),
        &action,
        &connector_collection(),
        &no_identity_policy_clause(&action),
    );
    assert_eq!(
        parsed["Message"].as_str().or(parsed["message"].as_str()),
        Some(expected.as_str()),
        "case {case:?}: expected {expected:?}, got {parsed}"
    );
    assert!(
        parsed.get("ConnectorArn").is_none(),
        "case {case:?}: a refused create mints no connector, got {parsed}"
    );

    // The document decides each request as it stands when that request is made: allowing
    // the action lifts the refusal for the SAME session.
    let case = "the same session once the role allows the action";
    put_role_policy(
        &router,
        case,
        role,
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": format!("{ACTION_PREFIX}:CreateConnector"),
                "Resource": "*",
            }]
        })
        .to_string(),
    )
    .await;
    let (status, code, parsed) = call_as(
        &router,
        &session_key,
        "POST",
        "/connectors",
        Some(&connector_body(directory_id, "sts-session-allowed")),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: an allowed create is served, got {status} / {code:?} / {parsed}"
    );
    assert!(
        parsed["ConnectorArn"].as_str().is_some(),
        "case {case:?}: an allowed create mints a connector, got {parsed}"
    );
}

/// The SCOPE half of the decision: the session policy `sts:AssumeRole` was called with
/// caps what the session may do, so a credential minted under a scope that does not allow
/// the action is refused even though the role's own policy allows the whole service —
/// "the resulting session's permissions are the intersection of the role's identity-based
/// policy and the session policies" (`AssumeRoleRequest$PolicyArns`).
#[tokio::test]
async fn a_session_policy_caps_what_the_roles_own_policy_grants() {
    let (router, _engine) = router_with_identity();
    let role = "scoped-session";
    let case = "a role allowed the whole service";
    let role_arn = create_role(&router, case, role).await;
    put_role_policy(
        &router,
        case,
        role,
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": format!("{ACTION_PREFIX}:*"),
                "Resource": "*",
            }]
        })
        .to_string(),
    )
    .await;
    let directory_id = "d-00000000f1";
    given_registration(&router, directory_id).await;

    // A scope that allows another action of this service and nothing else.
    let scope = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": format!("{ACTION_PREFIX}:ListConnectors"),
            "Resource": "*",
        }]
    })
    .to_string();
    let (scoped_key, session_name) = assume_role_scoped(&router, case, &role_arn, Some(&scope)).await;
    let case = "the scoped session, on an action the scope does not allow";
    let (status, code, parsed) = call_as(
        &router,
        &scoped_key,
        "POST",
        "/connectors",
        Some(&connector_body(directory_id, "scoped-refused")),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (403, "AccessDeniedException"),
        "case {case:?}: a scope that does not allow the action refuses it, \
         got {status} / {code:?} / {parsed}"
    );
    let action = format!("{ACTION_PREFIX}:CreateConnector");
    let expected = refusal_sentence(
        &format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{session_name}"),
        &action,
        &connector_collection(),
        &no_session_policy_clause(&action),
    );
    assert_eq!(
        parsed["Message"].as_str().or(parsed["message"].as_str()),
        Some(expected.as_str()),
        "case {case:?}: expected {expected:?}, got {parsed}"
    );

    // The same scope ALLOWS the action it names, so the cap is the scope's content and not
    // the mere presence of one.
    let case = "the scoped session, on the action the scope allows";
    let (status, code, parsed) = call_as(&router, &scoped_key, "GET", "/connectors", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the action the scope allows is served, got {status} / {code:?} / \
         {parsed}"
    );

    // And a session the same role mints with NO scope is capped by nothing, so the role's
    // own grant stands.
    let case = "an unscoped session of the same role";
    let (unscoped_key, _) = assume_role_scoped(&router, case, &role_arn, None).await;
    let (status, code, parsed) = call_as(
        &router,
        &unscoped_key,
        "POST",
        "/connectors",
        Some(&connector_body(directory_id, "unscoped-allowed")),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: an unscoped session keeps the role's own grant, got {status} / \
         {code:?} / {parsed}"
    );
}

/// A policy allowing the whole of this service and explicitly denying ONE of its actions:
/// the shape a least-privilege document carves an action out with, and what IAM's
/// explicit-deny clause is about.
fn allow_service_but_deny(action: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
            { "Effect": "Deny", "Action": action, "Resource": "*" },
        ]
    })
}

/// One row of the table of IAM's REASONS: what the role's own documents say, what the scope
/// the session was minted under says, what the role's permissions boundary says, and the
/// clause the refusal must carry — `None` where the call must be served.
struct ReasonRow {
    case: &'static str,
    /// The role the call signs as a session of. One per row, so the documents of one row
    /// cannot decide another.
    role: &'static str,
    /// The role's inline policy, or `None` for a role carrying none at all.
    policy: Option<Value>,
    /// The inline session policy `sts:AssumeRole` scopes the credential to.
    scope: Option<Value>,
    /// The role's permissions boundary.
    boundary: Option<Value>,
    /// The clause the refusal must name, or `None` where the call is served.
    reason: Option<String>,
}

/// The rows of the reason table: one per rule IAM can withhold a grant by, over each of the
/// three documents the decision reads — the principal's own, the scope the credential was
/// minted under and the permissions boundary — in both the shapes each can withhold by: an
/// explicit `Deny` of the action, and an absence of any grant for it.
fn reason_rows() -> Vec<ReasonRow> {
    let action = format!("{ACTION_PREFIX}:CreateConnector");
    let another = format!("{ACTION_PREFIX}:GetConnector");
    let whole_service = allow(json!(format!("{ACTION_PREFIX}:*")));
    vec![
        ReasonRow {
            case: "a role carrying no document at all",
            role: "reason-no-document",
            policy: None,
            scope: None,
            boundary: None,
            reason: Some(no_identity_policy_clause(&action)),
        },
        ReasonRow {
            case: "a document allowing only another action of this service",
            role: "reason-other-action",
            policy: Some(allow(json!(another.clone()))),
            scope: None,
            boundary: None,
            reason: Some(no_identity_policy_clause(&action)),
        },
        ReasonRow {
            case: "a document allowing the whole service and denying this action",
            role: "reason-explicit-deny",
            policy: Some(allow_service_but_deny(&action)),
            scope: None,
            boundary: None,
            reason: Some(EXPLICIT_DENY_IN_IDENTITY_POLICY.to_string()),
        },
        ReasonRow {
            case: "a document that does nothing but deny this action",
            role: "reason-deny-alone",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{ "Effect": "Deny", "Action": action.clone(), "Resource": "*" }]
            })),
            scope: None,
            boundary: None,
            reason: Some(EXPLICIT_DENY_IN_IDENTITY_POLICY.to_string()),
        },
        ReasonRow {
            case: "a scope allowing only another action of this service",
            role: "reason-scope-other",
            policy: Some(whole_service.clone()),
            scope: Some(allow(json!(another.clone()))),
            boundary: None,
            reason: Some(no_session_policy_clause(&action)),
        },
        ReasonRow {
            case: "a scope allowing the whole service and denying this action",
            role: "reason-scope-deny",
            policy: Some(whole_service.clone()),
            scope: Some(allow_service_but_deny(&action)),
            boundary: None,
            reason: Some(EXPLICIT_DENY_IN_SESSION_POLICY.to_string()),
        },
        ReasonRow {
            case: "a boundary allowing only another action of this service",
            role: "reason-boundary-other",
            policy: Some(whole_service.clone()),
            scope: None,
            boundary: Some(allow(json!(another))),
            reason: Some(no_permissions_boundary_clause(&action)),
        },
        ReasonRow {
            case: "a boundary allowing the whole service and denying this action",
            role: "reason-boundary-deny",
            policy: Some(whole_service.clone()),
            scope: None,
            boundary: Some(allow_service_but_deny(&action)),
            reason: Some(EXPLICIT_DENY_IN_PERMISSIONS_BOUNDARY.to_string()),
        },
        ReasonRow {
            case: "a document allowing exactly this action",
            role: "reason-allowed",
            policy: Some(allow(json!(action))),
            scope: None,
            boundary: None,
            reason: None,
        },
    ]
}

/// THE RULE about the WORDS of a denial: IAM's refusal names the principal that signed, the
/// action and the resource it asked for, and then WHICH of its rules withheld the grant — an
/// explicit `Deny` in the principal's own documents, in the scope the credential was minted
/// under or in its permissions boundary, or the absence of any grant in one of those three.
/// The clause is a fact about the document that withheld it, so a client that reads the
/// denial learns which one to change.
#[tokio::test]
async fn the_refusal_names_which_of_iams_rules_withheld_the_grant() {
    let (router, _engine) = router_with_identity();
    let directory_id = "d-00000000a1";
    given_registration(&router, directory_id).await;
    let action = format!("{ACTION_PREFIX}:CreateConnector");

    for (index, row) in reason_rows().into_iter().enumerate() {
        let case = row.case;
        let role_arn = create_role(&router, case, row.role).await;
        if let Some(policy) = &row.policy {
            put_role_policy(&router, case, row.role, &policy.to_string()).await;
        }
        if let Some(boundary) = &row.boundary {
            given_permissions_boundary(&router, case, row.role, &boundary.to_string()).await;
        }
        let scope = row.scope.as_ref().map(ToString::to_string);
        let (session_key, session_name) =
            assume_role_scoped(&router, case, &role_arn, scope.as_deref()).await;

        // A fresh `ClientToken` per row, so no row is answered from the idempotency record
        // of an earlier one.
        let (status, code, parsed) = call_as(
            &router,
            &session_key,
            "POST",
            "/connectors",
            Some(&connector_body(directory_id, &format!("reason-token-{index}"))),
        )
        .await;
        match &row.reason {
            None => {
                assert_eq!(
                    status, 202,
                    "case {case:?}: an allowed create is served, got {status} / {code:?} / \
                     {parsed}"
                );
                assert!(
                    parsed["ConnectorArn"].as_str().is_some(),
                    "case {case:?}: an allowed create mints a connector, got {parsed}"
                );
            }
            Some(reason) => {
                assert_eq!(
                    (status, code.as_str()),
                    (403, "AccessDeniedException"),
                    "case {case:?}: expected HTTP 403 AccessDeniedException, got {status} / \
                     {code:?} / {parsed}"
                );
                let expected = refusal_sentence(
                    &format!(
                        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{session_name}",
                        row.role
                    ),
                    &action,
                    &connector_collection(),
                    reason,
                );
                assert_eq!(
                    parsed["Message"].as_str().or(parsed["message"].as_str()),
                    Some(expected.as_str()),
                    "case {case:?}: expected IAM's wording {expected:?}, got {parsed}"
                );
                assert!(
                    parsed.get("ConnectorArn").is_none(),
                    "case {case:?}: a refused create mints no connector, got {parsed}"
                );
            }
        }
    }
}

/// Post one awsQuery form body to `signing_name` as the account's own credential, and
/// answer `(status, body)`. IAM and STS both speak that protocol, so one helper drives
/// both.
async fn query(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let host = format!("{signing_name}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("the request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The text of the first `<name>…</name>` element of an XML body.
fn element(case: &str, body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let (_, rest) = body
        .split_once(&open)
        .unwrap_or_else(|| panic!("case {case:?}: the answer must carry <{name}>, got {body}"));
    let (text, _) = rest
        .split_once(&close)
        .unwrap_or_else(|| panic!("case {case:?}: <{name}> must be closed, got {body}"));
    text.to_string()
}

/// A trust policy letting the account assume the role, which is what `sts:AssumeRole`
/// checks before it mints a session.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
            "Action": "sts:AssumeRole",
        }]
    })
    .to_string()
}

/// Create one IAM role through `iam:CreateRole` and answer its ARN.
async fn create_role(router: &MultiServiceRouter, case: &str, role: &str) -> String {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={}&AssumeRolePolicyDocument={}",
            encode_label(role),
            encode_label(&trust_policy())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({role}) must answer 200, got {status} with {body}"
    );
    element(case, &body, "Arn")
}

/// Embed one inline policy in `role` through `iam:PutRolePolicy`.
async fn put_role_policy(router: &MultiServiceRouter, case: &str, role: &str, document: &str) {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={}&PolicyName=probe\
             &PolicyDocument={}",
            encode_label(role),
            encode_label(document)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy({role}) must answer 200, got {status} with {body}"
    );
}

/// Attach `boundary` to `role` as its permissions boundary, through the managed policy a
/// boundary is attached as ("the ARN of the managed policy that is used to set the
/// permissions boundary", `PutRolePermissionsBoundary`).
async fn given_permissions_boundary(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    boundary: &str,
) {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=CreatePolicy&Version=2010-05-08&PolicyName={}&PolicyDocument={}",
            encode_label(&format!("{role}-boundary")),
            encode_label(boundary)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePolicy({role}-boundary) must answer 200, got {status} with {body}"
    );
    let policy_arn = element(case, &body, "Arn");
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=PutRolePermissionsBoundary&Version=2010-05-08&RoleName={}\
             &PermissionsBoundary={}",
            encode_label(role),
            encode_label(&policy_arn)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePermissionsBoundary({role}) must answer 200, got {status} with \
         {body}"
    );
}

/// Assume `role_arn` through `sts:AssumeRole` and answer the access key id of the session
/// it minted, beside the session name it was minted for.
async fn assume_role(router: &MultiServiceRouter, case: &str, role_arn: &str) -> (String, String) {
    assume_role_scoped(router, case, role_arn, None).await
}

/// [`assume_role`], with the inline session policy the mint scopes the credential to —
/// `None` for a session no scope caps.
async fn assume_role_scoped(
    router: &MultiServiceRouter,
    case: &str,
    role_arn: &str,
    scope: Option<&str>,
) -> (String, String) {
    let session = match scope {
        Some(_) => "scoped-session",
        None => "assumed-session",
    };
    let mut form = format!(
        "Action=AssumeRole&Version=2011-06-15&RoleArn={}&RoleSessionName={session}",
        encode_label(role_arn)
    );
    if let Some(scope) = scope {
        form.push_str(&format!("&Policy={}", encode_label(scope)));
    }
    let (status, body) = query(router, "sts", form).await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole({role_arn}) must answer 200, got {status} with {body}"
    );
    (element(case, &body, "AccessKeyId"), session.to_string())
}
