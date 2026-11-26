// SPDX-License-Identifier: CC0-1.0

//! WHO may make a call this service addresses to a REVISION of a task-definition family — a
//! launch and a deregistration — driven end to end over the real wire path (an `awsJson1_1`
//! request through the multi-service router into the mounted service), so what is asserted is
//! what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: a call addressed to a revision of a task-definition family has its
//! `taskDefinition` reference RESOLVED to one revision first — the revision it spells, and
//! "the latest ACTIVE revision" for a reference that spells none — and only then is it decided
//! whether the identity-based IAM policies in force on the principal the request's credential
//! speaks for allow `ecs:<OperationName>` on THAT revision's ARN. `RunTask`'s own doc states
//! the order: "a change was made to resolve the task definition revision before
//! authorization. When a task definition revision is not specified, authorization will occur
//! using the latest revision of a task definition." So a statement scoped to one revision
//! decides a call that names only its family, a request an explicit `Deny` covers or one no
//! statement allows is answered HTTP 403 `AccessDeniedException` naming that revision and
//! writes nothing, and a reference that resolves to no revision at all is answered by the
//! operation's own refusal rather than by this gate.
//!
//! The role, its inline policy and the session are made the way a client makes them —
//! `iam:CreateRole`, `iam:PutRolePolicy` and `sts:AssumeRole` over the wire against the same
//! shared engine — through the harness [`crate::mutating_request_authorization`] keeps for
//! this service's authorization decision, so the documents under test are the ones IAM stores
//! and the principal a refusal blames is the one `sts:GetCallerIdentity` reports.
//!
//! The tables span what the rule generalises over: the operation (`RunTask`, `StartTask`,
//! `DeregisterTaskDefinition`), the FORM the reference takes (a bare family, `family:revision`
//! and the full ARN of a revision), which revision of the family it resolves to (the latest
//! ACTIVE one and an earlier one), the document in force (a grant scoped to one revision, an
//! explicit deny beside an allow of everything) and a reference that resolves to nothing.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    create_cluster, list, ok, register_capacity, register_definition, string, ACCOUNT,
    SIGNED_REGION,
};
use crate::mutating_request_authorization::{
    allow_all_but, assumed_role, default_deny_message, ecs_as, emulator, error_code,
    explicit_deny_message, Session, ACCESS_DENIED, FORBIDDEN,
};

/// The cluster every launch below places into.
const CLUSTER: &str = "launchauthz";

/// The task-definition family every row addresses a revision of.
const FAMILY: &str = "launchauthzapp";

/// The family nothing registers a revision of, so a reference to it resolves to none.
const UNREGISTERED_FAMILY: &str = "launchauthznothing";

/// The revision of [`FAMILY`] a reference naming no revision resolves to: the fixture
/// registers two, so "the latest ACTIVE revision" is not the first one either.
const LATEST_REVISION: i32 = 2;

/// The earlier revision of [`FAMILY`], which only a reference that SPELLS it addresses.
const EARLIER_REVISION: i32 = 1;

/// The status a task-definition revision nothing retired reports
/// (`TaskDefinition$status`: `ACTIVE`).
const ACTIVE: &str = "ACTIVE";

/// The ARN of one revision of [`FAMILY`], as this service mints one.
fn revision_arn(revision: i32) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:task-definition/{FAMILY}:{revision}")
}

/// The fixture every test below starts from: the two authorization stores beside this
/// service, a cluster with one registered container instance for a placement to land on, and
/// two revisions of [`FAMILY`]. Answers the router and the container instance's ARN, which a
/// `StartTask` row names as the host it places on.
///
/// Every step is taken with the account's own published credential, which this gate does not
/// bound, so the fixture itself is never the thing under test.
async fn fixture(case: &str) -> (MultiServiceRouter, String) {
    let router = emulator();
    create_cluster(&router, CLUSTER, case).await;
    let host = register_capacity(&router, CLUSTER, case).await;
    for revision in [EARLIER_REVISION, LATEST_REVISION] {
        let registered = register_definition(&router, FAMILY, case).await;
        assert_eq!(
            registered,
            revision_arn(revision),
            "case {case:?}: expected the fixture's revision {revision} to be minted as \
             {}, got {registered}",
            revision_arn(revision)
        );
    }
    (router, host)
}

/// The `RunTask` body naming `reference`, placed into the fixture's cluster.
fn run(reference: &str) -> Value {
    json!({
        "cluster": CLUSTER,
        "taskDefinition": reference,
        "launchType": "EC2",
        "count": 1,
    })
}

/// The `StartTask` body naming `reference`, placed on `host`.
fn start(reference: &str, host: &str) -> Value {
    json!({
        "cluster": CLUSTER,
        "taskDefinition": reference,
        "containerInstances": [host],
    })
}

/// The task ARNs the account holds in the fixture's cluster, whatever their state.
async fn placed_tasks(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let listed = ok(router, "ListTasks", json!({ "cluster": CLUSTER }), case).await;
    list(&listed, "taskArns", case)
}

