// SPDX-License-Identifier: CC0-1.0

//! WHO may reach this data plane's operations, driven end to end over the real wire path
//! (a restJson1 request through the multi-service router into the mounted service), so what
//! is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: `AccessDeniedException` is a service-common error of this model
//! (`smithy_query(query="digest")`), so EVERY operation evaluates the identity-based IAM
//! policies in force on the principal its credentials name against its OWN action
//! `sagemaker:<OperationName>` — on the `JobArn` the request carries, under the request's
//! condition context — BEFORE it resolves the job or the trajectory. A request no statement
//! allows (IAM's default deny), one an explicit `Deny` covers, and one whose only grant is
//! conditioned on a key the request's values do not satisfy are each answered HTTP 403
//! `AccessDeniedException` and perform nothing, while a request a matching `Allow` covers
//! reaches the operation's own answer — the job-lookup refusal included, which is how an
//! enforced policy is told apart from a blanket denial.
//!
//! The role, its policies and the session live in the IAM and STS stores of the SAME shared
//! engine — the stores those services' own handlers write — so the documents under test are
//! the ones `iam:PutRolePolicy` stores and the principal a refusal blames is the one
//! `sts:GetCallerIdentity` answers for the key the request signed with.
//!
//! The tables span what the rule generalises over: the operation (both sampling calls and
//! both rollout-lifecycle calls), the document in force (none at all, another action of this
//! service, another service's actions, an explicit deny beside an allow, an allow of exactly
//! the action, of the action in another case, of the whole namespace and of everything), the
//! account the `JobArn` names against the condition the grant carries, and the principal (an
//! assumed-role session and the account's own published credential).
//!
//! THE ORDER is part of the rule and has its own table at the end of this suite: the decision
//! is taken BEFORE anything of the request's own values is checked, so a caller no statement
//! grants is answered 403 whether or not its request also breaks a modelled constraint,
//! while a caller IAM permits meets this service's own 400 for the same request.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    create_job, job_arn, minted_job_arn, mounted_with_identity, send_as, Reply, ACCOUNT,
    COMPLETE_ROLLOUT_PATH, DEFAULT_ACCESS_KEY_ID, JOB_ARN_HEADER, REGION, SAMPLE_PATH,
    SAMPLE_STREAM_PATH, TRAJECTORY_ID_HEADER, UPDATE_REWARD_PATH,
};

/// The access key id of the assumed-role session every policy row signs with.
const SESSION_KEY_ID: &str = "ASIAEXAMPLESESSION01";

/// The role that session is assumed from, and the session's own name.
const ROLE: &str = "rft-runner";
const SESSION: &str = "rft-session";

/// The name of the inline policy the role carries.
const POLICY_NAME: &str = "inline";

/// How long the seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// The job every row addresses, and the trajectory the rollout calls name.
const JOB_CATEGORY: &str = "AgentRFT";
const JOB_NAME: &str = "authz-run";
const TRAJECTORY: &str = "traj-1";

/// An account that is NOT the one this emulator serves, for the cross-account rows.
const FOREIGN_ACCOUNT: &str = "999999999999";

/// The four operations the model binds, each with the path it is bound to.
const OPERATIONS: [(&str, &str); 4] = [
    ("Sample", SAMPLE_PATH),
    ("SampleWithResponseStream", SAMPLE_STREAM_PATH),
    ("CompleteRollout", COMPLETE_ROLLOUT_PATH),
    ("UpdateReward", UPDATE_REWARD_PATH),
];

/// This service's IAM action prefix, from its `arnNamespace`
/// (`smithy_query(query="service_info")`).
const ACTION_PREFIX: &str = "sagemaker";

/// A router carrying this service beside the IAM and STS services over ONE shared engine,
/// so the policies and the session the rows seed are the ones those services hold — the
/// shared harness's own assembly (`tests/all/harness.rs`), which every suite about WHO may
/// call an operation drives.
fn router_with_identity() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    mounted_with_identity()
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
    let now = SystemTime::now();
    let role_arn = role_arn();
    let iam_store = iam::service_state(&**engine).expect("the IAM store");
    iam_store.lock().roles.insert(
        iam::name_key(ROLE),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: ROLE.to_string(),
            role_id: "AROAEXAMPLERFTRUNNER1".to_string(),
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
        .insert(vend.into_session(SESSION_KEY_ID, now))
        .expect("the session is recorded");
}

