// SPDX-License-Identifier: CC0-1.0

//! Who owes a connector's asynchronous transitions, and when they are materialised.
//!
//! THE RULE pinned here: every asynchronous change this control plane starts —
//! provisioning a new connector (`PENDING` -> `ACTIVE`), an update
//! (`LastUpdateStatus` `InProgress` -> `Successful`) and a deletion (`DELETING` ->
//! the record is gone) — is owed by the CLOCK. The call that starts it reports the
//! transient value in its own answer; the record keeps that value until the instant
//! the change falls due; and the FIRST call at or after that instant reports the
//! reached value, whichever operation makes it and whether or not anything was
//! called in between. No client action brings the change forward, no number of
//! reads advances it, and none is needed to materialise it: a client that polls
//! converges, and a client that polls nothing finds the change already made on its
//! next call.
//!
//! WHEN each change falls due follows from that. Provisioning and an update settle
//! from the instant the call that started them answered, because
//! `CreateNetworkConnector` tells a client to "use `GetNetworkConnector` to poll the
//! connector state until it reaches `ACTIVE`" and an update's status "transitions
//! through `InProgress` to `Successful`" — promises made to a client polling on ITS
//! own schedule, which may be one single look taken immediately. So a single probe
//! converges, and the transient stays where the model exposes it: in the answer of
//! the call that starts the work. A deletion keeps a window the record is still
//! there for, because the model states a behaviour over it —
//! `DeleteNetworkConnector` is idempotent, "calling delete on a connector that is
//! already deleting ... succeeds without error", while a delete of an identifier
//! this account holds no connector for is the modelled `ResourceNotFoundException`.
//!
//! The state gates (`UpdateNetworkConnector`'s "the connector must be in `ACTIVE`
//! state to accept updates" and `DeleteNetworkConnector`'s "you can delete
//! connectors in `ACTIVE` or `FAILED` states") are stated over a connector whose
//! provisioning the clock still owes, which is a record carrying a due instant it
//! has not reached — whatever put it there.
//!
//! The cases below put a transition on either side of its due instant by moving the
//! instant the record itself carries, so each assertion is decided by the rule
//! rather than by how fast the host runs; the convergence cases take ONE call after
//! the call under test and the end-to-end case polls for the condition, never for a
//! span. The request helpers are the lifecycle suite's, so this suite drives the
//! emulator over HTTP exactly as a client does.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use service_com_amazonaws_lambdacore_lambdacoreapiservice::state::service_state;
use smithy_server_runtime::MultiServiceRouter;

use super::network_connector_lifecycle::{
    at, create, delete, emulator, emulator_with_engine, get, list, list_versions, minimal_create,
    text, update, vpc_egress, Answer, OPERATOR_ROLE,
};

/// How far in the past a case puts a due instant it wants already reached: long
/// enough that the call under test cannot arrive before it.
///
/// `pub(crate)` with [`reschedule`]: the sibling suite
/// `timestamp_and_state_reason_wire` decides its cases by the same clock rather than
/// keeping a second copy of this arrangement.
pub(crate) const ALREADY_DUE: Duration = Duration::from_secs(60);

/// How far in the future a case puts a due instant it wants NOT reached: long
/// enough that no call of a case can arrive after it.
pub(crate) const NOT_YET_DUE: Duration = Duration::from_secs(3_600);

/// How long the one poll loop here keeps looking before it gives up. Generous: the
/// condition is owed by the emulator's clock and nothing here measures how long it
/// took to fall due.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long that poll loop waits between two looks.
const BETWEEN_LOOKS: Duration = Duration::from_millis(25);

/// The subnet an update asks for: never the one `minimal_create` stored, so the
/// update is a configuration CHANGE and therefore appends a version.
const UPDATED_SUBNET: &str = "subnet-0fedcba9876543210";

