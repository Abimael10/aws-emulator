// SPDX-License-Identifier: CC0-1.0

//! WHO may reach X-Ray's operations, driven end to end over the real wire path (a
//! restJson1 request through the multi-service router into the mounted service), so
//! what is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: every request to this service is authorized against the IAM
//! identity-based policies in force on the principal its credentials name BEFORE the
//! operation reads or writes anything. The action is `xray:<OperationName>` and the
//! resource is the ARN of this service the request's input names, or IAM's
//! any-resource wildcard for a request that names none. An explicit `Deny` covering
//! that action wins over any `Allow`, and a request no readable `Allow` covers falls
//! to IAM's default deny; either way the answer is HTTP 403 `AccessDeniedException`
//! naming the principal, the action and — when the request addressed one — the
//! resource, and nothing is created, changed or reported. A request a matching
//! `Allow` covers reaches the operation's own answer, whatever that answer is.
//!
//! The user, the role, their policies and the session live in the IAM and STS stores
//! of the SAME shared engine — the stores those services' own handlers write — so the
//! documents under test are the ones `iam:PutUserPolicy` stores and the principal a
//! refusal blames is the one `sts:GetCallerIdentity` answers for the key the request
//! signed with.
//!
//! The tables span what the rule generalises over: the operation (a reporting query, a
//! write, an ingest, a retrieval, a tag call and a graph query), the document in force
//! (none at all, another action of this service, another service's actions, an
//! explicit deny beside an allow, an allow of exactly the action, the same allow
//! spelled in another case, an allow of the whole service and an allow of
//! everything), the principal (an IAM user's long-term access key, an assumed-role
//! session and the account's own published credential), and the resource an allow or a
//! deny is scoped to (every resource, exactly the resource the request names, another
//! resource of this service).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;

use crate::xray_wire::{trace_id, Answer, Xray, DEFAULT_ACCESS_KEY_ID};

/// This service's IAM action prefix: its `arnNamespace`, which is also its
/// `sigv4Name` (`smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-xray-awsxray/smithy-model/com-amazonaws-xray-awsxray.json")`).
const ACTION_PREFIX: &str = "xray";

/// The error shape an authorization refusal carries, and the status it carries it at.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The access key id of the IAM user the policy rows sign with, and the user itself.
const USER_KEY_ID: &str = "AKIAEXAMPLEXRAYUSER1";
const USER_NAME: &str = "trace-reader";

/// The access key id of the assumed-role session the role row signs with, the role it
/// is assumed from and the session's own name.
const SESSION_KEY_ID: &str = "ASIAEXAMPLEXRAYSESS1";
const ROLE: &str = "trace-retriever";
const SESSION: &str = "trace-session";

/// The name of the inline policy the user and the role carry.
const POLICY_NAME: &str = "inline";

/// How long the seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// This service beside the IAM and STS services over ONE shared engine, so the
/// policies and the session a case seeds are the ones those services hold.
fn emulator_with_identity() -> (Xray, Arc<engine::InMemoryEngine>) {
    let (mut emulator, engine) = Xray::with_engine();
    emulator.mount(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    emulator.mount(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (emulator, engine)
}

/// The IAM user's ARN, as IAM holds it and `sts:GetCallerIdentity` reports it.
fn user_arn() -> String {
    format!(
        "arn:aws:iam::{}:user/{USER_NAME}",
        ::engine::emulator_account_id()
    )
}

/// The ARN the account holds the role under — the identity whose policies an
/// assumed-role session's request is judged on.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", ::engine::emulator_account_id())
}

/// The principal ARN the seeded session speaks for — what its refusals must name.
fn session_arn() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{ROLE}/{SESSION}",
        ::engine::emulator_account_id()
    )
}