/// Attach `document` to the role as its inline policy, replacing whatever it carried —
/// what `iam:PutRolePolicy` does. `None` takes every policy off it, as
/// `iam:DeleteRolePolicy` does.
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

/// One call of the operation bound at `path`, signed with `access_key_id` and addressing
/// `job`: the sampling operations carry `TrajectoryId` in a header, the rollout operations
/// in the JSON body.
async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    path: &str,
    job: &str,
) -> Reply {
    let json_header = ("content-type", "application/json");
    match path {
        SAMPLE_PATH | SAMPLE_STREAM_PATH => {
            send_as(
                router,
                access_key_id,
                path,
                &[
                    (JOB_ARN_HEADER, job),
                    (TRAJECTORY_ID_HEADER, TRAJECTORY),
                    json_header,
                ],
                serde_json::to_vec(&json!({ "messages": [{ "role": "user", "content": "go" }] }))
                    .expect("a serializable payload"),
            )
            .await
        }
        COMPLETE_ROLLOUT_PATH => {
            let body = json!({ "TrajectoryId": TRAJECTORY, "Status": "ready" });
            send_as(
                router,
                access_key_id,
                path,
                &[(JOB_ARN_HEADER, job), json_header],
                serde_json::to_vec(&body).expect("a serializable body"),
            )
            .await
        }
        UPDATE_REWARD_PATH => {
            let body = json!({ "TrajectoryId": TRAJECTORY, "Rewards": [1.0] });
            send_as(
                router,
                access_key_id,
                path,
                &[(JOB_ARN_HEADER, job), json_header],
                serde_json::to_vec(&body).expect("a serializable body"),
            )
            .await
        }
        other => unreachable!("this model binds no operation at {other}"),
    }
}

/// Bring the trajectory the rollout-lifecycle calls need into the state each of them
/// requires, driven as the account's own credential so the seeding is never what the row is
/// measuring.
async fn given_trajectory_for(router: &MultiServiceRouter, path: &str, job: &str) {
    if path != COMPLETE_ROLLOUT_PATH && path != UPDATE_REWARD_PATH {
        return;
    }
    let seeded = call_as(router, DEFAULT_ACCESS_KEY_ID, SAMPLE_PATH, job).await;
    assert_eq!(
        seeded.status,
        200,
        "the seeding Sample must be served, got {} / {}",
        seeded.status,
        seeded.text()
    );
    if path == UPDATE_REWARD_PATH {
        let sealed = call_as(router, DEFAULT_ACCESS_KEY_ID, COMPLETE_ROLLOUT_PATH, job).await;
        assert_eq!(
            sealed.status,
            200,
            "the seeding CompleteRollout must be served, got {} / {}",
            sealed.status,
            sealed.text()
        );
    }
}

/// Which of IAM's refusals a row expects.
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

/// Assert that `reply` is the modelled refusal of `action` on `resource`, worded as IAM
/// words an identity-based denial.
fn assert_refused(case: &str, reply: &Reply, action: &str, resource: &str, refusal: Refusal) {
    assert_eq!(
        reply.status, 403,
        "case {case:?}: expected HTTP 403, got {} / {}",
        reply.status,
        reply.text()
    );
    assert_eq!(
        reply.error_code(),
        "AccessDeniedException",
        "case {case:?}: expected an AccessDeniedException, got {:?} / {}",
        reply.error_type,
        reply.text()
    );
    assert_eq!(
        reply.message(),
        format!(
            "User: {} is not authorized to perform: {action} on resource: {resource} {}",
            session_arn(),
            refusal.reason(action)
        ),
        "case {case:?}: expected IAM's wording, got {:?}",
        reply.message()
    );
}

