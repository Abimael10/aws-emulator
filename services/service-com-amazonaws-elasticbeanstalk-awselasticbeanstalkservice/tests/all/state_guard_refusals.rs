// SPDX-License-Identifier: CC0-1.0

//! How Elastic Beanstalk words the refusals its STATE GUARDS answer with, driven
//! end to end over this service's awsQuery surface with the shared harness.
//!
//! RULE A (nothing to cancel): an operation that acts on an update or deployment
//! already in flight refuses an environment that reports none with the service's
//! shared invalid-state sentence — `Environment named <EnvironmentName> is in an
//! invalid state for this operation. Must be pending deployment.` The ONLY
//! variable part is the environment's name; the state the environment is in is
//! not disclosed, and the sentence is the same whichever member of the
//! `EnvironmentName` / `EnvironmentId` pair the request named it by.
//!
//! RULE B (a delete a live resource blocks): a delete refused because the
//! application still holds a running environment is answered with the
//! service-common `InvalidParameterValue` (400) — the class a caller branches on
//! to retry with `TerminateEnvByForce`, not the operation's modelled
//! operation-in-progress error, which is reserved for another operation already
//! running on the same element — and with `Unable to delete application
//! <ApplicationName> because it has a version that is deployed to a running
//! environment.` The only variable part is the application the request named: no
//! environment of it is named, and the refused delete leaves both the
//! application and its environments exactly as they were — while the IDENTICAL
//! call with `TerminateEnvByForce` set is served.
//!
//! Both templates are written out HERE rather than imported from the service, so
//! a change to the handlers' wording fails this suite instead of moving with it.
//! Tables span the inputs each rule generalises over — which member names the
//! environment, which state the environment is in, how many environments the
//! application holds, how the force flag is spelled, and how long the names are
//! — and every wait is a poll for the condition, never a fixed sleep.

use crate::harness::{wait_for_status, Emulator, SOLUTION_STACK};
use crate::lifecycle::{POLL_INTERVAL, STATUS_DEADLINE};

/// RULE A's template: the environment the request named, and the state the
/// operation requires.
fn invalid_state_pending_deployment(environment_name: &str) -> String {
    format!(
        "Environment named {environment_name} is in an invalid state for this operation. \
         Must be pending deployment."
    )
}

/// RULE B's template: the application the request named, and nothing else.
fn unable_to_delete_application(application_name: &str) -> String {
    format!(
        "Unable to delete application {application_name} because it has a version that is \
         deployed to a running environment."
    )
}

/// Look again until no application is described any more — the CONDITION a
/// started delete settles into, never a fixed wait for the clock. The deadline
/// bounds nothing this suite asserts; it only stops a delete that never settles
/// from hanging the target.
async fn wait_until_no_application_is_described(emulator: &Emulator, case: &str) {
    let started = std::time::Instant::now();
    loop {
        let applications = emulator.call("DescribeApplications", &[]).await;
        let names = applications.values_at("Applications/member/ApplicationName");
        if names.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < STATUS_DEADLINE,
            "case {case:?}: the deleted applications are still described as {names:?} in body {}",
            applications.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create `application`.
async fn application(emulator: &Emulator, case: &str, name: &str) {
    let created = emulator
        .call("CreateApplication", &[("ApplicationName", name)])
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateApplication, got {} and body {}",
        created.status, created.body
    );
}

/// Launch `environment` under `application_name` and answer its
/// `EnvironmentId`. The launch is NOT waited for here: a row that needs it
/// settled polls for the status it needs.
async fn launch(
    emulator: &Emulator,
    case: &str,
    application_name: &str,
    environment: &str,
) -> String {
    let launched = emulator
        .call(
            "CreateEnvironment",
            &[
                ("ApplicationName", application_name),
                ("EnvironmentName", environment),
                ("SolutionStackName", SOLUTION_STACK.as_str()),
            ],
        )
        .await;
    assert_eq!(
        launched.status, 200,
        "case {case:?}: expected 200 from CreateEnvironment, got {} and body {}",
        launched.status, launched.body
    );
    launched
        .value("EnvironmentId")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: CreateEnvironment must answer with an EnvironmentId, got {}",
                launched.body
            )
        })
        .to_string()
}

