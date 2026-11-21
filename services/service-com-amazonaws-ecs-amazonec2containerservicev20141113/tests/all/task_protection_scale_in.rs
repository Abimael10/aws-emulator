// SPDX-License-Identifier: CC0-1.0

//! Task SCALE-IN PROTECTION and the identifiers a task-protection call carries, driven end
//! to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rules under test, each over the whole class rather than one request:
//!
//! > 1. A task whose caller has turned scale-in protection on is NOT selected when the
//! >    scheduler reduces a service's task set: `UpdateTaskProtection` "protects an Amazon
//! >    ECS task from being terminated by scale-in events from Service Auto Scaling or
//! >    deployments", so an unprotected task is stopped in its place — and when the service
//! >    holds no unprotected task to stop, the reduction waits until protection is taken off
//! >    (or lapses) rather than killing a protected one.
//! > 2. A request that carries a task ARN whose cluster differs from the request's own
//! >    `cluster` names two scopes at once, which is a bad IDENTIFIER and not a resource
//! >    that could not be found: both `GetTaskProtection` and `UpdateTaskProtection` refuse
//! >    it with `InvalidParameterException` and `Invalid identifier: cluster identifiers
//! >    mismatch`, and they do so before either identifier is RESOLVED — so the answer is
//! >    the same whether the cluster the request names exists or not. A request whose
//! >    identifiers agree is resolved, and a cluster this account does not hold is then the
//! >    `ClusterNotFoundException` that condition states.
//! > 3. Every `UpdateTaskProtection` that turns protection ON stores the instant THAT CALL
//! >    was made plus the period it named — `expiresInMinutes` is "the duration for task
//! >    protection in minutes", and the task is "automatically protected for 120 minutes"
//! >    when the member is absent — replacing whatever expiry the task already held, because
//! >    "the `expiresInMinutes` property is always reset when you invoke this operation for a
//! >    task that already has `protectionEnabled` set to true". That ONE stored instant is
//! >    what every read reports (`ProtectedTask$expirationDate`, "the epoch time when
//! >    protection for the task will expire"): the answering call's own and every later
//! >    `GetTaskProtection`, which never moves it on its own, so a second call made later
//! >    answers a LATER date and taking protection off leaves no date at all.
//!
//! The first table spans WHICH of a service's tasks is protected (the one the scheduler
//! would otherwise pick, one it would not, and all of them), the second spans the two
//! operations, both forms of the cluster reference, and whether the named cluster exists, and
//! the third spans the periods a call may name — both published bounds, a value inside them,
//! the default's own value and the absent member — each applied to a task that is already
//! protected as well as to one that is not.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through
//! `DescribeTasks`, `ListTasks` and `GetTaskProtection`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, refusal, register_definition, service, string, ACCOUNT,
    SIGNED_REGION,
};
use crate::list_service_deployments::epoch_seconds;

/// The cluster every service below runs in, and a second cluster that EXISTS so a
/// cross-cluster identifier can be weighed against a cluster that resolves.
const CLUSTER: &str = "protectscale";
const OTHER_CLUSTER: &str = "protectelsewhere";

/// A cluster name this account holds nothing under, for the rows whose cluster does not
/// resolve at all.
const MISSING_CLUSTER: &str = "protectnosuchcluster";

/// The task-definition family every service below runs.
const FAMILY: &str = "protectfamily";

/// How often a poll re-reads the surface it is waiting on, and how long it keeps trying:
/// the task transitions a scale-in takes, with generous margin for a loaded test machine.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_BUDGET: Duration = Duration::from_secs(30);

/// AWS's message for a request whose two identifiers name different clusters.
const IDENTIFIERS_MISMATCH: &str = "Invalid identifier: cluster identifiers mismatch";

