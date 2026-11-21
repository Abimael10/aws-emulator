// SPDX-License-Identifier: CC0-1.0

//! The execution role a WRITE hands this service is resolved by that write: end to end
//! over the wire through the real `MultiServiceRouter`, with Amazon SQS and IAM mounted
//! beside EventBridge Scheduler on ONE engine, so the role is resolved against the very
//! records `CreateRole` wrote.
//!
//! RULE: `Target.RoleArn` is "the IAM role that EventBridge Scheduler will use for this
//! target when the schedule is invoked", and EventBridge Scheduler takes that reference by
//! ASSUMING the role as it writes the schedule — so every write that states one, the
//! update as much as the create, is refused with 400 `ValidationException` and the fixed
//! sentence [`EXECUTION_ROLE_REFUSAL`] unless this account holds the role and its trust
//! policy lets the scheduler service principal assume it. A refused write changes nothing:
//! the schedule keeps the role the last accepted write left on it, so it keeps delivering
//! as it did.
//!
//! The rows span the class rather than one recorded case: they differ along WHY the role
//! cannot be assumed — this account holds no such role, holds none under the path the ARN
//! names, the role is another account's (which this account's Scheduler cannot assume at
//! all), and the role exists here but trusts a different service — and the last row is the
//! control that an assumable role IS applied, so the refusal is the role's and not the
//! operation's.
//!
//! What the CALLER must be allowed in order to hand over a role at all (`iam:PassRole`),
//! and the same refusal from the create, are `tests/all/pass_role_authorization.rs`.

use serde_json::{json, Value};
use service_com_amazonaws_scheduler_awschronosservice::state::{
    EXECUTION_ROLE_REFUSAL, SCHEDULER_SERVICE_PRINCIPAL,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::schedule_invocation::{
    at_expression_body, emulator, given_execution_role_for, given_queue, iam, schedule_read,
    scheduler, seconds_from_now,
};
use crate::wire_harness::{account_id, error_code, error_message};

/// The group a schedule lands in when the write names none.
const DEFAULT_GROUP: &str = "default";

/// The error code every write of this service declares for an input it does not admit
/// (`smithy_query(query="operation", operation="UpdateSchedule")`: `ValidationException`,
/// `@httpError 400`).
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// Another account's id, used for the row whose role is not this account's: twelve digits,
/// as `Target$RoleArn`'s `@pattern` requires (`::\d{12}:role\/`), and not this emulator's.
const OTHER_ACCOUNT: &str = "210987654321";

/// A service principal that is NOT the scheduler's, for the row whose role exists here and
/// trusts something else.
const OTHER_PRINCIPAL: &str = "events.amazonaws.com";

/// How far ahead the schedules below state their one occurrence: far enough that no
/// occurrence of them falls due while the suite is writing and reading them, because what
/// is asserted here is the WRITE and not a delivery.
const OCCURRENCE_IN: u64 = 3_600;

/// WHERE the role an update states comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// An ARN of this account naming a role nothing created.
    Absent,
    /// An ARN of this account naming a role under a PATH, which this account holds no
    /// role under either.
    AbsentUnderAPath,
    /// A well-formed role ARN of another account.
    OtherAccount,
    /// A role this account holds whose trust policy names another service.
    TrustsAnotherService,
    /// A role this account holds that trusts the scheduler, which is the one an update may
    /// move a schedule onto.
    Assumable,
}

/// One row: the role the update states, and whether the update is served.
struct Row {
    case: &'static str,
    role: Role,
    /// Whether the write is accepted, which is also whether the stored role changes.
    applied: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a role this account does not hold",
        role: Role::Absent,
        applied: false,
    },
    Row {
        case: "a role under a path this account holds nothing under",
        role: Role::AbsentUnderAPath,
        applied: false,
    },
    Row {
        case: "another account's role",
        role: Role::OtherAccount,
        applied: false,
    },
    Row {
        case: "a role of this account that trusts another service",
        role: Role::TrustsAnotherService,
        applied: false,
    },
    Row {
        case: "a role of this account the scheduler may assume",
        role: Role::Assumable,
        applied: true,
    },
];