/// One row of RULE A: which environment the abort names, and by which member of
/// the either-or identifier pair.
struct AbortRow {
    case: &'static str,
    /// The environment whose name the refusal must carry.
    environment: &'static str,
    send_name: bool,
    send_id: bool,
}

#[tokio::test]
async fn an_abort_with_no_deployment_pending_is_refused_in_the_invalid_state_sentence() {
    const APPLICATION: &str = "guard-abort-app";
    let emulator = Emulator::new();
    application(&emulator, "RULE A", APPLICATION).await;

    // `idle-env` has settled with nothing in flight; `envz` is the same with a
    // name at the modelled minimum length of 4; `coming-up-env` is still
    // launching, and `going-away-env` is on its way out — neither reports an
    // abortable operation in any of the states it passes through, so no row
    // depends on catching a transient state.
    let idle_id = launch(&emulator, "RULE A", APPLICATION, "idle-env").await;
    let short_id = launch(&emulator, "RULE A", APPLICATION, "envz").await;
    wait_for_status(&emulator, "RULE A", "idle-env", "Ready").await;
    wait_for_status(&emulator, "RULE A", "envz", "Ready").await;
    let launching_id = launch(&emulator, "RULE A", APPLICATION, "coming-up-env").await;
    let leaving_id = launch(&emulator, "RULE A", APPLICATION, "going-away-env").await;
    wait_for_status(&emulator, "RULE A", "going-away-env", "Ready").await;
    let terminated = emulator
        .call(
            "TerminateEnvironment",
            &[("EnvironmentName", "going-away-env")],
        )
        .await;
    assert_eq!(
        terminated.status, 200,
        "case \"RULE A\": expected 200 from TerminateEnvironment, got body {}",
        terminated.body
    );

    let identifiers: std::collections::HashMap<&str, &str> = [
        ("idle-env", idle_id.as_str()),
        ("envz", short_id.as_str()),
        ("coming-up-env", launching_id.as_str()),
        ("going-away-env", leaving_id.as_str()),
    ]
    .into_iter()
    .collect();

    let rows = [
        AbortRow {
            case: "a settled environment named by EnvironmentName",
            environment: "idle-env",
            send_name: true,
            send_id: false,
        },
        AbortRow {
            case: "the same environment named by EnvironmentId",
            environment: "idle-env",
            send_name: false,
            send_id: true,
        },
        AbortRow {
            case: "the same environment named by both members",
            environment: "idle-env",
            send_name: true,
            send_id: true,
        },
        AbortRow {
            // The modelled minimum length for `EnvironmentName` is 4.
            case: "a name at the modelled minimum length of 4",
            environment: "envz",
            send_name: true,
            send_id: false,
        },
        AbortRow {
            case: "an environment that is still coming up",
            environment: "coming-up-env",
            send_name: true,
            send_id: false,
        },
        AbortRow {
            // Named by its id: a shut-down environment is still resolved by id,
            // so this row reads the same whether the shutdown has completed yet.
            case: "an environment on its way out, named by EnvironmentId",
            environment: "going-away-env",
            send_name: false,
            send_id: true,
        },
    ];

    for AbortRow {
        case,
        environment,
        send_name,
        send_id,
    } in &rows
    {
        let mut params: Vec<(&str, &str)> = Vec::new();
        if *send_name {
            params.push(("EnvironmentName", environment));
        }
        if *send_id {
            params.push(("EnvironmentId", identifiers[environment]));
        }
        let refused = emulator.call("AbortEnvironmentUpdate", &params).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400, got {} and body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "InvalidParameterValue",
            "case {case:?}: expected InvalidParameterValue, got body {}",
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            invalid_state_pending_deployment(environment),
            "case {case:?}: expected the invalid-state sentence, got {:?}",
            refused.error_message()
        );
    }

    // And the refused aborts changed nothing a later caller sees: the settled
    // environment is still ready and still reports no abortable operation.
    let described = emulator
        .call(
            "DescribeEnvironments",
            &[("EnvironmentNames.member.1", "idle-env")],
        )
        .await;
    assert_eq!(
        described.values_at("Environments/member/Status"),
        vec!["Ready"],
        "case \"RULE A\": a refusal must not move the environment, got {}",
        described.body
    );
    assert_eq!(
        described.values_at("Environments/member/AbortableOperationInProgress"),
        vec!["false"],
        "case \"RULE A\": a refusal must not make an operation abortable, got {}",
        described.body
    );
}