/// The documents a row puts in force on the role, and which of IAM's refusals the request
/// then meets — `None` for a request a matching `Allow` permits.
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
            Some(allow(json!("s3:*"))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an explicit deny of this whole namespace beside an allow of exactly this action",
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
            Some(allow(json!(["s3:GetObject", action]))),
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

/// THE RULE: every operation is judged against its OWN `sagemaker:<OperationName>` before
/// it resolves anything; a refusal is HTTP 403 `AccessDeniedException` naming the principal,
/// the action, the `JobArn` and which of IAM's rules refused it, and an allowed call reaches
/// the operation's own answer.
#[tokio::test]
async fn every_operation_is_authorized_against_its_own_action() {
    let resource = minted_job_arn(JOB_CATEGORY, JOB_NAME);
    for (operation, path) in OPERATIONS {
        let action = format!("{ACTION_PREFIX}:{operation}");
        // Another action of this service, which a row grants instead of the one under test.
        let other = OPERATIONS
            .iter()
            .map(|(name, _)| format!("{ACTION_PREFIX}:{name}"))
            .find(|candidate| *candidate != action)
            .expect("this service binds more than one operation");

        for (row, document, refused) in documents_in_force(&action, &other) {
            let case = format!("{operation} with {row}");
            // A fresh store, role and session per row: no row can pass on another's write.
            let (engine, router) = router_with_identity();
            given_session(&engine);
            given_role_policy(&engine, document.as_ref());
            create_job(&engine, JOB_CATEGORY, JOB_NAME);
            given_trajectory_for(&router, path, &resource).await;

            let reply = call_as(&router, SESSION_KEY_ID, path, &resource).await;
            match refused {
                Some(refusal) => {
                    assert_refused(&case, &reply, &action, &resource, refusal);
                }
                None => {
                    assert_eq!(
                        reply.status,
                        200,
                        "case {case:?}: an allowed request must be served, got {} {:?} / {}",
                        reply.status,
                        reply.error_type,
                        reply.text()
                    );
                }
            }
        }
    }
}

/// THE RULE, in the form that tells an enforced policy apart from a blanket denial: the ONE
/// action a role is granted clears authorization and reaches the resource layer — which for
/// a job no create ever minted is each operation's own refusal — while the other three
/// actions of the same service are refused at the gate.
#[tokio::test]
async fn only_the_granted_action_reaches_the_resource_layer() {
    // No job is created, so an authorized call lands on the resource layer's own answer:
    // the sampling pair reports the unresolvable `JobArn`, the rollout pair the trajectory
    // that cannot exist inside a job that does not.
    let resource = minted_job_arn(JOB_CATEGORY, "never-created");
    for (granted, granted_path) in OPERATIONS {
        let (engine, router) = router_with_identity();
        given_session(&engine);
        given_role_policy(
            &engine,
            Some(&allow(json!(format!("{ACTION_PREFIX}:{granted}")))),
        );

        for (operation, path) in OPERATIONS {
            let case = format!("a grant of {granted} calling {operation}");
            let action = format!("{ACTION_PREFIX}:{operation}");
            let reply = call_as(&router, SESSION_KEY_ID, path, &resource).await;
            if path == granted_path {
                let (expected_status, expected_code) =
                    if path == SAMPLE_PATH || path == SAMPLE_STREAM_PATH {
                        (400, "ValidationException")
                    } else {
                        (404, "ResourceNotFoundException")
                    };
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (expected_status, expected_code),
                    "case {case:?}: the granted action must reach the resource layer, expected \
                     {expected_status} {expected_code}, got {} {:?} / {}",
                    reply.status,
                    reply.error_type,
                    reply.text()
                );
            } else {
                assert_refused(&case, &reply, &action, &resource, Refusal::DefaultDeny);
            }
        }
    }
}