/// The `taskDefinition` each task of `arns` runs, as `DescribeTasks` reports it.
async fn task_definitions(
    router: &MultiServiceRouter,
    arns: &[Value],
    case: &str,
) -> Vec<String> {
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        case,
    )
    .await;
    list(&described, "tasks", case)
        .iter()
        .map(|task| string(task, "taskDefinitionArn", case))
        .collect()
}

/// One row of the scoped-grant table: the call a session whose grant names ONE revision
/// makes, and the revision the reference it carries resolves to.
struct Row {
    case: &'static str,
    operation: &'static str,
    /// The `taskDefinition` reference the request carries, in one of the three documented
    /// forms.
    reference: String,
    /// The revision the gate must judge the call on.
    revision: i32,
}

#[tokio::test]
async fn a_grant_scoped_to_one_revision_decides_the_revision_the_reference_resolves_to() {
    let case = "a grant scoped to the latest revision";
    let (router, host) = fixture(case).await;
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["ecs:RunTask", "ecs:StartTask"],
            "Resource": revision_arn(LATEST_REVISION),
        }],
    })
    .to_string();
    let session: Session = assumed_role(&router, "ecs-launcher", Some(&document), case).await;

    // Every form that resolves to the revision the grant names is served, whichever of the
    // three the request spells it in.
    let granted = vec![
        Row {
            case: "RunTask naming the family alone, which resolves to the latest revision",
            operation: "RunTask",
            reference: FAMILY.to_string(),
            revision: LATEST_REVISION,
        },
        Row {
            case: "RunTask naming family:revision",
            operation: "RunTask",
            reference: format!("{FAMILY}:{LATEST_REVISION}"),
            revision: LATEST_REVISION,
        },
        Row {
            case: "RunTask naming the revision's full ARN",
            operation: "RunTask",
            reference: revision_arn(LATEST_REVISION),
            revision: LATEST_REVISION,
        },
        Row {
            case: "StartTask naming the family alone",
            operation: "StartTask",
            reference: FAMILY.to_string(),
            revision: LATEST_REVISION,
        },
    ];
    for row in &granted {
        let row_case = row.case;
        let body = match row.operation {
            "StartTask" => start(&row.reference, &host),
            _ => run(&row.reference),
        };
        let (status, answered) =
            ecs_as(&router, &session.access_key_id, row.operation, body).await;
        assert_eq!(
            status, 200,
            "case {row_case:?}: expected {} to be served, got {status} with {answered}",
            row.operation
        );
        let launched = list(&answered, "tasks", row_case);
        assert_eq!(
            launched.len(),
            1,
            "case {row_case:?}: expected one placed task, got {answered}"
        );
        assert_eq!(
            string(&launched[0], "taskDefinitionArn", row_case),
            revision_arn(row.revision),
            "case {row_case:?}: expected the task to run the revision the decision was \
             taken on, got {answered}"
        );
    }

    // Every form that resolves to a revision the grant does NOT name falls to IAM's default
    // deny, named by that revision's own ARN.
    let refused = vec![
        Row {
            case: "RunTask naming the earlier revision by family:revision",
            operation: "RunTask",
            reference: format!("{FAMILY}:{EARLIER_REVISION}"),
            revision: EARLIER_REVISION,
        },
        Row {
            case: "RunTask naming the earlier revision by its full ARN",
            operation: "RunTask",
            reference: revision_arn(EARLIER_REVISION),
            revision: EARLIER_REVISION,
        },
        Row {
            case: "StartTask naming the earlier revision",
            operation: "StartTask",
            reference: format!("{FAMILY}:{EARLIER_REVISION}"),
            revision: EARLIER_REVISION,
        },
        Row {
            case: "DeregisterTaskDefinition, an action the grant names no statement about",
            operation: "DeregisterTaskDefinition",
            reference: format!("{FAMILY}:{EARLIER_REVISION}"),
            revision: EARLIER_REVISION,
        },
    ];
    for row in &refused {
        let row_case = row.case;
        let body = match row.operation {
            "StartTask" => start(&row.reference, &host),
            "DeregisterTaskDefinition" => json!({ "taskDefinition": row.reference }),
            _ => run(&row.reference),
        };
        let (status, answered) =
            ecs_as(&router, &session.access_key_id, row.operation, body).await;
        assert_eq!(
            status, FORBIDDEN,
            "case {row_case:?}: expected {} to answer {FORBIDDEN}, got {status} with \
             {answered}",
            row.operation
        );
        assert_eq!(
            error_code(&answered),
            ACCESS_DENIED,
            "case {row_case:?}: expected an {ACCESS_DENIED}, got {answered}"
        );
        assert_eq!(
            answered["message"].as_str().unwrap_or_default(),
            default_deny_message(
                &session.session_arn,
                row.operation,
                &revision_arn(row.revision)
            ),
            "case {row_case:?}: expected IAM's default-deny wording naming revision {}, got \
             {answered}",
            row.revision
        );
    }

    // What the account holds matches the decisions: one task per served launch, each of the
    // granted revision, and nothing from any refused one.
    let placed = placed_tasks(&router, case).await;
    assert_eq!(
        placed.len(),
        granted.len(),
        "case {case:?}: expected one task per served launch, got {placed:?}"
    );
    let running = task_definitions(&router, &placed, case).await;
    assert!(
        running
            .iter()
            .all(|definition| *definition == revision_arn(LATEST_REVISION)),
        "case {case:?}: expected every placed task to run {}, got {running:?}",
        revision_arn(LATEST_REVISION)
    );
}