/// Record the IAM user [`USER_KEY_ID`] belongs to, as `iam:CreateUser` and
/// `iam:CreateAccessKey` record one: the user, and the key record that maps the access
/// key id to it. NO policy is attached.
fn given_user_key(engine: &Arc<engine::InMemoryEngine>) {
    let minted = ::aws_smithy_types::DateTime::from_secs(0);
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.users.insert(
        iam::name_key(USER_NAME),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User {
            path: "/".to_string(),
            user_name: USER_NAME.to_string(),
            user_id: "AIDAEXAMPLEXRAYUSER".to_string(),
            arn: user_arn(),
            create_date: minted,
            password_last_used: None,
            permissions_boundary: None,
            tags: None,
        },
    );
    store.access_keys.insert(
        USER_KEY_ID.to_string(),
        iam::AccessKeyRecord {
            user_key: iam::name_key(USER_NAME),
            status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
            create_date: minted,
        },
    );
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
            role_id: "AROAEXAMPLEXRAYROLE".to_string(),
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
                account: ::engine::emulator_account_id().to_string(),
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

/// One inline policy holding `document`, as `iam:PutUserPolicy` stores one.
fn inline(document: &Value) -> BTreeMap<String, iam::InlinePolicy> {
    BTreeMap::from([(
        iam::name_key(POLICY_NAME),
        iam::InlinePolicy {
            policy_name: POLICY_NAME.to_string(),
            policy_document: document.to_string(),
        },
    )])
}

/// Put `document` in force on the IAM user, replacing whatever it carried; `None`
/// takes every policy off it, as `iam:DeleteUserPolicy` does.
fn given_user_policy(engine: &Arc<engine::InMemoryEngine>, document: Option<&Value>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.user_inline_policies.remove(&iam::name_key(USER_NAME));
    if let Some(document) = document {
        store
            .user_inline_policies
            .insert(iam::name_key(USER_NAME), inline(document));
    }
}

/// The same for the role an assumed-role session is of, as `iam:PutRolePolicy` does.
fn given_role_policy(engine: &Arc<engine::InMemoryEngine>, document: Option<&Value>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.role_inline_policies.remove(&iam::name_key(ROLE));
    if let Some(document) = document {
        store
            .role_inline_policies
            .insert(iam::name_key(ROLE), inline(document));
    }
}

/// A policy allowing `actions` on every resource.
fn allow(actions: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }],
    })
}

/// A policy allowing every action of this service on every resource beside a `Deny` of
/// `action` scoped to `resource` — the shape the recorded check attaches.
fn allow_all_but(action: &str, resource: &Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "xray:*", "Resource": "*" },
            { "Effect": "Deny", "Action": action, "Resource": resource },
        ],
    })
}

/// Which of IAM's two refusals a row expects.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    /// The default deny of a request no statement allows.
    DefaultDeny,
    /// The explicit deny of a request a `Deny` statement covers.
    ExplicitDeny,
}