/// THE RULE, applied to the condition keys a request publishes: a grant conditioned on the
/// account that OWNS the resource covers a `JobArn` the condition holds for and refuses one
/// it does not — so the account the ARN names, not the emulator's own, is what decides. A
/// refusal about a resource of another account names the principal alone.
#[tokio::test]
async fn a_resource_account_condition_decides_the_job_arn_the_request_names() {
    let action = format!("{ACTION_PREFIX}:Sample");
    // The grant the AWS-managed job-runtime access policy carries: every job of the
    // caller's own account, through the condition rather than through a literal account.
    let own_account_only = json!({
        "Statement": [{
            "Effect": "Allow",
            "Action": action,
            "Resource": "arn:aws:sagemaker:*:*:job/*",
            "Condition": { "StringEquals": { "aws:ResourceAccount": "${aws:PrincipalAccount}" } },
        }],
    });
    // The same grant conditioned on a LITERAL foreign account, which is what shows the
    // condition is evaluated rather than cross-account ARNs being refused outright.
    let foreign_account_only = json!({
        "Statement": [{
            "Effect": "Allow",
            "Action": action,
            "Resource": "arn:aws:sagemaker:*:*:job/*",
            "Condition": { "StringEquals": { "aws:ResourceAccount": FOREIGN_ACCOUNT } },
        }],
    });

    // (case, the document in force, the account the JobArn names, whether it is authorized)
    let rows: &[(&str, &Value, &str, bool)] = &[
        (
            "the caller's own account under the condition on it",
            &own_account_only,
            ACCOUNT,
            true,
        ),
        (
            "another account under the condition on the caller's own",
            &own_account_only,
            FOREIGN_ACCOUNT,
            false,
        ),
        (
            "an account differing from the caller's in one digit",
            &own_account_only,
            "000000000001",
            false,
        ),
        (
            "another account under the condition naming that account",
            &foreign_account_only,
            FOREIGN_ACCOUNT,
            true,
        ),
        (
            "the caller's own account under the condition naming another",
            &foreign_account_only,
            ACCOUNT,
            false,
        ),
    ];

    for (case, document, account, authorized) in rows {
        // No job is created, so an authorized call falls through to the job-lookup refusal
        // and a refused one never gets that far — which is also what keeps a refusal from
        // disclosing whether the job exists.
        let resource = job_arn("aws", REGION, account, JOB_CATEGORY, JOB_NAME);
        let (engine, router) = router_with_identity();
        given_session(&engine);
        given_role_policy(&engine, Some(document));

        let reply = call_as(&router, SESSION_KEY_ID, SAMPLE_PATH, &resource).await;
        if *authorized {
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, "ValidationException"),
                "case {case:?}: an authorized call must reach the job lookup, got {} {:?} / {}",
                reply.status,
                reply.error_type,
                reply.text()
            );
            assert_eq!(
                reply.message(),
                format!("Could not find job with jobArn {resource}"),
                "case {case:?}: expected the job-lookup refusal, got {:?}",
                reply.message()
            );
        } else if *account == ACCOUNT {
            assert_refused(case, &reply, &action, &resource, Refusal::DefaultDeny);
        } else {
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (403, "AccessDeniedException"),
                "case {case:?}: expected 403 AccessDeniedException, got {} {:?} / {}",
                reply.status,
                reply.error_type,
                reply.text()
            );
            assert_eq!(
                reply.message(),
                format!(
                    "User: {} is not authorized to access this resource",
                    session_arn()
                ),
                "case {case:?}: expected a cross-account refusal naming the principal alone, got \
                 {:?}",
                reply.message()
            );
        }
    }
}

/// RULE: the account's own published credential — a key neither STS nor IAM holds a record
/// for — is not a principal the IAM store holds policies for, so it is served whatever the
/// role's policies say.
#[tokio::test]
async fn the_accounts_own_credential_is_served() {
    let resource = minted_job_arn(JOB_CATEGORY, JOB_NAME);
    for (operation, path) in OPERATIONS {
        let (engine, router) = router_with_identity();
        given_session(&engine);
        // The role carries nothing at all, which is what refuses the SESSION above.
        given_role_policy(&engine, None);
        create_job(&engine, JOB_CATEGORY, JOB_NAME);
        given_trajectory_for(&router, path, &resource).await;

        let reply = call_as(&router, DEFAULT_ACCESS_KEY_ID, path, &resource).await;
        assert_eq!(
            reply.status,
            200,
            "case {operation:?}: the account's own credential must be served, got {} {:?} / {}",
            reply.status,
            reply.error_type,
            reply.text()
        );
    }
}

