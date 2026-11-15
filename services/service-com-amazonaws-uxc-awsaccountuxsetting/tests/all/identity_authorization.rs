// SPDX-License-Identifier: CC0-1.0

//! WHO may reach this service's operations, driven end to end over the real wire path
//! (a `restJson1` request through the multi-service router into the mounted service), so
//! what is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: `AccessDeniedException` (HTTP 403) is a service-common error of
//! this model (`smithy_query(query="service_info")` lists it under `common_errors`), so
//! EVERY one of the three operations evaluates the identity-based IAM policies in force on
//! the principal its credentials name against `uxc:<OperationName>` BEFORE it reads or
//! writes anything — a request no statement allows (IAM's default deny), or one an explicit
//! `Deny` covers, is answered HTTP 403 `AccessDeniedException` and performs nothing, while a
//! request a matching `Allow` covers reaches the operation's own answer. The two operations
//! that address the account's customization settings are judged against that account-level
//! singleton's ARN and name it in the refusal; the catalog listing addresses no resource of
//! its own and names none.
//!
//! Because the decision is taken before the write, a DENIED update leaves the stored
//! setting exactly as it was — the effect-absence guarantee a client's least-privilege test
//! depends on, asserted below by reading the record back.
//!
//! The user, the role, their policies and the session live in the IAM and STS stores of the
//! SAME shared engine — the stores those services' own handlers write — so the documents
//! under test are the ones `iam:PutUserPolicy` stores and the principal a refusal blames is
//! the one `sts:GetCallerIdentity` answers for the key the request signed with.
//!
//! The tables span what the rule generalises over: the operation (all three), the document
//! in force (none at all, another action of this service, another service's actions, an
//! explicit deny beside an allow, an allow of exactly the action, of the action in another
//! case, of the whole service and of everything), the principal (an IAM user's long-term
//! access key, an assumed-role session and the account's own published credential), the
//! resource an allow is scoped to (every resource, this service's account resources, the
//! singleton itself, another resource) and the setting a denied update tried to change.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{build_router_with_identity, call_as, DEFAULT_ACCESS_KEY_ID};

/// The IAM user the user rows sign as, and the access key id it owns.
const USER_NAME: &str = "console-reader";
const USER_KEY_ID: &str = "AKIAEXAMPLEUSERKEY01";

/// The role the session rows are assumed from, the session's own name, and the access key
/// id STS minted for it.
const ROLE: &str = "console-role";
const SESSION: &str = "console-session";
const SESSION_KEY_ID: &str = "ASIAEXAMPLESESSION01";

/// The name of the inline policy a row attaches.
const POLICY_NAME: &str = "inline";

/// How long the seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// This service's IAM action prefix: its `arnNamespace`, which
/// `smithy_query(query="service_info")` gives as `uxc` — the same string as its `sigv4Name`.
const ACTION_PREFIX: &str = "uxc";

/// The ARN of the account-level singleton the customization settings live in, written out
/// here rather than taken from the service, so what the suite asserts is the ARN form and
/// not the emulator's own spelling of it. The account id comes from the emulator's account
/// contract.
fn singleton_arn() -> String {
    format!(
        "arn:aws:{ACTION_PREFIX}::{}:account-customizations",
        ::engine::emulator_account_id()
    )
}

/// The IAM user's ARN, as IAM holds it and `sts:GetCallerIdentity` reports it.
fn user_arn() -> String {
    format!(
        "arn:aws:iam::{}:user/{USER_NAME}",
        ::engine::emulator_account_id()
    )
}

/// The ARN the account holds the role under — the identity whose policies an assumed-role
/// session's request is judged on.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/{ROLE}",
        ::engine::emulator_account_id()
    )
}

/// The principal ARN the seeded session speaks for — what a refusal must name.
fn session_arn() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{ROLE}/{SESSION}",
        ::engine::emulator_account_id()
    )
}