/// Every row: an update stating a role EventBridge Scheduler cannot assume is refused
/// with the fixed sentence and leaves the stored role as it was, while one stating a role
/// it can assume applies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_update_resolves_the_execution_role_it_states() {
    let emulator = emulator();
    let (_, target_arn) = given_queue(&emulator, "the suite", "update-role-target").await;
    // The role every row's schedule is CREATED with, and the one a refused update has to
    // leave in place.
    let original =
        given_execution_role_for(&emulator, "the suite", "original-role", &[&target_arn]).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("role-update-{index}");
        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/{name}"),
            Some(schedule_body(&target_arn, &original)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {created}"
        );

        let stated = role_of(&emulator, case, row.role, index, &target_arn).await;
        let (status, answered) = scheduler(
            &emulator,
            "PUT",
            &format!("/schedules/{name}"),
            Some(schedule_body(&target_arn, &stated)),
        )
        .await;
        if row.applied {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be served, got {status} {answered}"
            );
        } else {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, VALIDATION_EXCEPTION),
                "case {case:?}: expected the modelled refusal, got {status} {answered}"
            );
            assert_eq!(
                error_message(&answered),
                EXECUTION_ROLE_REFUSAL,
                "case {case:?}: expected the execution-role sentence, got {answered}"
            );
        }

        // What the schedule DELIVERS as is what a read reports: a refused update leaves
        // the role the create took, and an accepted one carries the role it stated.
        let expected = if row.applied { &stated } else { &original };
        let (status, read) = schedule_read(&emulator, DEFAULT_GROUP, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetSchedule answered {status} {read}"
        );
        assert_eq!(
            read["Target"]["RoleArn"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the stored role {expected:?}, got {read}"
        );
    }
}

/// The role ARN one row's update states, created in IAM where the row needs a role to
/// exist.
async fn role_of(
    router: &MultiServiceRouter,
    case: &str,
    role: Role,
    index: usize,
    target_arn: &str,
) -> String {
    match role {
        Role::Absent => format!("arn:aws:iam::{}:role/never-created-{index}", account_id()),
        Role::AbsentUnderAPath => format!(
            "arn:aws:iam::{}:role/service-role/never-created-{index}",
            account_id()
        ),
        Role::OtherAccount => format!("arn:aws:iam::{OTHER_ACCOUNT}:role/elsewhere-{index}"),
        Role::TrustsAnotherService => {
            let name = format!("other-principal-{index}");
            let trust = json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": OTHER_PRINCIPAL },
                    "Action": "sts:AssumeRole",
                }],
            })
            .to_string();
            let (status, created) = iam(
                router,
                "CreateRole",
                &[("RoleName", &name), ("AssumeRolePolicyDocument", &trust)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: CreateRole answered {status}: {created}"
            );
            // The role exists, so what refuses the update is the trust document alone —
            // which is why this row's role is not the scheduler's principal.
            assert_ne!(
                OTHER_PRINCIPAL, SCHEDULER_SERVICE_PRINCIPAL,
                "case {case:?}: the row's principal must not be the scheduler's"
            );
            format!("arn:aws:iam::{}:role/{name}", account_id())
        }
        Role::Assumable => {
            given_execution_role_for(router, case, &format!("assumable-{index}"), &[target_arn])
                .await
        }
    }
}

/// The schedule body a write states: one occurrence far enough ahead that nothing is
/// invoked while the suite runs, and the target the row's role is about.
fn schedule_body(target_arn: &str, role_arn: &str) -> Value {
    json!({
        "ScheduleExpression": format!("at({})", at_expression_body(seconds_from_now(OCCURRENCE_IN))),
        "ScheduleExpressionTimezone": "UTC",
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "State": "ENABLED",
        "Target": {
            "Arn": target_arn,
            "RoleArn": role_arn,
            "Input": "{}",
        },
    })
}