#[tokio::test]
async fn an_explicit_deny_refuses_the_call_addressed_to_the_revision_and_writes_nothing() {
    let case = "an explicit deny beside an allow of everything";
    let (router, host) = fixture(case).await;
    let session: Session = assumed_role(
        &router,
        "ecs-launch-denied",
        Some(&allow_all_but(&[
            "ecs:RunTask",
            "ecs:StartTask",
            "ecs:DeregisterTaskDefinition",
        ])),
        case,
    )
    .await;

    let rows = vec![
        Row {
            case: "RunTask, judged on the revision the family resolves to",
            operation: "RunTask",
            reference: FAMILY.to_string(),
            revision: LATEST_REVISION,
        },
        Row {
            case: "StartTask, judged on the revision it spells",
            operation: "StartTask",
            reference: format!("{FAMILY}:{EARLIER_REVISION}"),
            revision: EARLIER_REVISION,
        },
        Row {
            case: "DeregisterTaskDefinition, judged on the revision it would retire",
            operation: "DeregisterTaskDefinition",
            reference: revision_arn(EARLIER_REVISION),
            revision: EARLIER_REVISION,
        },
    ];
    for row in &rows {
        let row_case = row.case;
        let body = match row.operation {
            "StartTask" => start(&row.reference, &host),
            "DeregisterTaskDefinition" => json!({ "taskDefinition": row.reference }),
            _ => run(&row.reference),
        };
        let (status, answered) =
            ecs_as(&router, &session.access_key_id, row.operation, body).await;
        assert_eq!(
            status, FORBIDDEN,
            "case {row_case:?}: expected {} to answer {FORBIDDEN}, got {status} with \
             {answered}",
            row.operation
        );
        assert_eq!(
            error_code(&answered),
            ACCESS_DENIED,
            "case {row_case:?}: expected an {ACCESS_DENIED}, got {answered}"
        );
        assert_eq!(
            answered["message"].as_str().unwrap_or_default(),
            explicit_deny_message(
                &session.session_arn,
                row.operation,
                Some(&revision_arn(row.revision))
            ),
            "case {row_case:?}: expected IAM's wording for an explicit deny naming revision \
             {}, got {answered}",
            row.revision
        );
    }

    // Nothing the refused calls asked for was written: no task was placed, and the revision
    // the deregistration named is still ACTIVE.
    let placed = placed_tasks(&router, case).await;
    assert!(
        placed.is_empty(),
        "case {case:?}: a refused launch must place no task, got {placed:?}"
    );
    let described = ok(
        &router,
        "DescribeTaskDefinition",
        json!({ "taskDefinition": revision_arn(EARLIER_REVISION) }),
        case,
    )
    .await;
    assert_eq!(
        string(&described["taskDefinition"], "status", case),
        ACTIVE,
        "case {case:?}: a refused deregistration must retire nothing, got {described}"
    );
}

#[tokio::test]
async fn a_reference_resolving_to_no_revision_is_answered_by_the_operation_not_by_the_gate() {
    let case = "a family the account holds no revision of";
    let (router, _) = fixture(case).await;
    let session: Session = assumed_role(
        &router,
        "ecs-launch-unresolved",
        Some(&allow_all_but(&["ecs:RunTask"])),
        case,
    )
    .await;

    // The revision is resolved BEFORE the decision, so a reference that resolves to none
    // never reaches the decision: the operation's own refusal is what the caller gets, even
    // for a principal an explicit deny covers.
    let (status, answered) = ecs_as(
        &router,
        &session.access_key_id,
        "RunTask",
        run(UNREGISTERED_FAMILY),
    )
    .await;
    assert_ne!(
        status, FORBIDDEN,
        "case {case:?}: expected the operation's own refusal rather than an authorization \
         one, got {status} with {answered}"
    );
    assert_ne!(
        error_code(&answered),
        ACCESS_DENIED,
        "case {case:?}: expected the operation's own refusal class, got {answered}"
    );

    // And nothing was placed either, so the unresolved reference is refused rather than run.
    let placed = placed_tasks(&router, case).await;
    assert!(
        placed.is_empty(),
        "case {case:?}: expected no task from a reference naming no revision, got {placed:?}"
    );
}