/// Move the instant at which everything the clock owes the connector `name` falls
/// due, and answer how many owed changes were moved.
///
/// Only the due INSTANT is touched: the state the connector is in, and what the
/// clock owes it, stay exactly what the service itself wrote. That is what lets a
/// case ask "what does the first call after this instant report?" without racing a
/// span.
fn owed_changes_fall_due_at(
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    due: SystemTime,
) -> usize {
    let store = service_state(&**engine).expect("the shared engine holds this service's store");
    let mut store = store.lock();
    let connector = store
        .connectors
        .get_mut(name)
        .unwrap_or_else(|| panic!("the service stored a connector named {name:?}"));
    let mut moved = 0;
    if let Some(pending) = connector.pending.as_mut() {
        pending.due = due;
        moved += 1;
    }
    if let Some(pending) = connector.pending_update.as_mut() {
        pending.due = due;
        moved += 1;
    }
    for version in connector.versions.values_mut() {
        if let Some(pending) = version.pending.as_mut() {
            pending.due = due;
            moved += 1;
        }
    }
    moved
}

/// The same, asserting the call under test left the clock something to owe — a case
/// that moved nothing would prove nothing.
pub(crate) fn reschedule(engine: &Arc<engine::InMemoryEngine>, name: &str, case: &str, due: SystemTime) {
    let moved = owed_changes_fall_due_at(engine, name, due);
    assert!(
        moved > 0,
        "case {case:?}: the call under test must leave the clock a change to owe {name:?}, \
         but the record carries none"
    );
}

/// Which operation a case takes its report of the connector through: every operation
/// this service answers that reports a connector's state, and — for the one that
/// accepts every identifier form — each of those forms.
#[derive(Clone, Copy, Debug)]
enum Report {
    GetByName,
    GetById,
    GetByUnqualifiedArn,
    GetByVersionQualifiedArn,
    ListConnectors,
    ListVersions,
}

/// Every form, so no row's shape can be special-cased.
const EVERY_REPORT: [Report; 6] = [
    Report::GetByName,
    Report::GetById,
    Report::GetByUnqualifiedArn,
    Report::GetByVersionQualifiedArn,
    Report::ListConnectors,
    Report::ListVersions,
];

/// The connector's identity, as the create reported it.
struct Created {
    name: String,
    id: String,
    arn: String,
}

/// Create a connector and assert the create itself carries the transient state the
/// model documents for it ("the network connector starts in `PENDING` state").
async fn create_connector(router: &MultiServiceRouter, case: &str, name: &str) -> Created {
    let created = create(router, minimal_create(name)).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: creating {name:?} must succeed, got {} with {:?}",
        created.status, created.body
    );
    assert_eq!(
        text(&created.body, "/State"),
        "PENDING",
        "case {case:?}: the create that starts provisioning carries the transient state \
         itself, got {:?}",
        at(&created.body, "/State")
    );
    Created {
        name: name.to_string(),
        id: text(&created.body, "/Id"),
        arn: text(&created.body, "/Arn"),
    }
}

/// The `State` `report` reports for `connector`.
async fn reported_state(
    router: &MultiServiceRouter,
    report: Report,
    connector: &Created,
) -> String {
    match report {
        Report::GetByName => text(&get(router, &connector.name).await.body, "/State"),
        Report::GetById => text(&get(router, &connector.id).await.body, "/State"),
        Report::GetByUnqualifiedArn => text(&get(router, &connector.arn).await.body, "/State"),
        Report::GetByVersionQualifiedArn => {
            text(&get(router, &format!("{}:1", connector.arn)).await.body, "/State")
        }
        Report::ListConnectors => {
            text(&list(router, "").await.body, "/NetworkConnectors/0/State")
        }
        Report::ListVersions => text(
            &list_versions(router, &connector.name, "").await.body,
            "/NetworkConnectorVersions/0/State",
        ),
    }
}

/// Look again until `condition` holds of what `GetNetworkConnector` answers for
/// `identifier`. Panics naming `what` when the deadline passes.
async fn poll_get(
    router: &MultiServiceRouter,
    identifier: &str,
    what: &str,
    condition: impl Fn(&Answer) -> bool,
) -> Answer {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answer = get(router, identifier).await;
        if condition(&answer) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "{identifier:?} never {what}: last answer {} with {:?}",
            answer.status,
            answer.body
        );
        std::thread::sleep(BETWEEN_LOOKS);
    }
}