/// The ARNs of the service's tasks that are still wanted running, oldest first — a task ARN
/// ends with the id it was minted under, so sorting them orders the task set by age.
async fn live_task_arns(router: &MultiServiceRouter, service_name: &str, case: &str) -> Vec<String> {
    let listed = ok(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": service_name,
            "desiredStatus": "RUNNING",
        }),
        case,
    )
    .await;
    let mut arns: Vec<String> = list(&listed, "taskArns", case)
        .into_iter()
        .map(|arn| arn.as_str().unwrap_or_default().to_string())
        .collect();
    arns.sort();
    arns
}

/// The `lastStatus` of every task in `arns`, in the order given.
async fn statuses(router: &MultiServiceRouter, arns: &[String], case: &str) -> Vec<String> {
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        case,
    )
    .await;
    let tasks = list(&described, "tasks", case);
    arns.iter()
        .map(|arn| {
            tasks
                .iter()
                .find(|task| task["taskArn"].as_str() == Some(arn.as_str()))
                .and_then(|task| task["lastStatus"].as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Create one service of `desired` tasks and wait until that many are running, answering
/// their ARNs oldest first.
async fn service_running(
    router: &MultiServiceRouter,
    service_name: &str,
    desired: i64,
    case: &str,
) -> Vec<String> {
    ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": service_name,
            "taskDefinition": FAMILY,
            "desiredCount": desired,
            "launchType": "EC2",
        }),
        case,
    )
    .await;
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let arns = live_task_arns(router, service_name, case).await;
        let held = statuses(router, &arns, case).await;
        let running = held.iter().filter(|status| *status == "RUNNING").count() as i64;
        if running == desired || Instant::now() >= deadline {
            assert_eq!(
                running, desired,
                "case {case:?}: expected {desired} running task(s), got {held:?}"
            );
            return arns;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Turn protection on or off for `tasks` of `service_name`'s cluster, naming
/// `expiresInMinutes` when `minutes` carries one, asserting the call reports what it stored.
///
/// Answers the epoch this machine read BEFORE the call, the `protectedTasks` the call
/// reported, and the epoch it read AFTER it — the window the call's own instant lies inside,
/// whatever else the machine was doing, which is what an expiry read off the answer is
/// weighed against ([`assert_expiry_of_call`]).
async fn protect_tasks(
    router: &MultiServiceRouter,
    tasks: &[String],
    enabled: bool,
    minutes: Option<i64>,
    case: &str,
) -> (f64, Vec<Value>, f64) {
    let mut request = json!({
        "cluster": CLUSTER,
        "tasks": tasks,
        "protectionEnabled": enabled,
    });
    if let Some(minutes) = minutes {
        request["expiresInMinutes"] = json!(minutes);
    }
    let before = epoch_seconds();
    let updated = ok(router, "UpdateTaskProtection", request, case).await;
    let after = epoch_seconds();
    let reported = list(&updated, "protectedTasks", case);
    assert_eq!(
        reported.len(),
        tasks.len(),
        "case {case:?}: every task named must be answered, got {updated}"
    );
    for task in &reported {
        assert_eq!(
            task["protectionEnabled"].as_bool(),
            Some(enabled),
            "case {case:?}: the protection the call stored is what it reports, got {updated}"
        );
    }
    (before, reported, after)
}

/// Turn protection on or off for `tasks` of `service_name`'s cluster for the period Amazon
/// ECS protects a task for when a call names none.
async fn protect(router: &MultiServiceRouter, tasks: &[String], enabled: bool, case: &str) {
    protect_tasks(router, tasks, enabled, None, case).await;
}

/// Reduce the service's desired count, asserting the call keeps the count it was given.
async fn scale_to(router: &MultiServiceRouter, service_name: &str, desired: i64, case: &str) {
    let updated = ok(
        router,
        "UpdateService",
        json!({
            "cluster": CLUSTER,
            "service": service_name,
            "desiredCount": desired,
        }),
        case,
    )
    .await;
    assert_eq!(
        updated["service"]["desiredCount"].as_i64(),
        Some(desired),
        "case {case:?}: the update keeps the count the caller asked for, got {updated}"
    );
}

/// Wait until the tasks the service still holds running are exactly `wanted`, answering what
/// it held at the end.
async fn settled_on(
    router: &MultiServiceRouter,
    arns: &[String],
    wanted: &[String],
    case: &str,
) -> Vec<String> {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = statuses(router, arns, case).await;
        let running: Vec<String> = arns
            .iter()
            .zip(&held)
            .filter(|(_, status)| *status == "RUNNING")
            .map(|(arn, _)| arn.clone())
            .collect();
        if running == wanted || Instant::now() >= deadline {
            assert_eq!(
                running, wanted,
                "case {case:?}: expected exactly the unprotected tasks to be stopped, got \
                 {held:?}"
            );
            return held;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the protection table: how many tasks the service runs, which of them (by age
/// index) are protected, how far it is scaled in, and which of them must survive.
struct Row {
    case: &'static str,
    service_name: &'static str,
    desired: i64,
    protected: &'static [usize],
    scale_to: i64,
    /// The indexes of the tasks that must still be running when the scale-in has settled.
    survivors: &'static [usize],
}

const ROWS: [Row; 3] = [
    Row {
        // Scaling in stops the NEWEST task of the deployment first, so protecting that one
        // is the row where the protection decides the selection.
        case: "the task the scheduler would otherwise stop is protected",
        service_name: "protectnewest",
        desired: 3,
        protected: &[2],
        scale_to: 2,
        survivors: &[0, 2],
    },
    Row {
        case: "a task the scheduler would not have stopped is protected",
        service_name: "protectoldest",
        desired: 3,
        protected: &[0],
        scale_to: 2,
        survivors: &[0, 1],
    },
    Row {
        // With every task protected there is no unprotected task to stop in their place, so
        // the reduction waits and the service keeps running more tasks than it wants.
        case: "every task is protected, so the reduction waits",
        service_name: "protectall",
        desired: 2,
        protected: &[0, 1],
        scale_to: 1,
        survivors: &[0, 1],
    },
];

#[tokio::test]
async fn a_protected_task_is_never_the_one_a_scale_in_stops() {
    let router = service();
    let case = "setup";
    cluster_with_capacity(&router, CLUSTER, case).await;
    register_definition(&router, FAMILY, case).await;

    for row in &ROWS {
        let case = row.case;
        let arns = service_running(&router, row.service_name, row.desired, case).await;
        let protected: Vec<String> = row
            .protected
            .iter()
            .map(|index| arns[*index].clone())
            .collect();
        protect(&router, &protected, true, case).await;
        scale_to(&router, row.service_name, row.scale_to, case).await;
        let survivors: Vec<String> = row
            .survivors
            .iter()
            .map(|index| arns[*index].clone())
            .collect();
        settled_on(&router, &arns, &survivors, case).await;

        // And the protection a caller takes OFF makes the task eligible again: the
        // reduction the scheduler could not make before is made without a further call.
        if row.survivors.len() as i64 > row.scale_to {
            let case = "protection taken off lets the waiting reduction through";
            let released: Vec<String> = row
                .survivors
                .iter()
                .rev()
                .take(row.survivors.len() - row.scale_to as usize)
                .map(|index| arns[*index].clone())
                .collect();
            protect(&router, &released, false, case).await;
            let kept: Vec<String> = row
                .survivors
                .iter()
                .filter(|index| !released.contains(&arns[**index]))
                .map(|index| arns[*index].clone())
                .collect();
            settled_on(&router, &arns, &kept, case).await;
        }
    }
}

/// One row of the identifier table: the operation, the `cluster` the request names, and
/// whether the task is named by an ARN of ANOTHER cluster.
struct IdentifierRow {
    case: &'static str,
    operation: &'static str,
    cluster: &'static str,
    /// Whether the task entry is an ARN scoped to [`CLUSTER`] (a cluster that is not the one
    /// the request names), rather than the bare task id.
    foreign_arn: bool,
    expected_code: &'static str,
    expected_message: &'static str,
}

const IDENTIFIER_ROWS: [IdentifierRow; 6] = [
    IdentifierRow {
        case: "GetTaskProtection against another EXISTING cluster",
        operation: "GetTaskProtection",
        cluster: OTHER_CLUSTER,
        foreign_arn: true,
        expected_code: "InvalidParameterException",
        expected_message: IDENTIFIERS_MISMATCH,
    },
    IdentifierRow {
        case: "UpdateTaskProtection against another EXISTING cluster",
        operation: "UpdateTaskProtection",
        cluster: OTHER_CLUSTER,
        foreign_arn: true,
        expected_code: "InvalidParameterException",
        expected_message: IDENTIFIERS_MISMATCH,
    },
    IdentifierRow {
        // The identifiers disagree whatever the cluster resolves to, so the answer is the
        // identifier's refusal and not the cluster's.
        case: "GetTaskProtection against a cluster that does not exist",
        operation: "GetTaskProtection",
        cluster: MISSING_CLUSTER,
        foreign_arn: true,
        expected_code: "InvalidParameterException",
        expected_message: IDENTIFIERS_MISMATCH,
    },
    IdentifierRow {
        case: "UpdateTaskProtection against a cluster that does not exist",
        operation: "UpdateTaskProtection",
        cluster: MISSING_CLUSTER,
        foreign_arn: true,
        expected_code: "InvalidParameterException",
        expected_message: IDENTIFIERS_MISMATCH,
    },
    IdentifierRow {
        // Nothing disagrees here — the entry carries no cluster of its own — so the cluster
        // is resolved, and this one does not exist.
        case: "GetTaskProtection by bare task id against a cluster that does not exist",
        operation: "GetTaskProtection",
        cluster: MISSING_CLUSTER,
        foreign_arn: false,
        expected_code: "ClusterNotFoundException",
        expected_message: "Cluster not found.",
    },
    IdentifierRow {
        case: "UpdateTaskProtection by bare task id against a cluster that does not exist",
        operation: "UpdateTaskProtection",
        cluster: MISSING_CLUSTER,
        foreign_arn: false,
        expected_code: "ClusterNotFoundException",
        expected_message: "Cluster not found.",
    },
];

#[tokio::test]
async fn a_task_protection_call_whose_identifiers_disagree_is_refused_for_the_identifier() {
    let router = service();
    let case = "setup";
    cluster_with_capacity(&router, CLUSTER, case).await;
    cluster_with_capacity(&router, OTHER_CLUSTER, case).await;
    register_definition(&router, FAMILY, case).await;
    let arns = service_running(&router, "protectidentity", 1, case).await;
    let task_arn = arns
        .first()
        .cloned()
        .expect("the service runs the one task it was asked for");
    let task_id = task_arn
        .rsplit('/')
        .next()
        .expect("a task ARN ends with the task id")
        .to_string();

    // The call that AGREES with itself is served, so the refusals below are about the
    // identifiers and not about the task.
    let served = ok(
        &router,
        "GetTaskProtection",
        json!({ "cluster": CLUSTER, "tasks": [task_arn.clone()] }),
        "the cluster that holds the task",
    )
    .await;
    assert_eq!(
        list(&served, "protectedTasks", case).len(),
        1,
        "case {case:?}: the task's own cluster answers for it, got {served}"
    );

    for row in &IDENTIFIER_ROWS {
        let case = row.case;
        let entry = if row.foreign_arn {
            task_arn.clone()
        } else {
            task_id.clone()
        };
        let mut request = json!({ "cluster": row.cluster, "tasks": [entry] });
        if row.operation == "UpdateTaskProtection" {
            request["protectionEnabled"] = json!(true);
        }
        let (code, message) = refusal(&router, row.operation, request, case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (row.expected_code, row.expected_message),
            "case {case:?}: expected the identifier refusal, got {code} with {message:?}"
        );
    }

    // A task ARN of a cluster that does not exist is read the same way: the identifier is
    // what disagrees with the request's `cluster`, so the refusal names neither cluster.
    let case = "an ARN scoped to a cluster nothing holds";
    let elsewhere =
        format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:task/{MISSING_CLUSTER}/{task_id}");
    let (code, message) = refusal(
        &router,
        "GetTaskProtection",
        json!({ "cluster": CLUSTER, "tasks": [elsewhere] }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("InvalidParameterException", IDENTIFIERS_MISMATCH),
        "case {case:?}: expected the identifier refusal, got {code} with {message:?}"
    );

    // And the protection the served call reported is still readable, so no refusal above
    // wrote anything.
    let read = ok(
        &router,
        "GetTaskProtection",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    let protected = list(&read, "protectedTasks", case);
    assert_eq!(
        protected
            .first()
            .and_then(|task| task["protectionEnabled"].as_bool()),
        Some(false),
        "case {case:?}: a refused update stored nothing, got {read}"
    );
    assert!(
        !string(&protected[0], "taskArn", case).is_empty(),
        "case {case:?}: the reported task names itself, got {read}"
    );
}

/// The periods one row of the expiry table names, as `(the `expiresInMinutes` the request
/// carries, the minutes the task is then protected for)`.
///
/// The member's own published range — "You can specify a value from 1 minute to up to 2,880
/// minutes (48 hours)" — at both of its bounds and at a value inside them, beside the
/// member's own default, which the same doc states: "If you don't specify the time, then the
/// task is automatically protected for 120 minutes (2 hours)"
/// (`smithy_query(query="doc",shape="UpdateTaskProtectionRequest",member="expiresInMinutes")`).
const PERIODS: [(Option<i64>, i64); 5] = [
    (None, 120),
    (Some(1), 1),
    (Some(10), 10),
    (Some(120), 120),
    (Some(2880), 2880),
];

/// Turn protection on for `task_arn` for `requested` minutes through the shared protection
/// call ([`protect_tasks`]), answering the epoch this machine read BEFORE the call, the
/// `expirationDate` the call reported, and the epoch it read AFTER it.
async fn protect_for(
    router: &MultiServiceRouter,
    task_arn: &str,
    requested: Option<i64>,
    case: &str,
) -> (f64, f64, f64) {
    let tasks = [task_arn.to_string()];
    let (before, reported, after) =
        protect_tasks(router, &tasks, true, requested, case).await;
    let expiry = reported[0]["expirationDate"].as_f64().unwrap_or_else(|| {
        panic!(
            "case {case:?}: a protected task reports when its protection expires, got {:?}",
            reported[0]
        )
    });
    (before, expiry, after)
}

/// What `GetTaskProtection` reports for `task_arn`: whether protection is on and the epoch
/// it expires at.
async fn read_protection(
    router: &MultiServiceRouter,
    task_arn: &str,
    case: &str,
) -> (bool, Option<f64>) {
    let read = ok(
        router,
        "GetTaskProtection",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    let reported = list(&read, "protectedTasks", case);
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: the one task named must be answered, got {read}"
    );
    (
        reported[0]["protectionEnabled"]
            .as_bool()
            .unwrap_or_else(|| panic!("case {case:?}: expected protectionEnabled, got {read}")),
        reported[0]["expirationDate"].as_f64(),
    )
}

/// How far a reported epoch may sit outside the window the call was bracketed by: the
/// instants a response reports are truncated to the millisecond
/// (`state::reported_instant`), and the floats cross the wire as decimal text, so one
/// millisecond and the rounding either side of it is allowed for — never a share of the
/// period itself.
const EPOCH_ROUNDING: f64 = 0.002;

/// Assert that `expiry` is the instant of a call bracketed by `before`..=`after` plus
/// `minutes`, which holds however long the machine took over the call.
fn assert_expiry_of_call(case: &str, minutes: i64, before: f64, expiry: f64, after: f64) {
    let period = (minutes * 60) as f64;
    assert!(
        expiry >= before + period - EPOCH_ROUNDING,
        "case {case:?}: expected an expiry at least {minutes} minute(s) past the instant \
         before the call ({}), got {expiry}",
        before + period
    );
    assert!(
        expiry <= after + period + EPOCH_ROUNDING,
        "case {case:?}: expected an expiry at most {minutes} minute(s) past the instant \
         after the call ({}), got {expiry}",
        after + period
    );
}

#[tokio::test]
async fn protecting_a_task_resets_its_expiry_from_the_call_that_protected_it() {
    let router = service();
    let case = "setup";
    cluster_with_capacity(&router, CLUSTER, case).await;
    register_definition(&router, FAMILY, case).await;
    let arns = service_running(&router, "protectexpiry", 1, case).await;
    let task_arn = arns
        .first()
        .cloned()
        .expect("the service runs the one task it was asked for");

    for (requested, minutes) in PERIODS {
        let case = format!("expiresInMinutes {requested:?}");
        let case = case.as_str();

        // The period the call named, measured off the call itself rather than off any
        // instant a previous row left on the task: each row runs against a task the row
        // before it protected for a different period, so an expiry that was KEPT rather
        // than reset passes no row but the one whose period it was set by.
        let (before, first, after) = protect_for(&router, &task_arn, requested, case).await;
        assert_expiry_of_call(case, minutes, before, first, after);

        // That one stored instant is what a READ reports, and reading does not move it:
        // `GetTaskProtection` is a report, so two reads of an untouched protection answer
        // the same date.
        for read in 1..=2 {
            let case = format!("{case}, read {read}");
            let case = case.as_str();
            let (enabled, expiry) = read_protection(&router, &task_arn, case).await;
            assert!(
                enabled,
                "case {case:?}: the protection the update stored is still on"
            );
            assert_eq!(
                expiry,
                Some(first),
                "case {case:?}: a read reports the instant the update stored, unmoved"
            );
        }

        // And the SAME period asked for again is reset from the LATER call's own instant, so
        // the date it answers is strictly later than the one before it. Looked for in a loop
        // rather than after a wait: what the rule turns on is that the date follows the call,
        // so the test re-asks until the clock it is read against has moved.
        let deadline = Instant::now() + POLL_BUDGET;
        let later = loop {
            let (before, again, after) = protect_for(&router, &task_arn, requested, case).await;
            assert_expiry_of_call(case, minutes, before, again, after);
            assert!(
                again >= first,
                "case {case:?}: a re-applied period never moves the expiry backwards, got \
                 {again} after {first}"
            );
            if again > first {
                break again;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected a later call with the same period to answer a later \
                 expiry than {first}, got {again}"
            );
            ::tokio::time::sleep(POLL_INTERVAL).await;
        };
        let (enabled, expiry) = read_protection(&router, &task_arn, case).await;
        assert!(
            enabled,
            "case {case:?}: the task is still protected after the second call"
        );
        assert_eq!(
            expiry,
            Some(later),
            "case {case:?}: a read reports the instant the LAST update stored"
        );
    }

    // "Specify true to mark a task for protection and false to unset protection": the task a
    // period was just stored for holds no expiry at all once protection is taken off, and a
    // later call protects it again from that call's own instant.
    let case = "protection taken off";
    protect(&router, std::slice::from_ref(&task_arn), false, case).await;
    let (enabled, expiry) = read_protection(&router, &task_arn, case).await;
    assert!(
        !enabled,
        "case {case:?}: a task whose protection was unset reports it off"
    );
    assert_eq!(
        expiry, None,
        "case {case:?}: a task that is not protected has no expiry to report"
    );
    let case = "protected again after being unset";
    let (before, again, after) = protect_for(&router, &task_arn, Some(1), case).await;
    assert_expiry_of_call(case, 1, before, again, after);
}