/// One row of RULE B: the application, how many environments it holds, and how
/// the request spells the force flag it does NOT set.
struct DeleteRow {
    case: &'static str,
    application: &'static str,
    environments: &'static [&'static str],
    /// `None` leaves `TerminateEnvByForce` out of the request entirely.
    force: Option<&'static str>,
    /// Poll until the environments are `Ready` before the delete, rather than
    /// refusing them while they are still launching.
    settled: bool,
}

/// An application name at the modelled maximum length of 100, so the template is
/// exercised on a name far from the short ones the other rows use.
fn long_application_name() -> String {
    format!("guard-{}", "a".repeat(94))
}

#[tokio::test]
async fn deleting_an_application_that_still_holds_an_environment_is_refused_as_invalid_parameter() {
    let long_name = long_application_name();
    assert_eq!(
        long_name.len(),
        100,
        "fixture: the long application name must sit on the modelled maximum of 100"
    );

    let rows = [
        DeleteRow {
            case: "one settled environment, TerminateEnvByForce left out",
            application: "guard-single",
            // No environment name here is a substring of its application's
            // name, so the row's "names no environment" check means what it says.
            environments: &["solo"],
            force: None,
            settled: true,
        },
        DeleteRow {
            case: "one settled environment, TerminateEnvByForce=false",
            application: "guard-flag-false",
            environments: &["flagged"],
            force: Some("false"),
            settled: true,
        },
        DeleteRow {
            case: "two settled environments, TerminateEnvByForce left out",
            application: "guard-pair",
            environments: &["twin-a", "twin-b"],
            force: None,
            settled: true,
        },
        DeleteRow {
            case: "an environment that is still coming up",
            application: "guard-fresh",
            environments: &["coming-up"],
            force: None,
            settled: false,
        },
    ];

    for row in &rows {
        refusal_row(row.case, row.application, row.environments, row.force, row.settled).await;
    }
    // The same rule on a name at the modelled maximum length: the template
    // carries whatever name the request named, however long.
    refusal_row(
        "an application name at the modelled maximum of 100",
        &long_name,
        &["lengthy"],
        None,
        true,
    )
    .await;
}

/// Drive one RULE B row: the refusal, what it says, and what survives it. The
/// other half of the rule — the identical call succeeding once the force flag is
/// set — is its own test, so a row costs one settled environment and no
/// teardown.
async fn refusal_row(
    case: &str,
    application_name: &str,
    environments: &[&str],
    force: Option<&str>,
    settled: bool,
) {
    let emulator = Emulator::new();
    application(&emulator, case, application_name).await;
    for environment in environments {
        launch(&emulator, case, application_name, environment).await;
    }
    if settled {
        for environment in environments {
            wait_for_status(&emulator, case, environment, "Ready").await;
        }
    }

    let mut params: Vec<(&str, &str)> = vec![("ApplicationName", application_name)];
    if let Some(force) = force {
        params.push(("TerminateEnvByForce", force));
    }
    let refused = emulator.call("DeleteApplication", &params).await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected HTTP 400, got {} and body {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        "InvalidParameterValue",
        "case {case:?}: expected InvalidParameterValue, got body {}",
        refused.body
    );
    assert_eq!(
        refused.error_message(),
        unable_to_delete_application(application_name),
        "case {case:?}: expected the delete-blocked sentence, got {:?}",
        refused.error_message()
    );
    for environment in environments {
        assert!(
            !refused.error_message().contains(environment),
            "case {case:?}: the refusal must not name the environment {environment:?}, got {:?}",
            refused.error_message()
        );
    }

    // Nothing the refused delete touched is gone: the application is still
    // described, and so is every environment of it.
    let applications = emulator.call("DescribeApplications", &[]).await;
    assert_eq!(
        applications.values_at("Applications/member/ApplicationName"),
        vec![application_name],
        "case {case:?}: the refused delete must leave the application, got {}",
        applications.body
    );
    let described = emulator.call("DescribeEnvironments", &[]).await;
    let mut names = described.values_at("Environments/member/EnvironmentName");
    names.sort_unstable();
    let mut expected: Vec<&str> = environments.to_vec();
    expected.sort_unstable();
    assert_eq!(
        names, expected,
        "case {case:?}: the refused delete must leave the environments, got {}",
        described.body
    );

}