/// Assert `answer` is the modelled 409 for a request the connector's state refuses.
/// The SENTENCE each such refusal states is the `state_gate_refusal_wording` suite's
/// rule; what is pinned here is only that the gate exists while the clock still owes
/// the provisioning.
fn assert_state_gate(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 409,
        "case {case:?}: a connector that is still provisioning refuses this call with HTTP \
         409, got {} with {:?}",
        answer.status, answer.body
    );
    let code = text(&answer.body, "/__type");
    assert!(
        code.ends_with("ResourceConflictException"),
        "case {case:?}: expected the modelled ResourceConflictException, got {code:?} with {:?}",
        answer.body
    );
}

/// RULE, first half: the first call taken at or after the instant provisioning falls
/// due reports `ACTIVE` — whichever operation makes it, even when it is the first
/// call since the create, and nothing is withheld for a later one.
///
/// The rows span every operation that reports a connector's state and every
/// identifier form the one that takes an identifier admits.
#[tokio::test]
async fn the_first_call_after_provisioning_falls_due_reports_active() {
    for report in EVERY_REPORT {
        let case = format!("provisioning reported through {report:?}");
        let (router, engine) = emulator_with_engine();
        let connector = create_connector(&router, &case, "provisioned").await;
        reschedule(
            &engine,
            &connector.name,
            &case,
            SystemTime::now() - ALREADY_DUE,
        );

        let first = reported_state(&router, report, &connector).await;
        assert_eq!(
            first, "ACTIVE",
            "case {case:?}: the FIRST call after provisioning fell due must report the state \
             the connector reached, got {first:?}"
        );
        // And the report is stable: materialising what the clock owed is not an event a
        // later caller sees differently.
        let again = reported_state(&router, report, &connector).await;
        assert_eq!(
            again, "ACTIVE",
            "case {case:?}: a second report of the settled connector answers the same state, \
             got {again:?}"
        );
    }
}

/// RULE, second half: until provisioning falls due the connector stays `PENDING`, no
/// matter how many times it is reported, and the state gates the model states over
/// that window refuse an update and a delete. Reading a resource neither advances
/// nor spends what the clock owes it.
///
/// The rows span every reporting operation and every identifier form again, each
/// taken twice, plus the two operations the state gate refuses.
#[tokio::test]
async fn no_number_of_reads_brings_provisioning_forward() {
    let case = "provisioning the clock does not owe yet";
    let (router, engine) = emulator_with_engine();
    let connector = create_connector(&router, case, "still-provisioning").await;
    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() + NOT_YET_DUE,
    );

    for look in 1..=2 {
        for report in EVERY_REPORT {
            let state = reported_state(&router, report, &connector).await;
            assert_eq!(
                state, "PENDING",
                "case {case:?}: look {look} through {report:?} must still report PENDING, got \
                 {state:?}"
            );
        }
    }

    let refused_update = update(
        &router,
        &connector.id,
        json!({ "Configuration": vpc_egress(&[UPDATED_SUBNET], &[], None, None) }),
    )
    .await;
    assert_state_gate(&format!("{case}: an update"), &refused_update);
    let refused_delete = delete(&router, &connector.id).await;
    assert_state_gate(&format!("{case}: a delete"), &refused_delete);

    let after_refusals = reported_state(&router, Report::GetByName, &connector).await;
    assert_eq!(
        after_refusals, "PENDING",
        "case {case:?}: neither refusal moved the connector, got {after_refusals:?}"
    );

    // The reads did not spend the change either: once the clock reaches it, the next call
    // reports it, with no extra poll earned or needed.
    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() - ALREADY_DUE,
    );
    let settled = reported_state(&router, Report::GetByName, &connector).await;
    assert_eq!(
        settled, "ACTIVE",
        "case {case:?}: the call after provisioning fell due reports ACTIVE, got {settled:?}"
    );
}