/// Record the IAM user [`USER_KEY_ID`] belongs to, as `iam:CreateUser` and
/// `iam:CreateAccessKey` record one: the user, and the key record that maps the access key
/// id to it. NO policy is attached — the position a freshly created user is in.
fn given_user_key(engine: &Arc<engine::InMemoryEngine>) {
    let minted = ::aws_smithy_types::DateTime::from_secs(0);
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.users.insert(
        iam::name_key(USER_NAME),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User {
            path: "/".to_string(),
            user_name: USER_NAME.to_string(),
            user_id: "AIDAEXAMPLECONSOLEUSER".to_string(),
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
    let account = ::engine::emulator_account_id();
    let role_arn = role_arn();
    let iam_store = iam::service_state(&**engine).expect("the IAM store");
    iam_store.lock().roles.insert(
        iam::name_key(ROLE),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: ROLE.to_string(),
            role_id: "AROAEXAMPLECONSOLEROLE".to_string(),
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
                account: account.to_string(),
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

/// One inline policy holding `document`, as `iam:PutUserPolicy` / `iam:PutRolePolicy` store
/// one.
fn inline(document: &Value) -> BTreeMap<String, iam::InlinePolicy> {
    BTreeMap::from([(
        iam::name_key(POLICY_NAME),
        iam::InlinePolicy {
            policy_name: POLICY_NAME.to_string(),
            policy_document: document.to_string(),
        },
    )])
}

/// Attach `document` to the user as its inline policy, replacing whatever it carried.
/// `None` takes every policy off it, as `iam:DeleteUserPolicy` does.
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

/// The same for the role an assumed-role session speaks for.
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
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }]
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

/// The documents a row puts in force on the principal, and which of IAM's refusals the
/// request then meets — `None` for a request a matching `Allow` permits.
fn documents_in_force(
    action: &str,
    other: &str,
) -> Vec<(&'static str, Option<Value>, Option<Refusal>)> {
    vec![
        ("no policy at all", None, Some(Refusal::DefaultDeny)),
        (
            "an allow of another action of this service, but not this one",
            Some(allow(json!(other))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an allow of every action of another service",
            Some(allow(json!("iam:*"))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an explicit deny of this whole service beside an allow of exactly this action",
            Some(json!({
                "Statement": [
                    { "Effect": "Allow", "Action": action, "Resource": "*" },
                    { "Effect": "Deny", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
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
            // IAM compares action names case-insensitively, so the spelling of the allow is
            // not what decides the request.
            "an allow of this action spelled in lower case",
            Some(allow(json!(action.to_lowercase()))),
            None,
        ),
        (
            "an allow listing this action beside another service's",
            Some(allow(json!(["iam:GetUser", action]))),
            None,
        ),
        (
            "an allow of every action of this service",
            Some(allow(json!(format!("{ACTION_PREFIX}:*")))),
            None,
        ),
        ("an allow of every action", Some(allow(json!("*"))), None),
    ]
}

/// One operation of this service as a row of the tables below: the operation name, the HTTP
/// method and URI the model binds it to, a request it would SERVE, the resource clause its
/// refusal carries, and the response member a served call answers with.
struct Addressed {
    operation: &'static str,
    method: &'static str,
    uri: &'static str,
    body: Option<Value>,
    clause: String,
    served_member: &'static str,
}

/// Every operation of this service, each with a request it would otherwise serve — so an
/// authorized row reaches a 200 rather than another refusal.
///
/// The method and URI of each are the model's own HTTP bindings
/// (`smithy_query(query="digest")`).
fn addressed_operations() -> Vec<Addressed> {
    let on_singleton = format!(" on resource: {}", singleton_arn());
    vec![
        Addressed {
            operation: "GetAccountCustomizations",
            method: "GET",
            uri: "/v1/account-customizations",
            body: None,
            clause: on_singleton.clone(),
            served_member: "accountColor",
        },
        Addressed {
            operation: "UpdateAccountCustomizations",
            method: "PATCH",
            uri: "/v1/account-customizations",
            body: Some(json!({ "accountColor": "teal" })),
            clause: on_singleton,
            served_member: "accountColor",
        },
        Addressed {
            operation: "ListServices",
            method: "GET",
            uri: "/v1/services",
            // The catalog listing addresses no resource of its own, so IAM judges it
            // against every resource and the refusal names none.
            body: None,
            clause: String::new(),
            served_member: "services",
        },
    ]
}

/// Drive one addressed operation as the principal `access_key_id` speaks for.
async fn call_addressed(
    router: &MultiServiceRouter,
    access_key_id: &str,
    addressed: &Addressed,
) -> (u16, String, Value) {
    call_as(
        router,
        access_key_id,
        addressed.method,
        addressed.uri,
        addressed.body.clone(),
    )
    .await
}

/// Require the refusal `answer` to be IAM's: HTTP 403, the modelled error shape, and the
/// sentence `expected_message` word for word.
fn assert_refusal(case: &str, status: u16, error_type: &str, answer: &Value, expected: &str) {
    assert_eq!(
        status, 403,
        "case {case:?}: expected HTTP 403, got {status} {answer}"
    );
    assert_eq!(
        error_type, "AccessDeniedException",
        "case {case:?}: expected an AccessDeniedException, got {error_type:?} {answer}"
    );
    let message = answer
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no message: {answer}"));
    assert_eq!(
        message, expected,
        "case {case:?}: expected IAM's wording, got {message:?}"
    );
}

/// THE RULE: every operation is judged against its own `uxc:<OperationName>` action on the
/// resource it addresses before it reads or writes anything; a refusal is HTTP 403
/// `AccessDeniedException` naming the principal, the action, that resource and which of
/// IAM's rules refused it, and an allowed call reaches the operation's own answer.
#[tokio::test]
async fn every_operation_is_authorized_against_its_own_action() {
    for addressed in addressed_operations() {
        let action = format!("{ACTION_PREFIX}:{}", addressed.operation);
        // Another action of this service, which one row grants instead of the one under
        // test.
        let other = addressed_operations()
            .iter()
            .map(|row| format!("{ACTION_PREFIX}:{}", row.operation))
            .find(|candidate| *candidate != action)
            .expect("this service has more than one operation");

        for (row, document, refused) in documents_in_force(&action, &other) {
            let case = format!("{} with {row}", addressed.operation);
            // A fresh store, user and policy per row: no row can pass on another's write.
            let (router, engine) = build_router_with_identity();
            given_user_key(&engine);
            given_user_policy(&engine, document.as_ref());

            let (status, error_type, answer) =
                call_addressed(&router, USER_KEY_ID, &addressed).await;
            match refused {
                Some(refusal) => {
                    assert_refusal(
                        &case,
                        status,
                        &error_type,
                        &answer,
                        &format!(
                            "User: {} is not authorized to perform: {action}{} {}",
                            user_arn(),
                            addressed.clause,
                            refusal.reason(&action)
                        ),
                    );
                    // The refusal carries no part of the operation's own answer: an
                    // unauthorized caller is told nothing about the settings.
                    assert!(
                        answer.get(addressed.served_member).is_none(),
                        "case {case:?}: a refused call must report no {}, got {answer}",
                        addressed.served_member
                    );
                }
                None => {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: an allowed request must be served, got {status} {answer}"
                    );
                    assert!(
                        answer.get(addressed.served_member).is_some(),
                        "case {case:?}: a served call must report {}, got {answer}",
                        addressed.served_member
                    );
                }
            }
        }
    }
}

/// RULE: an assumed-role session is judged on the ROLE's own policies and the refusal names
/// the session's ARN — the same rule the user rows exercise, under the other principal form.
#[tokio::test]
async fn an_assumed_role_session_is_judged_on_the_roles_policies() {
    for addressed in addressed_operations() {
        let action = format!("{ACTION_PREFIX}:{}", addressed.operation);
        // (case, the document in force, the refusal it meets)
        let rows: Vec<(&str, Option<Value>, Option<Refusal>)> = vec![
            ("no policy at all", None, Some(Refusal::DefaultDeny)),
            (
                "an explicit deny of this service",
                Some(json!({
                    "Statement": [
                        { "Effect": "Deny", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
                    ],
                })),
                Some(Refusal::ExplicitDeny),
            ),
            (
                "an allow of every action of this service",
                Some(allow(json!(format!("{ACTION_PREFIX}:*")))),
                None,
            ),
        ];

        for (row, document, refused) in rows {
            let case = format!("{} as a session with {row}", addressed.operation);
            let (router, engine) = build_router_with_identity();
            given_session(&engine);
            given_role_policy(&engine, document.as_ref());

            let (status, error_type, answer) =
                call_addressed(&router, SESSION_KEY_ID, &addressed).await;
            match refused {
                Some(refusal) => assert_refusal(
                    &case,
                    status,
                    &error_type,
                    &answer,
                    &format!(
                        "User: {} is not authorized to perform: {action}{} {}",
                        session_arn(),
                        addressed.clause,
                        refusal.reason(&action)
                    ),
                ),
                None => assert_eq!(
                    status, 200,
                    "case {case:?}: an allowed request must be served, got {status} {answer}"
                ),
            }
        }
    }
}

/// RULE: the account's own published credential — a key neither STS nor IAM holds a record
/// for — is not a principal the IAM store holds policies for, so it is served whatever the
/// unprivileged user carries.
#[tokio::test]
async fn the_accounts_own_credential_is_served() {
    for addressed in addressed_operations() {
        let (router, engine) = build_router_with_identity();
        given_user_key(&engine);
        // The user carries nothing at all, which is what refuses it above.
        given_user_policy(&engine, None);

        let (status, _, answer) = call_addressed(&router, DEFAULT_ACCESS_KEY_ID, &addressed).await;
        assert_eq!(
            status, 200,
            "case {:?}: the account's own credential must be served, got {status} {answer}",
            addressed.operation
        );
        assert!(
            answer.get(addressed.served_member).is_some(),
            "case {:?}: a served call must report {}, got {answer}",
            addressed.operation,
            addressed.served_member
        );
    }
}

/// RULE: the decision is taken BEFORE the write, so an update a policy does not allow
/// changes nothing — whichever setting it tried to change.
#[tokio::test]
async fn a_denied_update_leaves_the_stored_setting_intact() {
    // (case, the update an unprivileged principal attempts)
    let attempts: Vec<(&str, Value)> = vec![
        ("the colour", json!({ "accountColor": "teal" })),
        ("the visible services", json!({ "visibleServices": ["s3"] })),
        (
            "the visible Regions",
            json!({ "visibleRegions": ["eu-west-1"] }),
        ),
        (
            "every setting at once",
            json!({
                "accountColor": "pink",
                "visibleServices": ["lambda"],
                "visibleRegions": ["us-west-2"],
            }),
        ),
        (
            "a reset of both list settings",
            json!({ "visibleServices": null, "visibleRegions": null }),
        ),
    ];
    // What the account is primed with first, through the credential that IS allowed.
    let baseline = json!({
        "accountColor": "yellow",
        "visibleServices": ["ec2", "s3"],
        "visibleRegions": ["eu-west-1", "us-east-1"],
    });

    for (case, attempt) in attempts {
        let (router, engine) = build_router_with_identity();
        given_user_key(&engine);
        given_user_policy(&engine, None);

        let (status, _, primed) = call_as(
            &router,
            DEFAULT_ACCESS_KEY_ID,
            "PATCH",
            "/v1/account-customizations",
            Some(baseline.clone()),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: priming the record must succeed, got {status} {primed}"
        );

        let (status, error_type, answer) = call_as(
            &router,
            USER_KEY_ID,
            "PATCH",
            "/v1/account-customizations",
            Some(attempt.clone()),
        )
        .await;
        let action = format!("{ACTION_PREFIX}:UpdateAccountCustomizations");
        assert_refusal(
            case,
            status,
            &error_type,
            &answer,
            &format!(
                "User: {} is not authorized to perform: {action} on resource: {} because no \
                 identity-based policy allows the {action} action",
                user_arn(),
                singleton_arn()
            ),
        );

        let (status, _, read_back) = call_as(
            &router,
            DEFAULT_ACCESS_KEY_ID,
            "GET",
            "/v1/account-customizations",
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading the record back must succeed, got {status} {read_back}"
        );
        assert_eq!(
            read_back, primed,
            "case {case:?}: a denied update must leave the record as it was ({primed}), got \
             {read_back}"
        );
    }
}

/// RULE: IAM compares the resource of a statement against the ARN the request is judged on,
/// so an allow scoped to the customization singleton covers the two operations that address
/// it and an allow scoped to anything else does not.
#[tokio::test]
async fn the_customization_operations_are_judged_against_the_singletons_arn() {
    let singleton = singleton_arn();
    let other_resource = format!(
        "arn:aws:{ACTION_PREFIX}::{}:something-else",
        ::engine::emulator_account_id()
    );
    // (case, the Resource the allow is scoped to, whether it covers the singleton)
    let rows: Vec<(&str, Value, bool)> = vec![
        ("every resource", json!("*"), true),
        ("exactly the singleton", json!(singleton.clone()), true),
        (
            "every resource of this service",
            json!(format!("arn:aws:{ACTION_PREFIX}::*:*")),
            true,
        ),
        (
            "the singleton beside another resource",
            json!([other_resource.clone(), singleton.clone()]),
            true,
        ),
        ("another resource only", json!(other_resource.clone()), false),
        (
            "another service's resources",
            json!("arn:aws:iam::*:user/*"),
            false,
        ),
    ];

    for addressed in addressed_operations()
        .into_iter()
        .filter(|row| !row.clause.is_empty())
    {
        let action = format!("{ACTION_PREFIX}:{}", addressed.operation);
        for (row, resource, covered) in &rows {
            let case = format!("{} under an allow of {row}", addressed.operation);
            let (router, engine) = build_router_with_identity();
            given_user_key(&engine);
            given_user_policy(
                &engine,
                Some(&json!({
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": action.clone(),
                        "Resource": resource.clone(),
                    }],
                })),
            );

            let (status, error_type, answer) =
                call_addressed(&router, USER_KEY_ID, &addressed).await;
            if *covered {
                assert_eq!(
                    status, 200,
                    "case {case:?}: an allowed request must be served, got {status} {answer}"
                );
            } else {
                assert_refusal(
                    &case,
                    status,
                    &error_type,
                    &answer,
                    &format!(
                        "User: {} is not authorized to perform: {action} on resource: {singleton} \
                         because no identity-based policy allows the {action} action",
                        user_arn()
                    ),
                );
            }
        }
    }
}