#[tokio::test]
async fn the_refused_delete_succeeds_once_terminate_env_by_force_is_set() {
    // The other half of RULE B: the guard is about the environments the
    // application holds and nothing else, so the IDENTICAL call is served once
    // the caller asks for those environments to go with it —
    // "TerminateEnvByForce: … running environments will be terminated before
    // deleting the application".
    const CASE: &str = "forced after a refusal";
    const APPLICATION: &str = "guard-forced";
    let emulator = Emulator::new();
    application(&emulator, CASE, APPLICATION).await;
    launch(&emulator, CASE, APPLICATION, "held").await;
    wait_for_status(&emulator, CASE, "held", "Ready").await;

    let refused = emulator
        .call("DeleteApplication", &[("ApplicationName", APPLICATION)])
        .await;
    assert_eq!(
        refused.error_code(),
        "InvalidParameterValue",
        "case {CASE:?}: expected the guard to refuse first, got body {}",
        refused.body
    );

    let deleted = emulator
        .call(
            "DeleteApplication",
            &[
                ("ApplicationName", APPLICATION),
                ("TerminateEnvByForce", "true"),
            ],
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {CASE:?}: expected 200 from the forced delete, got {} and body {}",
        deleted.status, deleted.body
    );
    // The environment it took with it is read back through an implemented
    // reader, and the application is gone once the delete it started settles.
    wait_for_status(&emulator, CASE, "held", "Terminated").await;
    wait_until_no_application_is_described(&emulator, CASE).await;
}

#[tokio::test]
async fn deleting_an_application_with_no_environment_needs_no_force_flag() {
    // The positive control of RULE B: the guard is about the environments the
    // application holds, so an application that holds none is deleted without
    // the flag, and one whose environments have all been terminated is too.
    let emulator = Emulator::new();
    application(&emulator, "no environment", "guard-empty").await;
    application(&emulator, "a terminated environment", "guard-emptied").await;
    launch(&emulator, "a terminated environment", "guard-emptied", "ex-env").await;
    wait_for_status(&emulator, "a terminated environment", "ex-env", "Ready").await;
    let terminated = emulator
        .call("TerminateEnvironment", &[("EnvironmentName", "ex-env")])
        .await;
    assert_eq!(
        terminated.status, 200,
        "case \"a terminated environment\": expected 200 from TerminateEnvironment, got {}",
        terminated.body
    );
    wait_for_status(&emulator, "a terminated environment", "ex-env", "Terminated").await;

    for (case, application_name) in [
        ("no environment", "guard-empty"),
        ("a terminated environment", "guard-emptied"),
    ] {
        let deleted = emulator
            .call("DeleteApplication", &[("ApplicationName", application_name)])
            .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected 200, got {} and body {}",
            deleted.status, deleted.body
        );
        assert_eq!(
            deleted.error_code(),
            "",
            "case {case:?}: expected a success envelope, got {}",
            deleted.body
        );
    }
    wait_until_no_application_is_described(&emulator, "positive control").await;
}