/// The same rule over the other two transitions this control plane starts: an
/// update's `LastUpdateStatus` ("transitions through `InProgress` to `Successful`")
/// and a deletion ("the connector transitions to `DELETING` state ... after deletion
/// completes, subsequent calls to GetNetworkConnector return
/// `ResourceNotFoundException`").
#[tokio::test]
async fn an_update_and_a_deletion_are_owed_by_the_clock_the_same_way() {
    let case = "an update in flight";
    let (router, engine) = emulator_with_engine();
    let connector = create_connector(&router, case, "settling").await;
    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() - ALREADY_DUE,
    );

    let updated = update(
        &router,
        &connector.name,
        json!({ "Configuration": vpc_egress(&[UPDATED_SUBNET], &[], None, None) }),
    )
    .await;
    assert_eq!(
        updated.status, 202,
        "case {case:?}: an ACTIVE connector accepts the update, got {} with {:?}",
        updated.status, updated.body
    );
    assert_eq!(
        text(&updated.body, "/LastUpdateStatus"),
        "InProgress",
        "case {case:?}: the update that starts the work carries the transient status itself, \
         got {:?}",
        at(&updated.body, "/LastUpdateStatus")
    );

    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() + NOT_YET_DUE,
    );
    for look in 1..=2 {
        let reported = get(&router, &connector.name).await;
        assert_eq!(
            text(&reported.body, "/LastUpdateStatus"),
            "InProgress",
            "case {case:?}: look {look} must still report the update in flight, got {:?}",
            at(&reported.body, "/LastUpdateStatus")
        );
        let versions = list_versions(&router, &connector.name, "").await;
        assert_eq!(
            at(&versions.body, "/NetworkConnectorVersions/1/State"),
            json!("PENDING"),
            "case {case:?}: look {look} must report the appended version as still \
             provisioning, got {:?}",
            versions.body
        );
    }

    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() - ALREADY_DUE,
    );
    let finished = get(&router, &connector.name).await;
    assert_eq!(
        text(&finished.body, "/LastUpdateStatus"),
        "Successful",
        "case {case:?}: the first call after the update fell due reports Successful, got {:?}",
        at(&finished.body, "/LastUpdateStatus")
    );
    let versions = list_versions(&router, &connector.name, "").await;
    assert_eq!(
        at(&versions.body, "/NetworkConnectorVersions/1/State"),
        json!("ACTIVE"),
        "case {case:?}: the appended version settles with the update, got {:?}",
        versions.body
    );

    // A deletion is the same shape of change, with the record's removal as its outcome.
    let case = "a deletion in flight";
    let removed = delete(&router, &connector.name).await;
    assert_eq!(
        removed.status, 202,
        "case {case:?}: deleting an ACTIVE connector is accepted, got {} with {:?}",
        removed.status, removed.body
    );
    assert_eq!(
        text(&removed.body, "/State"),
        "DELETING",
        "case {case:?}: the delete that starts the cleanup carries the transient state \
         itself, got {:?}",
        at(&removed.body, "/State")
    );

    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() + NOT_YET_DUE,
    );
    for look in 1..=2 {
        let reported = get(&router, &connector.name).await;
        assert_eq!(
            reported.status, 200,
            "case {case:?}: look {look} still names a connector this account has, got {} with \
             {:?}",
            reported.status, reported.body
        );
        assert_eq!(
            text(&reported.body, "/State"),
            "DELETING",
            "case {case:?}: look {look} reports the connector as deleting, got {:?}",
            at(&reported.body, "/State")
        );
    }

    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() - ALREADY_DUE,
    );
    let gone = get(&router, &connector.name).await;
    assert_eq!(
        gone.status, 404,
        "case {case:?}: the first call after the deletion fell due refuses the identifier, \
         got {} with {:?}",
        gone.status, gone.body
    );
    let code = text(&gone.body, "/__type");
    assert!(
        code.ends_with("ResourceNotFoundException"),
        "case {case:?}: expected the modelled ResourceNotFoundException, got {code:?} with {:?}",
        gone.body
    );
    let listed = list(&router, "").await;
    assert_eq!(
        at(&listed.body, "/NetworkConnectors"),
        json!([]),
        "case {case:?}: a connector whose deletion completed is listed nowhere, got {:?}",
        listed.body
    );
}