// ---------------------------------------------------------------------------
// The ORDER the decision is taken in: IAM first, the request's own values after
// ---------------------------------------------------------------------------

/// A value that is not an ARN at all, for the row whose `JobArn` is outside the modelled
/// `@pattern` (`smithy_query(query="shape",shape="JobArn")`).
const MALFORMED_JOB_ARN: &str = "not-a-job-arn";

/// `JobArn`'s and `TrajectoryId`'s modelled `@length` maximum — both are min 1 max 256
/// (`smithy_query(query="shape",shape="JobArn,TrajectoryId")`) — so a row one byte past it
/// is a value the walk refuses.
const LENGTH_MAX: usize = 256;

/// A value outside the `CompletionStatus` enum, whose only members are `ready` and `failed`
/// (`smithy_query(query="shape",shape="CompletionStatus")`).
const STATUS_OUTSIDE_THE_ENUM: &str = "finished";

/// Which modelled constraint a row's request breaks.
///
/// One row per class this model can break before a handler runs, read off the members the
/// four operations bind (`smithy_query(query="operation",operation="Sample,\
/// SampleWithResponseStream,CompleteRollout,UpdateReward")`): a `@length` maximum, a
/// `@length` minimum, an absent `@required` member, a `@pattern`, and an enum's value set.
#[derive(Debug, Clone, Copy)]
enum Broken {
    /// `TrajectoryId` one byte past its `@length` maximum.
    TrajectoryIdPastTheMaximum,
    /// `TrajectoryId` under its `@length` minimum of 1 — the empty value.
    TrajectoryIdBelowTheMinimum,
    /// No `TrajectoryId` at all, which is `@required` on every operation. Rowed only where
    /// the operation binds it in the BODY: a request missing a `@required` HEADER binding
    /// names no operation under rest routing, so it never reaches a service at all.
    TrajectoryIdAbsent,
    /// A `JobArn` that is not an ARN at all, so outside its `@pattern`.
    JobArnOutsideThePattern,
    /// A well-formed `JobArn` one byte past its `@length` maximum.
    JobArnPastTheMaximum,
    /// A `JobArn` carried as the empty value: under its `@length` minimum of 1, and the one
    /// member that names the resource the gate judges, so the request addresses none.
    JobArnCarriedEmpty,
    /// A `Status` outside the `CompletionStatus` value set (`CompleteRollout` alone binds
    /// it).
    StatusOutsideTheEnum,
    /// No `Rewards` at all, which is `@required` on `UpdateReward` alone.
    RewardsAbsent,
}

/// A well-formed `JobArn` carrying exactly `bytes` bytes, padded in the segment the modelled
/// `@pattern` leaves unbounded — the job category, `[a-zA-Z0-9_\-]+` — so the ARN satisfies
/// the pattern at any size and only its `@length` can refuse it.
fn job_arn_of(bytes: usize) -> String {
    let minted = minted_job_arn(JOB_CATEGORY, JOB_NAME);
    let padding = bytes.saturating_sub(minted.len());
    job_arn(
        "aws",
        REGION,
        ACCOUNT,
        &format!("{JOB_CATEGORY}{}", "c".repeat(padding)),
        JOB_NAME,
    )
}

/// The `JobArn` a row's request carries — which is the resource the gate judges it against —
/// and `None` for the row that carries none.
fn carried_job_arn(broken: Broken) -> Option<String> {
    match broken {
        Broken::JobArnOutsideThePattern => Some(MALFORMED_JOB_ARN.to_string()),
        Broken::JobArnPastTheMaximum => Some(job_arn_of(LENGTH_MAX + 1)),
        Broken::JobArnCarriedEmpty => Some(String::new()),
        _ => Some(minted_job_arn(JOB_CATEGORY, JOB_NAME)),
    }
}