impl Refusal {
    /// The clause IAM closes the refusal with.
    fn reason(self, action: &str) -> String {
        match self {
            Self::DefaultDeny => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// The sentence IAM refuses a request with: who, which action, the resource the
/// request addressed when it addressed one, and which of its rules refused.
fn not_authorized(principal: &str, action: &str, resource: Option<&str>, refusal: Refusal) -> String {
    let clause = resource.map_or_else(String::new, |resource| format!(" on resource: {resource}"));
    format!(
        "User: {principal} is not authorized to perform: {action}{clause} {}",
        refusal.reason(action)
    )
}

/// Assert `answer` is the authorization refusal, carrying `message`.
fn assert_refused(case: &str, answer: &Answer, message: &str) {
    assert_eq!(
        (answer.status, answer.error_type.as_deref()),
        (ACCESS_DENIED_STATUS, Some(ACCESS_DENIED)),
        "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?} with {}",
        answer.status,
        answer.error_type,
        answer.body
    );
    assert_eq!(
        answer.message(),
        Some(message),
        "case {case:?}: expected IAM's wording, got {:?}",
        answer.message()
    );
}

/// Assert `answer` is NOT the authorization refusal: whatever the operation itself
/// answers for the request, it reached the operation.
fn assert_not_refused(case: &str, answer: &Answer) {
    assert_ne!(
        answer.status, ACCESS_DENIED_STATUS,
        "case {case:?}: an allowed request must reach the operation, got {} with {}",
        answer.status, answer.body
    );
    assert_ne!(
        answer.error_type.as_deref(),
        Some(ACCESS_DENIED),
        "case {case:?}: an allowed request must reach the operation, got {}",
        answer.body
    );
}

/// Epoch seconds now, which is what this service's time members carry.
fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs() as f64
}

/// A filter expression every group a case creates carries: the groups exist only to be
/// ADDRESSED by an authorization decision, so the simplest expression this service
/// parses is enough.
const FILTER: &str = "fault = true";

/// One operation under the gate: its name, the route it is addressed by, a request the
/// operation would otherwise answer, and the resource of this service that request
/// addresses (`None` for a request that addresses none).
struct Guarded {
    operation: &'static str,
    path: &'static str,
    body: Value,
    resource: Option<String>,
}

/// A spanning set of this service's operations, each with a request the modelled
/// constraints admit — so a refused row is refused by the gate rather than by the
/// request layer, and an allowed row reaches the operation's own answer.
///
/// `group` is an ARN of this service a tag or graph call addresses, which is what makes
/// those rows carry a resource clause.
fn guarded(group: &str) -> Vec<Guarded> {
    let end = now_seconds();
    let start = end - 3_600.0;
    vec![
        Guarded {
            operation: "GetSamplingRules",
            path: "/GetSamplingRules",
            body: json!({}),
            resource: None,
        },
        Guarded {
            operation: "StartTraceRetrieval",
            path: "/StartTraceRetrieval",
            body: json!({ "TraceIds": [trace_id(1)], "StartTime": start, "EndTime": end }),
            resource: None,
        },
        Guarded {
            operation: "CreateGroup",
            path: "/CreateGroup",
            body: json!({ "GroupName": "gated", "FilterExpression": FILTER }),
            resource: None,
        },
        Guarded {
            operation: "PutTraceSegments",
            path: "/TraceSegments",
            body: json!({ "TraceSegmentDocuments": ["{}"] }),
            resource: None,
        },
        Guarded {
            operation: "GetTraceSummaries",
            path: "/TraceSummaries",
            body: json!({ "StartTime": start, "EndTime": end }),
            resource: None,
        },
        Guarded {
            operation: "CancelTraceRetrieval",
            path: "/CancelTraceRetrieval",
            body: json!({ "RetrievalToken": "no-such-token" }),
            resource: None,
        },
        Guarded {
            operation: "ListTagsForResource",
            path: "/ListTagsForResource",
            body: json!({ "ResourceARN": group }),
            resource: Some(group.to_string()),
        },
        Guarded {
            operation: "TagResource",
            path: "/TagResource",
            body: json!({ "ResourceARN": group, "Tags": [{ "Key": "team", "Value": "qa" }] }),
            resource: Some(group.to_string()),
        },
        Guarded {
            operation: "GetServiceGraph",
            path: "/ServiceGraph",
            body: json!({ "StartTime": start, "EndTime": end, "GroupARN": group }),
            resource: Some(group.to_string()),
        },
    ]
}

/// The group this account holds, created by the account's own credential — which this
/// gate does not bound — so the tag and graph rows address an ARN this service minted.
async fn given_group(emulator: &Xray, name: &str) -> String {
    let created = emulator
        .call(
            "/CreateGroup",
            &json!({ "GroupName": name, "FilterExpression": FILTER }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the account's own credential must create the group, got {} with {}",
        created.status, created.body
    );
    created.body["Group"]["GroupARN"]
        .as_str()
        .expect("the created group carries an ARN")
        .to_string()
}

/// THE RECORDED CASE: under an `Allow` of this service the call reaches the service
/// layer; once an explicit `Deny` of exactly that action is attached on top, the
/// IDENTICAL call is refused 403 `AccessDeniedException` with IAM's wording, while
/// another action of the same service keeps being served under the same credentials;
/// and removing the `Deny` restores the pre-deny outcome.
#[tokio::test]
async fn an_explicit_deny_refuses_only_the_denied_action() {
    let (emulator, engine) = emulator_with_identity();
    given_user_key(&engine);
    let action = format!("{ACTION_PREFIX}:StartTraceRetrieval");
    let end = now_seconds();
    let request = json!({
        "TraceIds": [trace_id(7)],
        "StartTime": end - 3_600.0,
        "EndTime": end,
    });

    // The positive control: the grant lets the call reach the service layer, whatever
    // the service then answers for it.
    given_user_policy(&engine, Some(&allow(json!("xray:*"))));
    let served = emulator
        .call_as(USER_KEY_ID, "/StartTraceRetrieval", &request)
        .await;
    assert_not_refused("an allow of every action of this service", &served);
    let before = served.status;

    // The deny on top: the identical call is refused, and nothing is retrieved.
    given_user_policy(&engine, Some(&allow_all_but(&action, &json!("*"))));
    let refused = emulator
        .call_as(USER_KEY_ID, "/StartTraceRetrieval", &request)
        .await;
    assert_refused(
        "an explicit deny of this action beside an allow of the service",
        &refused,
        &not_authorized(&user_arn(), &action, None, Refusal::ExplicitDeny),
    );
    assert!(
        refused.get("RetrievalToken").is_null(),
        "a refused retrieval must mint no token, got {}",
        refused.body
    );

    // The refusal is scoped to the denied action alone: another action of the same
    // service keeps being served under the identical credentials.
    let other = emulator
        .call_as(USER_KEY_ID, "/GetSamplingRules", &json!({}))
        .await;
    assert_eq!(
        other.status, 200,
        "an action the deny does not name must keep being served, got {} with {}",
        other.status, other.body
    );

    // The deny removed: the same identity is back to the outcome of the first leg.
    given_user_policy(&engine, Some(&allow(json!("xray:*"))));
    let again = emulator
        .call_as(USER_KEY_ID, "/StartTraceRetrieval", &request)
        .await;
    assert_not_refused("the deny removed", &again);
    assert_eq!(
        again.status, before,
        "removing the deny must restore the IAM-independent outcome ({before}), got {} with {}",
        again.status, again.body
    );
}

/// RULE: a refused write performs nothing — the group a refused `CreateGroup` names is
/// not there for a later read, and a refused `TagResource` leaves the tags it would
/// have written off the resource.
#[tokio::test]
async fn a_refused_write_changes_nothing() {
    let (emulator, engine) = emulator_with_identity();
    given_user_key(&engine);
    let group = given_group(&emulator, "untouched").await;
    given_user_policy(
        &engine,
        Some(&allow_all_but("xray:CreateGroup", &json!("*"))),
    );

    let refused = emulator
        .call_as(
            USER_KEY_ID,
            "/CreateGroup",
            &json!({ "GroupName": "refused", "FilterExpression": FILTER }),
        )
        .await;
    assert_refused(
        "a denied CreateGroup",
        &refused,
        &not_authorized(
            &user_arn(),
            &format!("{ACTION_PREFIX}:CreateGroup"),
            None,
            Refusal::ExplicitDeny,
        ),
    );
    let read = emulator
        .call("/GetGroup", &json!({ "GroupName": "refused" }))
        .await;
    assert_ne!(
        read.status, 200,
        "a refused create must store no group, got {} with {}",
        read.status, read.body
    );

    given_user_policy(
        &engine,
        Some(&allow_all_but("xray:TagResource", &json!("*"))),
    );
    let refused = emulator
        .call_as(
            USER_KEY_ID,
            "/TagResource",
            &json!({ "ResourceARN": group, "Tags": [{ "Key": "team", "Value": "qa" }] }),
        )
        .await;
    assert_refused(
        "a denied TagResource",
        &refused,
        &not_authorized(
            &user_arn(),
            &format!("{ACTION_PREFIX}:TagResource"),
            Some(&group),
            Refusal::ExplicitDeny,
        ),
    );
    let tags = emulator
        .call("/ListTagsForResource", &json!({ "ResourceARN": group }))
        .await;
    assert_eq!(
        tags.status, 200,
        "the account's own credential must read the tags, got {} with {}",
        tags.status, tags.body
    );
    assert_eq!(
        tags.get("Tags"),
        &json!([]),
        "a refused tag call must write no tag, got {}",
        tags.body
    );
}

/// RULE: every operation takes the decision, against its OWN action name and the
/// resource its OWN input addresses — so the document in force decides each of them
/// alike, and a refusal names the action of the operation that was addressed.
#[tokio::test]
async fn every_operation_is_judged_on_its_own_action_and_resource() {
    let (emulator, engine) = emulator_with_identity();
    given_user_key(&engine);
    let group = given_group(&emulator, "judged").await;

    for row in guarded(&group) {
        let action = format!("{ACTION_PREFIX}:{}", row.operation);
        // (case, the document in force, the refusal the request then meets)
        let documents: Vec<(&str, Option<Value>, Option<Refusal>)> = vec![
            ("no policy at all", None, Some(Refusal::DefaultDeny)),
            (
                "an allow of another action of this service",
                Some(allow(json!("xray:GetEncryptionConfig"))),
                Some(Refusal::DefaultDeny),
            ),
            (
                "an allow of every action of another service",
                Some(allow(json!("logs:*"))),
                Some(Refusal::DefaultDeny),
            ),
            (
                "an explicit deny of this whole service beside an allow of this action",
                Some(json!({
                    "Statement": [
                        { "Effect": "Allow", "Action": action, "Resource": "*" },
                        { "Effect": "Deny", "Action": "xray:*", "Resource": "*" },
                    ],
                })),
                Some(Refusal::ExplicitDeny),
            ),
            (
                "an allow of exactly this action",
                Some(allow(json!(action))),
                None,
            ),
            (
                // IAM compares action names case-insensitively, so the spelling of
                // the allow is not what decides the request.
                "an allow of this action spelled in lower case",
                Some(allow(json!(action.to_lowercase()))),
                None,
            ),
            (
                "an allow listing this action beside another service's",
                Some(allow(json!(["logs:PutLogEvents", action]))),
                None,
            ),
            (
                "an allow of every action of this service",
                Some(allow(json!("xray:*"))),
                None,
            ),
            ("an allow of every action", Some(allow(json!("*"))), None),
        ];

        for (document_case, document, refusal) in documents {
            let case = format!("{} with {document_case}", row.operation);
            given_user_policy(&engine, document.as_ref());
            let answer = emulator.call_as(USER_KEY_ID, row.path, &row.body).await;
            match refusal {
                Some(refusal) => assert_refused(
                    &case,
                    &answer,
                    &not_authorized(&user_arn(), &action, row.resource.as_deref(), refusal),
                ),
                None => assert_not_refused(&case, &answer),
            }
        }
    }
}

/// RULE: a request that names an ARN of this service is judged against THAT resource,
/// so an allow scoped to it covers it and an allow scoped to another resource does
/// not — and a deny scoped to it refuses the call a broad allow would otherwise permit.
#[tokio::test]
async fn a_request_naming_a_resource_is_judged_against_it() {
    let (emulator, engine) = emulator_with_identity();
    given_user_key(&engine);
    let group = given_group(&emulator, "scoped").await;
    let elsewhere = given_group(&emulator, "elsewhere").await;
    let every_group = format!(
        "arn:aws:xray:us-east-1:{}:group/*",
        ::engine::emulator_account_id()
    );
    let action = format!("{ACTION_PREFIX}:ListTagsForResource");
    let request = json!({ "ResourceARN": group });

    // (case, the document in force, the refusal the request then meets)
    let rows: Vec<(&str, Value, Option<Refusal>)> = vec![
        ("an allow of every resource", allow(json!(action)), None),
        (
            "an allow of exactly the resource the request names",
            json!({
                "Statement": [{ "Effect": "Allow", "Action": action, "Resource": group }],
            }),
            None,
        ),
        (
            "an allow of every group of this account",
            json!({
                "Statement": [{ "Effect": "Allow", "Action": action, "Resource": every_group }],
            }),
            None,
        ),
        (
            "an allow scoped to another group only",
            json!({
                "Statement": [{ "Effect": "Allow", "Action": action, "Resource": elsewhere }],
            }),
            Some(Refusal::DefaultDeny),
        ),
        (
            "a deny scoped to the resource the request names",
            allow_all_but(&action, &json!(group)),
            Some(Refusal::ExplicitDeny),
        ),
        (
            "a deny scoped to another group only",
            allow_all_but(&action, &json!(elsewhere)),
            None,
        ),
    ];

    for (case, document, refusal) in rows {
        given_user_policy(&engine, Some(&document));
        let answer = emulator
            .call_as(USER_KEY_ID, "/ListTagsForResource", &request)
            .await;
        match refusal {
            Some(refusal) => assert_refused(
                case,
                &answer,
                &not_authorized(&user_arn(), &action, Some(&group), refusal),
            ),
            None => assert_eq!(
                answer.status, 200,
                "case {case:?}: an allowed read must be served, got {} with {}",
                answer.status, answer.body
            ),
        }
    }
}

/// RULE: the principal a request speaks for is the one its access key id names, so an
/// assumed-role session is judged on the ROLE's policies and blamed under the SESSION's
/// ARN — while the account's own published credential, which the IAM store holds no
/// policies for, is not bounded by this gate at all.
#[tokio::test]
async fn the_principal_judged_is_the_one_the_access_key_names() {
    let (emulator, engine) = emulator_with_identity();
    given_user_key(&engine);
    given_session(&engine);
    let action = format!("{ACTION_PREFIX}:GetSamplingRules");
    let request = json!({});

    // Neither the user nor the role carries a policy: both are refused, each named by
    // its own principal ARN.
    given_user_policy(&engine, None);
    given_role_policy(&engine, None);
    for (case, key_id, principal) in [
        ("an IAM user's long-term key", USER_KEY_ID, user_arn()),
        ("an assumed-role session", SESSION_KEY_ID, session_arn()),
    ] {
        let answer = emulator.call_as(key_id, "/GetSamplingRules", &request).await;
        assert_refused(
            case,
            &answer,
            &not_authorized(&principal, &action, None, Refusal::DefaultDeny),
        );
    }

    // The account's own published credential is a key neither store holds a record
    // for, which is the position account root credentials are in.
    let served = emulator
        .call_as(DEFAULT_ACCESS_KEY_ID, "/GetSamplingRules", &request)
        .await;
    assert_eq!(
        served.status, 200,
        "the account's own credential must not be bounded by this gate, got {} with {}",
        served.status, served.body
    );

    // A grant on the ROLE lifts the session's refusal, and only the session's: the
    // user carries no policy of its own.
    given_role_policy(&engine, Some(&allow(json!("xray:*"))));
    let served = emulator
        .call_as(SESSION_KEY_ID, "/GetSamplingRules", &request)
        .await;
    assert_eq!(
        served.status, 200,
        "a grant on the role must serve its session, got {} with {}",
        served.status, served.body
    );
    let refused = emulator.call_as(USER_KEY_ID, "/GetSamplingRules", &request).await;
    assert_refused(
        "the user under a grant on the role",
        &refused,
        &not_authorized(&user_arn(), &action, None, Refusal::DefaultDeny),
    );
}