/// The convergence `CreateNetworkConnector` tells a client to rely on, over the
/// emulator's own clock and with nothing arranged: a client that creates a connector
/// and polls `GetNetworkConnector` reaches `ACTIVE`, and the connector stays there.
///
/// The loop looks for the CONDITION rather than waiting a span, so the case says
/// nothing about how long the transition takes — only that it falls due without any
/// call to make it.
#[tokio::test]
async fn a_client_that_polls_get_reaches_active() {
    let case = "a polling client";
    let router = emulator();
    let connector = create_connector(&router, case, "polled").await;

    let settled = poll_get(&router, &connector.id, "reached ACTIVE", |answer| {
        answer.status == 200 && text(&answer.body, "/State") == "ACTIVE"
    })
    .await;
    assert_eq!(
        text(&settled.body, "/Id"),
        connector.id,
        "case {case:?}: the poll reports the connector the create minted, got {:?}",
        at(&settled.body, "/Id")
    );
    assert_eq!(
        text(&settled.body, "/State"),
        "ACTIVE",
        "case {case:?}: the poll converges on ACTIVE, got {:?}",
        at(&settled.body, "/State")
    );

    let later = get(&router, &connector.id).await;
    assert_eq!(
        text(&later.body, "/State"),
        "ACTIVE",
        "case {case:?}: a connector AWS reports ACTIVE stays ACTIVE, got {:?}",
        at(&later.body, "/State")
    );
    let listed = list(&router, "?State=ACTIVE").await;
    assert_eq!(
        at(&listed.body, "/NetworkConnectors/0/Name"),
        json!(connector.name),
        "case {case:?}: the settled connector is listed among the ACTIVE ones, got {:?}",
        listed.body
    );
}

/// Every configuration a convergence row creates its connector from: the members
/// `NetworkConnectorVpcEgressConfiguration` declares, varied one at a time (`SubnetIds`
/// 1..16, `SecurityGroupIds` 0..16, `NetworkProtocol` one of `IPv4` / `DualStack`,
/// `AssociatedComputeResourceTypes` one of `MicroVm` / `WebFunction`, per
/// `smithy_query(query="shape",shape="NetworkConnectorVpcEgressConfiguration")`), paired
/// with a `Name` shape the model admits. Convergence is a property of the lifecycle, so no
/// row may be the one request shape a recording happened to carry.
fn converging_rows() -> Vec<(&'static str, &'static str, serde_json::Value)> {
    vec![
        (
            "one subnet, nothing else named",
            "converging",
            vpc_egress(&["subnet-0123456789abcdef0"], &[], None, None),
        ),
        (
            "two subnets and two groups, IPv4 for MicroVm",
            "converging_02",
            vpc_egress(
                &["subnet-0123456789abcdef0", "subnet-0fedcba9876543210"],
                &["sg-0123456789abcdef0", "sg-00112233445566778"],
                Some("IPv4"),
                Some("MicroVm"),
            ),
        ),
        (
            "a dual-stack connector for a WebFunction",
            "c",
            vpc_egress(
                &["subnet-00112233445566778"],
                &["sg-0a1b2c3d4e5f60718"],
                Some("DualStack"),
                Some("WebFunction"),
            ),
        ),
        (
            "three subnets, no groups, name spelled like an id",
            "nc-00000000000000ff",
            vpc_egress(
                &[
                    "subnet-0123456789abcdef0",
                    "subnet-0fedcba9876543210",
                    "subnet-0a1b2c3d4e5f60718",
                ],
                &[],
                Some("IPv4"),
                None,
            ),
        ),
    ]
}