/// The `TrajectoryId` a row's request carries, and `None` for the row that carries none.
fn carried_trajectory_id(broken: Broken) -> Option<String> {
    match broken {
        Broken::TrajectoryIdPastTheMaximum => Some("t".repeat(LENGTH_MAX + 1)),
        Broken::TrajectoryIdBelowTheMinimum => Some(String::new()),
        Broken::TrajectoryIdAbsent => None,
        _ => Some(TRAJECTORY.to_string()),
    }
}

/// One call of the operation bound at `path`, signed with `access_key_id`, breaking exactly
/// the modelled constraint `broken` names: the sampling operations carry `JobArn` and
/// `TrajectoryId` in headers, the rollout operations carry `JobArn` in a header and the rest
/// in the JSON body, so a row's broken member is left out or spoiled wherever that operation
/// binds it.
async fn call_breaking(
    router: &MultiServiceRouter,
    access_key_id: &str,
    path: &str,
    broken: Broken,
) -> Reply {
    let job = carried_job_arn(broken);
    let trajectory = carried_trajectory_id(broken);
    let mut headers: Vec<(&str, &str)> = vec![("content-type", "application/json")];
    if let Some(job) = job.as_deref() {
        headers.push((JOB_ARN_HEADER, job));
    }
    let body = match path {
        SAMPLE_PATH | SAMPLE_STREAM_PATH => {
            if let Some(trajectory) = trajectory.as_deref() {
                headers.push((TRAJECTORY_ID_HEADER, trajectory));
            }
            json!({ "messages": [{ "role": "user", "content": "go" }] })
        }
        COMPLETE_ROLLOUT_PATH => {
            let mut body = json!({
                "Status": match broken {
                    Broken::StatusOutsideTheEnum => STATUS_OUTSIDE_THE_ENUM,
                    _ => "ready",
                },
            });
            if let Some(trajectory) = trajectory {
                body["TrajectoryId"] = json!(trajectory);
            }
            body
        }
        UPDATE_REWARD_PATH => {
            let mut body = json!({});
            if let Some(trajectory) = trajectory {
                body["TrajectoryId"] = json!(trajectory);
            }
            if !matches!(broken, Broken::RewardsAbsent) {
                body["Rewards"] = json!([1.0]);
            }
            body
        }
        other => unreachable!("this model binds no operation at {other}"),
    };
    send_as(
        router,
        access_key_id,
        path,
        &headers,
        serde_json::to_vec(&body).expect("a serializable body"),
    )
    .await
}

/// Assert that `reply` is the modelled 403 of `action`, worded as IAM words an identity-based
/// default deny: naming the `JobArn` the request carried, and naming no resource at all for a
/// request that carried none.
fn assert_refused_by_iam(case: &str, reply: &Reply, action: &str, resource: Option<&str>) {
    assert_eq!(
        reply.status, 403,
        "case {case:?}: expected HTTP 403, got {} {:?} / {}",
        reply.status,
        reply.error_type,
        reply.text()
    );
    assert_eq!(
        reply.error_code(),
        "AccessDeniedException",
        "case {case:?}: expected an AccessDeniedException, got {:?} / {}",
        reply.error_type,
        reply.text()
    );
    let reason = Refusal::DefaultDeny.reason(action);
    let expected = match resource {
        Some(resource) => format!(
            "User: {} is not authorized to perform: {action} on resource: {resource} {reason}",
            session_arn()
        ),
        None => format!(
            "User: {} is not authorized to perform: {action} {reason}",
            session_arn()
        ),
    };
    assert_eq!(
        reply.message(),
        expected,
        "case {case:?}: expected IAM's wording, got {:?}",
        reply.message()
    );
}

/// Assert that `reply` is this service's own modelled 400 for what the request carried, in
/// the `Message` member the model declares — the answer a caller IAM permits gets for the
/// same request.
fn assert_refused_for_its_own_values(case: &str, reply: &Reply) {
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {} {:?} / {}",
        reply.status,
        reply.error_type,
        reply.text()
    );
    assert!(
        !reply.message().is_empty(),
        "case {case:?}: expected the refusal's prose in the modelled Message member, got {}",
        reply.text()
    );
}

