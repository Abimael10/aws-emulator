// SPDX-License-Identifier: CC0-1.0

//! A TERMINATED environment is ABSENT to every call that acts on a running one.
//!
//! The rule under test: `EnvironmentStatus$Terminated` is "Environment is not
//! running", so an operation that acts on a running environment resolves its
//! `EnvironmentId`/`EnvironmentName` pair among the environments that are still
//! running — a terminated one is refused exactly as an environment that was
//! never created is, in the not-found wording THAT operation answers in, naming
//! the member the caller used and reporting no status. An environment still
//! `Terminating` EXISTS, so it is refused for its STATUS instead, and the READS
//! of an environment still report a terminated one.
//!
//! The table spans what the rule generalises over: which operation acts on the
//! environment, which of the two members names it, and the operation's own
//! not-found template — the account-scoped one for the operations that act on an
//! environment's own lifecycle (the update here, and the swap below, which names
//! two environments and so also names the role of the missing one), the shared
//! one for the rest. The
//! operations-role pair and `ApplyEnvironmentManagedAction` resolve through the
//! same shared rule and are pinned by `refusal_message_templates.rs` and
//! `managed_action_refusal_messages.rs`; what is added here is every other
//! operation that acts on a running environment. The TRANSIENT half of the rule
//! — an environment still `Terminating` refused for its status — is pinned by
//! `refusal_message_templates.rs`; every state this suite asserts on is a
//! settled one, so nothing here depends on how fast the host runs.
//!
//! The harness is the one `lifecycle.rs` owns in this same test binary.

use crate::harness::{wait_for_status, Answer, Emulator, SOLUTION_STACK};
use crate::lifecycle::account;

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The application the fixture environments are launched under.
const APPLICATION: &str = "absent-env-app";

/// The environment every row below acts on: launched, settled and then
/// terminated.
const TERMINATED: &str = "absent-env-gone";

/// The environment the swap rows name as their other side: one that IS running,
/// so a refusal can only be about the terminated side.
const RUNNING: &str = "absent-env-live";

/// Which member of the pair a row names the terminated environment by.
#[derive(Debug, Clone, Copy)]
enum By {
    Name,
    Id,
}

/// One operation acting on the terminated environment, named by one member.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    action: &'static str,
    by: By,
    /// Whether this operation words its not-found refusal in the ACCOUNT-SCOPED
    /// template (the operations that act on an environment's own lifecycle)
    /// rather than in the shared one.
    in_account: bool,
}

/// One row per operation that acts on a running environment, named by each
/// member it takes.
const ROWS: &[Row] = &[
    Row {
        case: "RebuildEnvironment by name",
        action: "RebuildEnvironment",
        by: By::Name,
        in_account: false,
    },
    Row {
        case: "RebuildEnvironment by id",
        action: "RebuildEnvironment",
        by: By::Id,
        in_account: false,
    },
    Row {
        case: "RestartAppServer by name",
        action: "RestartAppServer",
        by: By::Name,
        in_account: false,
    },
    Row {
        case: "RestartAppServer by id",
        action: "RestartAppServer",
        by: By::Id,
        in_account: false,
    },
    Row {
        case: "AbortEnvironmentUpdate by name",
        action: "AbortEnvironmentUpdate",
        by: By::Name,
        in_account: false,
    },
    Row {
        case: "AbortEnvironmentUpdate by id",
        action: "AbortEnvironmentUpdate",
        by: By::Id,
        in_account: false,
    },
    // The update changes the environment's own configuration, so it answers in
    // the account-scoped template.
    Row {
        case: "UpdateEnvironment by name",
        action: "UpdateEnvironment",
        by: By::Name,
        in_account: true,
    },
    Row {
        case: "UpdateEnvironment by id",
        action: "UpdateEnvironment",
        by: By::Id,
        in_account: true,
    },
];

/// Create the application the environments are launched under.
async fn given_application(emulator: &Emulator, case: &str) {
    let created = emulator
        .call("CreateApplication", &[("ApplicationName", APPLICATION)])
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateApplication, got {} and body {}",
        created.status, created.body
    );
}

/// Launch `name` and answer its `EnvironmentId`.
async fn given_environment(emulator: &Emulator, case: &str, name: &str) -> String {
    let launched = emulator
        .call(
            "CreateEnvironment",
            &[
                ("ApplicationName", APPLICATION),
                ("EnvironmentName", name),
                ("SolutionStackName", SOLUTION_STACK.as_str()),
            ],
        )
        .await;
    assert_eq!(
        launched.status, 200,
        "case {case:?}: expected 200 from CreateEnvironment for {name:?}, got {} and body {}",
        launched.status, launched.body
    );
    launched
        .value("EnvironmentId")
        .expect("the launch reports the environment's id")
        .to_string()
}

/// The message every operation that answers in the shared template refuses the
/// absent environment with, built the way the service builds it.
fn no_environment_found(member: &str, value: &str) -> String {
    format!("No Environment found for {member} = '{value}'.")
}

/// The message the operations that act on an environment's own LIFECYCLE refuse
/// it with instead: the KIND of identifier the request used (`Id` or `Name`),
/// its value and the account the lookup was made in, led by the role the
/// request gave that environment where it named more than one. Written out here
/// rather than imported from the service, so a change to the service's builder
/// fails this suite instead of moving with it.
fn missing_environment_in_account(role: Option<&str>, kind: &str, value: &str) -> String {
    let subject = match role {
        Some(role) => format!("{role} environment"),
        None => "Environment".to_string(),
    };
    format!(
        "{subject} not found with {kind}: {value} in account '{}'",
        account()
    )
}