/// Create the row's connector and assert its own answer carries the transient state.
/// Answers the identity the create reported.
async fn start_provisioning(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    configuration: serde_json::Value,
) -> Created {
    let created = create(
        router,
        json!({
            "Name": name,
            "Configuration": configuration,
            "OperatorRole": OPERATOR_ROLE,
        }),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: creating {name:?} must succeed, got {} with {:?}",
        created.status, created.body
    );
    assert_eq!(
        text(&created.body, "/State"),
        "PENDING",
        "case {case:?}: the create that starts provisioning carries the transient state \
         itself, got {:?}",
        at(&created.body, "/State")
    );
    Created {
        name: name.to_string(),
        id: text(&created.body, "/Id"),
        arn: text(&created.body, "/Arn"),
    }
}

/// CONVERGENCE, as a client with ONE probe experiences it: the single call a client takes
/// straight after `CreateNetworkConnector` — the first since the create, with nothing in
/// between, nothing waited for and nothing arranged — already reports the state
/// provisioning reached.
///
/// The rows span what the rule generalises over: the configuration and `Name` the connector
/// was created from ([`converging_rows`]), and which operation the one probe is, including
/// every identifier form the read admits.
#[tokio::test]
async fn one_probe_straight_after_the_create_already_reports_active() {
    for (row, name, configuration) in converging_rows() {
        for report in EVERY_REPORT {
            let case = format!("{row}, probed through {report:?}");
            let router = emulator();
            let connector = start_provisioning(&router, &case, name, configuration.clone()).await;

            let first = reported_state(&router, report, &connector).await;
            assert_eq!(
                first, "ACTIVE",
                "case {case:?}: the ONE call a client takes after the create must report the \
                 state provisioning reached, got {first:?}"
            );
        }
    }
}

/// The same for the other work a call of this control plane starts: the single call taken
/// straight after an accepted `UpdateNetworkConnector` reports the update settled —
/// `LastUpdateStatus` `Successful` on the connector, `ACTIVE` on the version it appended —
/// while the update's own answer carries `InProgress`.
///
/// The rows span the configuration the update asks for ([`converging_rows`], whose
/// configurations are all different from the one the create stored) and which operation the
/// one probe is.
#[tokio::test]
async fn one_probe_straight_after_an_update_already_reports_it_settled() {
    for (row, name, configuration) in converging_rows() {
        for probe in [
            "a read by name",
            "a read by id",
            "the version listing",
        ] {
            let case = format!("{row}, probed through {probe}");
            let router = emulator();
            let connector = start_provisioning(
                &router,
                &case,
                name,
                vpc_egress(&["subnet-01234567890abcdef"], &[], None, None),
            )
            .await;
            // One call settles the create, which is what makes the connector ACTIVE and so
            // able to accept the update under test.
            let provisioned = reported_state(&router, Report::GetById, &connector).await;
            assert_eq!(
                provisioned, "ACTIVE",
                "case {case:?}: the update under test needs a provisioned connector, got \
                 {provisioned:?}"
            );

            let updated = update(
                &router,
                &connector.id,
                json!({ "Configuration": configuration.clone() }),
            )
            .await;
            assert_eq!(
                updated.status, 202,
                "case {case:?}: an ACTIVE connector accepts the update, got {} with {:?}",
                updated.status, updated.body
            );
            assert_eq!(
                text(&updated.body, "/LastUpdateStatus"),
                "InProgress",
                "case {case:?}: the update that starts the work carries the transient status \
                 itself, got {:?}",
                at(&updated.body, "/LastUpdateStatus")
            );

            // ONE call, the first since that update.
            let (answer, pointer, expected) = match probe {
                "a read by name" => (
                    get(&router, &connector.name).await,
                    "/LastUpdateStatus",
                    "Successful",
                ),
                "a read by id" => (
                    get(&router, &connector.id).await,
                    "/LastUpdateStatus",
                    "Successful",
                ),
                _ => (
                    list_versions(&router, &connector.name, "").await,
                    "/NetworkConnectorVersions/1/State",
                    "ACTIVE",
                ),
            };
            assert_eq!(
                text(&answer.body, pointer),
                expected,
                "case {case:?}: the ONE call after the update must report {pointer} as \
                 {expected:?}, got {:?}",
                at(&answer.body, pointer)
            );
        }
    }
}