/// THE RULE: the IAM decision is taken BEFORE anything of the request's own values is
/// checked, so a caller whose identity-based policies grant it nothing is answered the
/// modelled 403 `AccessDeniedException` — naming the principal, the action and the `JobArn`
/// the request carried — whether or not that request also breaks a modelled constraint; a
/// caller IAM permits is answered this service's own 400 `ValidationException` for the same
/// request.
///
/// The rows span the constraint classes this model can break before a handler runs (a
/// `@length` maximum, a `@length` minimum, an absent `@required` member, a `@pattern` and an
/// enum's value set), the member that breaks it (`JobArn`, `TrajectoryId`, `Status`,
/// `Rewards` — in a header and in the body), and every operation that binds it; the caller
/// varies over the three the gate tells apart: a session no statement grants, a session
/// granted the action, and the account's own published credential, which the gate does not
/// bound.
#[tokio::test]
async fn the_iam_decision_precedes_the_requests_own_values() {
    // (the class, the operations that bind the member it breaks)
    //
    // `TrajectoryIdAbsent` is rowed on the rollout pair alone: the sampling pair binds
    // `TrajectoryId` to a header, and rest routing admits an operation only when every
    // `@required` header binding it declares is on the request, so a request without it
    // names no operation and never reaches this service's gate at all.
    let rows: &[(Broken, &[(&str, &str)])] = &[
        (Broken::TrajectoryIdPastTheMaximum, &OPERATIONS),
        (Broken::TrajectoryIdBelowTheMinimum, &OPERATIONS),
        (
            Broken::TrajectoryIdAbsent,
            &[
                ("CompleteRollout", COMPLETE_ROLLOUT_PATH),
                ("UpdateReward", UPDATE_REWARD_PATH),
            ],
        ),
        (Broken::JobArnOutsideThePattern, &OPERATIONS),
        (Broken::JobArnPastTheMaximum, &OPERATIONS),
        (Broken::JobArnCarriedEmpty, &OPERATIONS),
        (
            Broken::StatusOutsideTheEnum,
            &[("CompleteRollout", COMPLETE_ROLLOUT_PATH)],
        ),
        (
            Broken::RewardsAbsent,
            &[("UpdateReward", UPDATE_REWARD_PATH)],
        ),
    ];

    for (broken, operations) in rows {
        for (operation, path) in *operations {
            let case = format!("{operation} with {broken:?}");
            let action = format!("{ACTION_PREFIX}:{operation}");
            // The resource the gate judges is the `JobArn` the request carried; a request
            // carrying it empty addresses no resource, which the refusal names none for.
            let carried = carried_job_arn(*broken);
            let named = carried.as_deref().filter(|arn| !arn.is_empty());

            // A caller the role's policies grant nothing: IAM's default deny, answered
            // before the request's own values are looked at.
            let (engine, router) = router_with_identity();
            given_session(&engine);
            given_role_policy(&engine, None);
            let reply = call_breaking(&router, SESSION_KEY_ID, path, *broken).await;
            assert_refused_by_iam(&case, &reply, &action, named);

            // The account's own published credential is a caller this gate does not bound,
            // so the same request meets this service's own refusal of what it carried.
            let served = call_breaking(&router, DEFAULT_ACCESS_KEY_ID, path, *broken).await;
            assert_refused_for_its_own_values(&format!("{case}, as the account"), &served);

            // And so does a session the role's policies DO grant the action to, which is
            // what tells an enforced decision apart from a blanket denial.
            let (granted_engine, granted_router) = router_with_identity();
            given_session(&granted_engine);
            given_role_policy(&granted_engine, Some(&allow(json!(action))));
            let granted = call_breaking(&granted_router, SESSION_KEY_ID, path, *broken).await;
            assert_refused_for_its_own_values(&format!("{case}, granted the action"), &granted);
        }
    }
}