/// Assert one refusal: the status, the code and the message.
fn assert_absent(refused: &Answer, case: &str, expected: &str) {
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
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        refused.error_message()
    );
    // The refusal says the environment is gone, not that it is busy: it reports
    // no status and does not state a state the operation needs.
    for leaked in [eb::state::STATUS_TERMINATED, "invalid state"] {
        assert!(
            !refused.error_message().contains(leaked),
            "case {case:?}: the not-found refusal must not report {leaked:?}, got {:?}",
            refused.error_message()
        );
    }
}

/// Every operation that acts on a running environment refuses a terminated one
/// as absent, naming the member the caller used.
#[tokio::test]
async fn a_terminated_environment_is_absent_to_every_call_that_acts_on_one() {
    let case = "the fixture";
    let emulator = Emulator::new();
    given_application(&emulator, case).await;
    let environment_id = given_environment(&emulator, case, TERMINATED).await;
    given_environment(&emulator, case, RUNNING).await;
    wait_for_status(&emulator, case, TERMINATED, eb::state::STATUS_READY).await;
    wait_for_status(&emulator, case, RUNNING, eb::state::STATUS_READY).await;
    let terminated = emulator
        .call("TerminateEnvironment", &[("EnvironmentName", TERMINATED)])
        .await;
    assert_eq!(
        terminated.status, 200,
        "case {case:?}: expected 200 from TerminateEnvironment, got {} and body {}",
        terminated.status, terminated.body
    );
    wait_for_status(
        &emulator,
        case,
        TERMINATED,
        eb::state::STATUS_TERMINATED,
    )
    .await;

    for row in ROWS {
        let (member, value) = match row.by {
            By::Name => ("EnvironmentName", TERMINATED),
            By::Id => ("EnvironmentId", environment_id.as_str()),
        };
        let expected = if row.in_account {
            let kind = match row.by {
                By::Name => "Name",
                By::Id => "Id",
            };
            missing_environment_in_account(None, kind, value)
        } else {
            no_environment_found(member, value)
        };
        let refused = emulator.call(row.action, &[(member, value)]).await;
        assert_absent(&refused, row.case, &expected);
    }

    // The swap names TWO environments, so it answers in its own not-found
    // template — the one that names the role the missing environment played —
    // and the terminated side is absent to it just the same.
    let swapped = emulator
        .call(
            "SwapEnvironmentCNAMEs",
            &[
                ("SourceEnvironmentName", TERMINATED),
                ("DestinationEnvironmentName", RUNNING),
            ],
        )
        .await;
    assert_absent(
        &swapped,
        "SwapEnvironmentCNAMEs with a terminated source",
        &missing_environment_in_account(Some("source"), "Name", TERMINATED),
    );

    // The reads of an environment still report the terminated record: the rule
    // is about acting on one, not about seeing it.
    let described = emulator
        .call(
            "DescribeEnvironments",
            &[
                ("EnvironmentNames.member.1", TERMINATED),
                ("IncludeDeleted", "true"),
            ],
        )
        .await;
    assert_eq!(
        described.values_at("Environments/member/Status"),
        vec![eb::state::STATUS_TERMINATED],
        "case {case:?}: the terminated environment must still be reported, got body {}",
        described.body
    );
}

/// An environment still TERMINATING is refused for its STATUS rather than as
/// absent — it exists, and `DescribeEnvironments` reports it without being asked
/// for deleted records. That half of the rule is pinned by
/// `refusal_message_templates.rs`, whose own fixtures put an environment into
/// that transient status immediately before the one call each row makes; this
/// suite asserts only the settled half, so nothing here depends on how fast the
/// host runs.
#[tokio::test]
async fn the_reads_of_a_terminated_environment_still_report_it() {
    let case = "a terminated environment read back";
    let emulator = Emulator::new();
    given_application(&emulator, case).await;
    let name = "absent-env-read";
    given_environment(&emulator, case, name).await;
    wait_for_status(&emulator, case, name, eb::state::STATUS_READY).await;
    let terminated = emulator
        .call("TerminateEnvironment", &[("EnvironmentName", name)])
        .await;
    assert_eq!(
        terminated.status, 200,
        "case {case:?}: expected 200 from TerminateEnvironment, got {} and body {}",
        terminated.status, terminated.body
    );
    wait_for_status(&emulator, case, name, eb::state::STATUS_TERMINATED).await;

    // The record is still there for the reads that report an environment: the
    // rule is about ACTING on one.
    let resources = emulator
        .call(
            "DescribeEnvironmentResources",
            &[("EnvironmentName", name)],
        )
        .await;
    assert_eq!(
        resources.status, 200,
        "case {case:?}: a read of a terminated environment is answered, got {} and body {}",
        resources.status, resources.body
    );
    let health = emulator
        .call(
            "DescribeEnvironmentHealth",
            &[
                ("EnvironmentName", name),
                ("AttributeNames.member.1", "Status"),
            ],
        )
        .await;
    assert_eq!(
        health.status, 200,
        "case {case:?}: a health read of a terminated environment is answered, got {} and body {}",
        health.status, health.body
    );
}
