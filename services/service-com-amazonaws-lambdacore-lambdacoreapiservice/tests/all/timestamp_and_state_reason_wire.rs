// SPDX-License-Identifier: CC0-1.0

//! The wire form of this service's `@timestampFormat("date-time")` members, and the
//! `StateReason` that accompanies every reported `State`.
//!
//! Two rules, each asserted over a SET of inputs rather than over one recorded response:
//!
//! 1. A member whose target carries `@timestampFormat("date-time")` goes onto the
//!    restJson1 wire as an RFC 3339 / ISO-8601 date-time STRING, never as the epoch NUMBER
//!    the protocol default would produce. Every timestamp this service models targets
//!    `com.amazonaws.lambdacore#CoreTimestamp`
//!    (`smithy_query(query="shape",shape="CoreTimestamp")` reports
//!    `"timestampFormat": "date-time"`), and the four response shapes that carry one are
//!    `GetNetworkConnectorResponse`, `UpdateNetworkConnectorResponse`,
//!    `NetworkConnectorSummary` and `NetworkConnectorVersionSummary` — so the rows below
//!    span every operation that reports an instant, each lookup form the identifier
//!    accepts, and both list projections.
//!
//! 2. A reported `State` comes with the human-readable `StateReason` for it: the reason
//!    names the event that set that state, changes when another event sets a new one, and
//!    is reported identically whichever identifier form named the resource. The rows span
//!    the states this control plane reaches — `PENDING` while provisioning, `ACTIVE` once
//!    the clock settles it, `DELETING` once a delete starts — and both resources that
//!    model the member (the connector and one of its versions).
//!
//! 3. Rule 2 over EVERY status this control plane reports, not just `State`: a status
//!    member is reported with the narrative member that explains it, whether the status was
//!    set by the call itself or by the clock when a transition fell due, and the narrative
//!    states the event behind the status it accompanies. The pairs this service models on a
//!    connector are `State`/`StateReason` and `LastUpdateStatus`/`LastUpdateStatusReason`
//!    (`smithy_query(query="operation",operation="GetNetworkConnector")`), and an update is
//!    the event both of them answer to: it restates the connector's `StateReason`, states
//!    the in-flight update narrative, and hands the clock the narrative of the status it
//!    settles on. The rows below span what the update changed, which identifier form named
//!    the connector, how many updates it has had, which operation reports the members, and
//!    both sides of the instant the update falls due.
//!
//! The harness (router, request helpers, the clock wait) is the sibling suite's; it is
//! `pub(crate)` there so both suites drive the emulator the same way.

use std::sync::Arc;
use std::time::SystemTime;

use aws_smithy_schema::traits::TimestampFormat;
use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use service_com_amazonaws_lambdacore_lambdacoreapiservice::ops::types::{
    GetNetworkConnectorOutput, NetworkConnectorSummary, NetworkConnectorVersionSummary,
    UpdateNetworkConnectorOutput,
};
use smithy_server_runtime::MultiServiceRouter;

use super::lifecycle_convergence::{reschedule, ALREADY_DUE, NOT_YET_DUE};
use super::network_connector_lifecycle::{
    at, create, delete, emulator, emulator_with_engine, expected_arn, get, list, list_versions,
    minimal_configuration, minimal_create, provisioned, text, update, vpc_egress, Answer,
    OPERATOR_ROLE,
};

/// Assert that the JSON value at `pointer` is the wire form a `date-time` member requires:
/// a STRING that parses as an RFC 3339 instant and renders back to the very same text.
///
/// A bare number is exactly the defect this pins — the restJson1 default for a timestamp
/// whose member schema reports no format — so the string check comes first and names what
/// it saw.
fn assert_iso8601(case: &str, body: &Value, pointer: &str) {
    let value = at(body, pointer);
    let Some(rendered) = value.as_str() else {
        panic!(
            "case {case:?}: {pointer} is modelled @timestampFormat(\"date-time\"), so it must \
             be an ISO-8601 STRING, got {value:?}"
        );
    };
    let parsed = DateTime::from_str(rendered, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: {pointer} must parse as an RFC 3339 date-time ({error}), got {rendered:?}")
    });
    let canonical = parsed
        .fmt(Format::DateTime)
        .unwrap_or_else(|error| panic!("case {case:?}: {pointer} must re-render ({error})"));
    assert_eq!(
        canonical, rendered,
        "case {case:?}: {pointer} must already be in the canonical date-time form, got \
         {rendered:?} which renders as {canonical:?}"
    );
    assert!(
        rendered.contains('T') && rendered.ends_with('Z'),
        "case {case:?}: {pointer} must be a UTC ISO-8601 date-time (T separator, Z offset), \
         got {rendered:?}"
    );
}

/// The `StateReason` an answer reports for the `State` at the same level.
fn state_and_reason(case: &str, body: &Value, prefix: &str) -> (String, String) {
    let state = text(body, &format!("{prefix}/State"));
    assert!(
        !state.is_empty(),
        "case {case:?}: {prefix}/State must be reported, got {:?}",
        at(body, &format!("{prefix}/State"))
    );
    let reason = at(body, &format!("{prefix}/StateReason"));
    let Some(reason) = reason.as_str() else {
        panic!(
            "case {case:?}: {prefix}/State is {state:?}, so {prefix}/StateReason must explain \
             it, got {reason:?}"
        );
    };
    assert!(
        !reason.trim().is_empty(),
        "case {case:?}: {prefix}/StateReason must be human-readable text, got {reason:?}"
    );
    // Every `NetworkConnectorStateReasonCode` value names a provisioning FAILURE
    // (`InvalidSubnet`, `InternalError`, …), which this emulator never produces, so no code
    // describes a state it does reach and the member must stay absent rather than be
    // invented.
    assert_eq!(
        at(body, &format!("{prefix}/StateReasonCode")),
        Value::Null,
        "case {case:?}: {prefix}/StateReasonCode names a failure this emulator never \
         produces, so it must be absent, got {:?}",
        at(body, &format!("{prefix}/StateReasonCode"))
    );
    (state, reason.to_string())
}

/// A configuration that differs from [`minimal_configuration`] in every member, so an
/// update built from it really is a configuration change and appends a version.
fn changed_configuration() -> Value {
    vpc_egress(
        &["subnet-0123456789abcdef0", "subnet-0fedcba9876543210"],
        &["sg-0123456789abcdef0"],
        Some("DualStack"),
        Some("MicroVm"),
    )
}

/// RULE 1 on the wire: every response of this service that reports an instant reports it
/// as an ISO-8601 date-time string.
///
/// The rows differ along what the rule generalises over: the operation (Get, Update, the
/// two Lists), the identifier form (name, id, unqualified ARN, version-qualified ARN) and
/// the shape the member sits in (two response shapes and two summary projections).
#[tokio::test]
async fn every_date_time_member_is_an_iso8601_string_on_the_wire() {
    let router = emulator();
    let name = "wire-shaped";
    let created = create(&router, minimal_create(name)).await;
    assert_eq!(
        created.status, 202,
        "expected HTTP 202 from CreateNetworkConnector, got {} with {:?}",
        created.status, created.body
    );
    let id = text(&created.body, "/Id");
    let arn = expected_arn(name);

    // An update is accepted once the create's provisioning has fallen due and left the
    // connector ACTIVE, and it is what gives the versions list a second row.
    provisioned(&router, name).await;
    let updated = update(&router, name, json!({ "Configuration": changed_configuration() })).await;
    assert_eq!(
        updated.status, 202,
        "expected HTTP 202 from UpdateNetworkConnector, got {} with {:?}",
        updated.status, updated.body
    );

    let by_name = get(&router, name).await;
    let by_id = get(&router, &id).await;
    let by_arn = get(&router, &arn).await;
    let by_version_arn = get(&router, &format!("{arn}:1")).await;
    let listed = list(&router, "").await;
    let versions = list_versions(&router, name, "").await;

    let rows: Vec<(&str, &Answer, &str)> = vec![
        ("GetNetworkConnector by name", &by_name, "/LastModified"),
        ("GetNetworkConnector by id", &by_id, "/LastModified"),
        (
            "GetNetworkConnector by unqualified ARN",
            &by_arn,
            "/LastModified",
        ),
        (
            "GetNetworkConnector by version-qualified ARN",
            &by_version_arn,
            "/LastModified",
        ),
        ("UpdateNetworkConnector", &updated, "/LastModified"),
        (
            "ListNetworkConnectors summary",
            &listed,
            "/NetworkConnectors/0/LastModified",
        ),
        (
            "ListNetworkConnectorVersions, version 1",
            &versions,
            "/NetworkConnectorVersions/0/LastModified",
        ),
        (
            "ListNetworkConnectorVersions, version 2",
            &versions,
            "/NetworkConnectorVersions/1/LastModified",
        ),
    ];

    for (case, answer, pointer) in rows {
        assert_eq!(
            answer.status / 100,
            2,
            "case {case:?}: expected a 2xx answer, got {} with {:?}",
            answer.status,
            answer.body
        );
        assert_iso8601(case, &answer.body, pointer);
    }

    // The three equivalent lookup forms describe one record, so they report one instant.
    for (case, answer) in [("by id", &by_id), ("by unqualified ARN", &by_arn)] {
        assert_eq!(
            at(&answer.body, "/LastModified"),
            at(&by_name.body, "/LastModified"),
            "case {case:?}: an equivalent lookup reports the same LastModified as the \
             lookup by name, got {:?}",
            at(&answer.body, "/LastModified")
        );
    }
}

/// RULE 1 on the schema: every member the model marks `@timestampFormat("date-time")`
/// carries that format on its member schema, which is what the JSON codec reads to pick
/// the wire form — and a member the model does not mark carries none.
///
/// The marked members are every `CoreTimestamp` member of this service's model
/// (`smithy_query(query="shape",shape="CoreTimestamp")` plus `query="shape"` on the four
/// shapes below); `Arn` and `Version` stand for the members that must stay unmarked.
#[test]
fn every_date_time_member_schema_reports_the_modelled_format() {
    let marked = [
        (
            "GetNetworkConnectorResponse$LastModified",
            &GetNetworkConnectorOutput::M_LAST_MODIFIED,
        ),
        (
            "UpdateNetworkConnectorResponse$LastModified",
            &UpdateNetworkConnectorOutput::M_LAST_MODIFIED,
        ),
        (
            "NetworkConnectorSummary$LastModified",
            &NetworkConnectorSummary::M_LAST_MODIFIED,
        ),
        (
            "NetworkConnectorVersionSummary$LastModified",
            &NetworkConnectorVersionSummary::M_LAST_MODIFIED,
        ),
    ];
    for (case, schema) in marked {
        let format = schema.timestamp_format().map(|trait_| trait_.format());
        assert!(
            matches!(format, Some(TimestampFormat::DateTime)),
            "case {case:?}: the member schema must carry @timestampFormat(date-time), got \
             {format:?}"
        );
    }

    let unmarked = [
        ("GetNetworkConnectorResponse$Arn", &GetNetworkConnectorOutput::M_ARN),
        (
            "NetworkConnectorVersionSummary$Version",
            &NetworkConnectorVersionSummary::M_VERSION,
        ),
    ];
    for (case, schema) in unmarked {
        let format = schema.timestamp_format().map(|trait_| trait_.format());
        assert!(
            format.is_none(),
            "case {case:?}: a member the model does not mark carries no timestamp format, \
             got {format:?}"
        );
    }
}

/// RULE 2: the `State` a read reports comes with the `StateReason` for it — in every state
/// this control plane reaches, through every identifier form, and on a version as well as
/// on the connector.
#[tokio::test]
async fn a_reported_state_always_carries_the_reason_for_it() {
    let (router, engine) = emulator_with_engine();
    let name = "state-reasoned";
    let created = create(&router, minimal_create(name)).await;
    assert_eq!(
        created.status, 202,
        "expected HTTP 202 from CreateNetworkConnector, got {} with {:?}",
        created.status, created.body
    );
    let id = text(&created.body, "/Id");
    let arn = expected_arn(name);

    // While provisioning is still owed — the window held open by moving the instant the
    // record carries — the connector is in its transient state, and the reason for it is
    // reported, which is the case the issue's recording covers.
    reschedule(
        &engine,
        name,
        "a connector still provisioning",
        SystemTime::now() + NOT_YET_DUE,
    );
    let pending_forms = [
        ("PENDING, looked up by name", name.to_string()),
        ("PENDING, looked up by id", id.clone()),
        ("PENDING, looked up by unqualified ARN", arn.clone()),
        (
            "PENDING, looked up by version-qualified ARN",
            format!("{arn}:1"),
        ),
    ];
    let mut pending_reason = String::new();
    for (case, identifier) in &pending_forms {
        let answer = get(&router, identifier).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        let (state, reason) = state_and_reason(case, &answer.body, "");
        assert_eq!(
            state, "PENDING",
            "case {case:?}: a connector still provisioning is PENDING, got {state:?}"
        );
        if pending_reason.is_empty() {
            pending_reason = reason;
        } else {
            assert_eq!(
                reason, pending_reason,
                "case {case:?}: every equivalent lookup form reports one record, so one \
                 StateReason, got {reason:?}"
            );
        }
    }

    // Once the clock settles provisioning the state moves, and the reason the connector
    // reached it is still reported with it.
    reschedule(
        &engine,
        name,
        "a connector whose provisioning has fallen due",
        SystemTime::now() - ALREADY_DUE,
    );
    let settled = get(&router, name).await;
    let (state, active_reason) = state_and_reason("ACTIVE after provisioning", &settled.body, "");
    assert_eq!(
        state, "ACTIVE",
        "provisioning settles a connector to ACTIVE, got {state:?}"
    );

    // An update appends a version. Each version reports its own state with its own reason,
    // and the two were put in that state by different events. The appended version is still
    // provisioning for as long as the clock owes that, which the case holds open.
    let updated = update(&router, name, json!({ "Configuration": changed_configuration() })).await;
    assert_eq!(
        updated.status, 202,
        "expected HTTP 202 from UpdateNetworkConnector, got {} with {:?}",
        updated.status, updated.body
    );
    reschedule(
        &engine,
        name,
        "an update still in flight",
        SystemTime::now() + NOT_YET_DUE,
    );
    let versions = list_versions(&router, name, "").await;
    let (first_state, first_reason) =
        state_and_reason("version 1", &versions.body, "/NetworkConnectorVersions/0");
    let (second_state, second_reason) =
        state_and_reason("version 2", &versions.body, "/NetworkConnectorVersions/1");
    assert_eq!(
        first_state, "ACTIVE",
        "version 1 finished provisioning before the update, got {first_state:?}"
    );
    assert_eq!(
        second_state, "PENDING",
        "the version an update appends provisions in turn, got {second_state:?}"
    );
    assert_eq!(
        first_reason, active_reason,
        "version 1 and the connector were put in their state by the same creation, so they \
         report the same reason, got {first_reason:?} and {active_reason:?}"
    );
    assert_ne!(
        second_reason, first_reason,
        "a version appended by an update was put in its state by that update, not by the \
         creation, so its reason differs, got {second_reason:?}"
    );

    // A delete moves the connector to its own state, with the reason for THAT state. The
    // record is still there for as long as the clock owes the removal, which the case holds
    // open rather than racing.
    let deleted = delete(&router, name).await;
    assert_eq!(
        deleted.status, 202,
        "expected HTTP 202 from DeleteNetworkConnector, got {} with {:?}",
        deleted.status, deleted.body
    );
    reschedule(
        &engine,
        name,
        "a deletion the clock still owes",
        SystemTime::now() + NOT_YET_DUE,
    );
    let deleting = get(&router, name).await;
    let (state, deleting_reason) = state_and_reason("DELETING after a delete", &deleting.body, "");
    assert_eq!(
        state, "DELETING",
        "a delete moves the connector to DELETING while its resources are cleaned up, got \
         {state:?}"
    );
    assert_ne!(
        deleting_reason, active_reason,
        "the state changed, so the reason naming the event that set it changed too, got \
         {deleting_reason:?}"
    );
}

// --- RULE 3: every reported status comes with the narrative that explains it -------------

/// Every (status, narrative) member pair a connector's description reports, from
/// `smithy_query(query="operation",operation="GetNetworkConnector")`: `State` with
/// `StateReason`, and `LastUpdateStatus` with `LastUpdateStatusReason`.
const STATUS_NARRATIVE_PAIRS: [(&str, &str); 2] = [
    ("/State", "/StateReason"),
    ("/LastUpdateStatus", "/LastUpdateStatusReason"),
];

/// Two of the three `NetworkConnectorLastUpdateStatus` values (`[Failed, InProgress,
/// Successful]`): the one an accepted update carries while it is in flight, and the one it
/// settles on. `Failed` is reached by no update this control plane accepts.
const IN_PROGRESS: &str = "InProgress";
const SUCCESSFUL: &str = "Successful";

/// The narrative reported beside [`IN_PROGRESS`]: an update that has been accepted and has
/// not settled yet. Fixed text — the status it explains has no request- or state-derived
/// part, so nothing of this sentence varies.
const IN_FLIGHT_UPDATE_NARRATIVE: &str = "Update in progress";

/// The narrative reported beside [`SUCCESSFUL`], once the update in flight has settled.
const SETTLED_UPDATE_NARRATIVE: &str = "Update was successful";

/// The `StateReason` of a connector whose latest lifecycle event is an accepted update:
/// `StateReason` explains the connector's current state, and an update is the event that
/// last moved it.
const UPDATE_INITIATED_NARRATIVE: &str = "Update initiated";

/// An `OperatorRole` that is not the one `minimal_create` stored, so an update carrying it
/// alone is a configuration CHANGE. Its spelling is what `NetworkConnectorRoleArn`'s
/// pattern admits.
const CHANGED_OPERATOR_ROLE: &str =
    "arn:aws:iam::000000000000:role/LambdaNetworkConnectorOperatorTwo";

/// How a case names the connector: every form `UpdateNetworkConnector` accepts ("you can
/// specify the connector ID, name, or full ARN").
///
/// `pub(crate)` with [`EVERY_FORM`]: a sibling suite that varies which spelling addresses the
/// connector uses this one rather than keeping a second copy of the form set.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NamedBy {
    Name,
    Id,
    Arn,
}

/// Every form, so no row's shape can be special-cased.
pub(crate) const EVERY_FORM: [NamedBy; 3] = [NamedBy::Name, NamedBy::Id, NamedBy::Arn];

/// The connector's identity as the create reported it, in each form an identifier takes.
struct Identity {
    name: String,
    id: String,
    arn: String,
}

impl Identity {
    /// This connector, spelled the way `form` names it.
    fn spelled(&self, form: NamedBy) -> &str {
        match form {
            NamedBy::Name => &self.name,
            NamedBy::Id => &self.id,
            NamedBy::Arn => &self.arn,
        }
    }
}

/// RULE 3, the pairing itself: a status member and the narrative member that explains it
/// are reported TOGETHER. A reported status comes with human-readable text for it, and a
/// narrative is never reported for a status the record does not hold.
fn assert_paired(case: &str, body: &Value, status_pointer: &str, narrative_pointer: &str) {
    let status = at(body, status_pointer);
    let narrative = at(body, narrative_pointer);
    match status.as_str() {
        Some(status) => {
            let Some(narrative) = narrative.as_str() else {
                panic!(
                    "case {case:?}: {status_pointer} is {status:?}, so {narrative_pointer} must \
                     explain it, got {narrative:?}"
                );
            };
            assert!(
                !narrative.trim().is_empty(),
                "case {case:?}: {narrative_pointer} must be human-readable text, got \
                 {narrative:?}"
            );
        }
        None => assert_eq!(
            narrative,
            Value::Null,
            "case {case:?}: {status_pointer} is not reported ({status:?}), so there is no status \
             for {narrative_pointer} to explain, got {narrative:?}"
        ),
    }
}

/// Every pair of [`STATUS_NARRATIVE_PAIRS`], checked by [`assert_paired`].
fn assert_every_pair(case: &str, body: &Value) {
    for (status_pointer, narrative_pointer) in STATUS_NARRATIVE_PAIRS {
        assert_paired(case, body, status_pointer, narrative_pointer);
    }
}

/// Create a connector whose provisioning has already fallen due, so it accepts an update,
/// and answer its identity in every form.
async fn connector_ready_to_update(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
) -> Identity {
    let created = create(router, minimal_create(name)).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: creating {name:?} must succeed, got {} with {:?}",
        created.status, created.body
    );
    reschedule(engine, name, case, SystemTime::now() - ALREADY_DUE);
    Identity {
        name: name.to_string(),
        id: text(&created.body, "/Id"),
        arn: text(&created.body, "/Arn"),
    }
}

/// The update requests a case drives, differing along what the update CHANGES — the two
/// members `UpdateNetworkConnector` takes, each alone and both together.
fn changing_requests() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a changed configuration",
            json!({ "Configuration": changed_configuration() }),
        ),
        (
            "a changed operator role",
            json!({ "OperatorRole": CHANGED_OPERATOR_ROLE }),
        ),
        (
            "a changed configuration and operator role",
            json!({
                "Configuration": changed_configuration(),
                "OperatorRole": CHANGED_OPERATOR_ROLE,
            }),
        ),
    ]
}

/// RULE 3 on the accepting call: the 202 of an accepted update reports the transient
/// `LastUpdateStatus` with the narrative for it, whatever the update changed and whichever
/// identifier form named the connector.
#[tokio::test]
async fn an_accepted_update_answers_the_in_flight_status_with_its_narrative() {
    for (change, request) in changing_requests() {
        for form in EVERY_FORM {
            let case = format!("{change}, named by {form:?}");
            let (router, engine) = emulator_with_engine();
            let connector =
                connector_ready_to_update(&router, &engine, &case, "narrated-update").await;

            let updated = update(&router, connector.spelled(form), request.clone()).await;
            assert_eq!(
                updated.status, 202,
                "case {case:?}: an ACTIVE connector accepts the update, got {} with {:?}",
                updated.status, updated.body
            );
            assert_paired(
                &case,
                &updated.body,
                "/LastUpdateStatus",
                "/LastUpdateStatusReason",
            );
            assert_eq!(
                text(&updated.body, "/LastUpdateStatus"),
                IN_PROGRESS,
                "case {case:?}: the call that starts the update carries the transient status, \
                 got {:?}",
                at(&updated.body, "/LastUpdateStatus")
            );
            assert_eq!(
                text(&updated.body, "/LastUpdateStatusReason"),
                IN_FLIGHT_UPDATE_NARRATIVE,
                "case {case:?}: the in-flight status is reported with its own narrative, got \
                 {:?}",
                at(&updated.body, "/LastUpdateStatusReason")
            );
        }
    }
}

/// RULE 3 on the read: once an update is accepted, a description of the connector states
/// that update as the connector's latest lifecycle event and reports the update status with
/// its narrative — the in-flight one until the update falls due, the settled one from the
/// first call at or after that instant, with the pairing holding on both sides of it.
///
/// The rows span every identifier form, two looks on each side of the due instant (a read
/// neither advances nor spends what the clock owes), and a SECOND update, which restates
/// the in-flight narrative rather than leaving the settled one standing.
#[tokio::test]
async fn a_read_after_an_update_reports_the_update_as_the_latest_event() {
    let case = "the first update";
    let (router, engine) = emulator_with_engine();
    let connector = connector_ready_to_update(&router, &engine, case, "narrated-read").await;

    // The creation narrative is what the connector carries before any update, so the
    // restatement below is a change rather than the text it already had.
    let before = get(&router, &connector.name).await;
    assert_every_pair("before any update", &before.body);
    let creation_narrative = text(&before.body, "/StateReason");
    assert!(
        !creation_narrative.is_empty(),
        "case {case:?}: a created connector reports the narrative of its creation, got {:?}",
        at(&before.body, "/StateReason")
    );
    assert_eq!(
        at(&before.body, "/LastUpdateStatus"),
        Value::Null,
        "case {case:?}: a connector no update has touched reports no update status, got {:?}",
        at(&before.body, "/LastUpdateStatus")
    );

    let updated = update(
        &router,
        &connector.name,
        json!({ "Configuration": changed_configuration() }),
    )
    .await;
    assert_eq!(
        updated.status, 202,
        "case {case:?}: the update is accepted, got {} with {:?}",
        updated.status, updated.body
    );

    // While the update is still owed, every form reports the in-flight status with its
    // narrative, and the connector's own StateReason states the update.
    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() + NOT_YET_DUE,
    );
    for look in 1..=2 {
        for form in EVERY_FORM {
            let case = format!("{case}, in flight, look {look} by {form:?}");
            let reported = get(&router, connector.spelled(form)).await;
            assert_eq!(
                reported.status, 200,
                "case {case:?}: expected HTTP 200, got {} with {:?}",
                reported.status, reported.body
            );
            assert_every_pair(&case, &reported.body);
            assert_eq!(
                text(&reported.body, "/LastUpdateStatus"),
                IN_PROGRESS,
                "case {case:?}: the update the clock still owes is reported in flight, got {:?}",
                at(&reported.body, "/LastUpdateStatus")
            );
            assert_eq!(
                text(&reported.body, "/LastUpdateStatusReason"),
                IN_FLIGHT_UPDATE_NARRATIVE,
                "case {case:?}: the in-flight status is reported with its narrative, got {:?}",
                at(&reported.body, "/LastUpdateStatusReason")
            );
            assert_eq!(
                text(&reported.body, "/StateReason"),
                UPDATE_INITIATED_NARRATIVE,
                "case {case:?}: the accepted update is the connector's latest lifecycle event, \
                 so StateReason states it instead of {creation_narrative:?}, got {:?}",
                at(&reported.body, "/StateReason")
            );
        }
    }

    // From the instant the update falls due, the status the clock reached is reported with
    // ITS narrative — and the state narrative still names the event that set the state.
    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() - ALREADY_DUE,
    );
    for look in 1..=2 {
        for form in EVERY_FORM {
            let case = format!("{case}, settled, look {look} by {form:?}");
            let settled = get(&router, connector.spelled(form)).await;
            assert_every_pair(&case, &settled.body);
            assert_eq!(
                text(&settled.body, "/LastUpdateStatus"),
                SUCCESSFUL,
                "case {case:?}: the update the clock owed has settled, got {:?}",
                at(&settled.body, "/LastUpdateStatus")
            );
            assert_eq!(
                text(&settled.body, "/LastUpdateStatusReason"),
                SETTLED_UPDATE_NARRATIVE,
                "case {case:?}: the status the clock reached carries its own narrative, got {:?}",
                at(&settled.body, "/LastUpdateStatusReason")
            );
            assert_eq!(
                text(&settled.body, "/StateReason"),
                UPDATE_INITIATED_NARRATIVE,
                "case {case:?}: the update is still the event that set the connector's state, \
                 got {:?}",
                at(&settled.body, "/StateReason")
            );
        }
    }

    // A second update is a second event: the narrative is restated for it, not written once
    // and left at the previous update's outcome.
    let case = "the second update";
    let again = update(
        &router,
        &connector.id,
        json!({ "OperatorRole": CHANGED_OPERATOR_ROLE }),
    )
    .await;
    assert_eq!(
        again.status, 202,
        "case {case:?}: the second update is accepted, got {} with {:?}",
        again.status, again.body
    );
    assert_eq!(
        text(&again.body, "/LastUpdateStatusReason"),
        IN_FLIGHT_UPDATE_NARRATIVE,
        "case {case:?}: a further update states the in-flight narrative again, got {:?}",
        at(&again.body, "/LastUpdateStatusReason")
    );
    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() + NOT_YET_DUE,
    );
    let in_flight_again = get(&router, &connector.arn).await;
    assert_every_pair(case, &in_flight_again.body);
    assert_eq!(
        text(&in_flight_again.body, "/LastUpdateStatusReason"),
        IN_FLIGHT_UPDATE_NARRATIVE,
        "case {case:?}: a read of the second update in flight reports its narrative, not the \
         previous update's outcome, got {:?}",
        at(&in_flight_again.body, "/LastUpdateStatusReason")
    );
    reschedule(
        &engine,
        &connector.name,
        case,
        SystemTime::now() - ALREADY_DUE,
    );
    let settled_again = get(&router, &connector.arn).await;
    assert_eq!(
        text(&settled_again.body, "/LastUpdateStatusReason"),
        SETTLED_UPDATE_NARRATIVE,
        "case {case:?}: the second update settles with the settled narrative, got {:?}",
        at(&settled_again.body, "/LastUpdateStatusReason")
    );
}

/// RULE 3 the other way round: a narrative is reported only where there is a status for it
/// to explain. A connector no update has touched reports neither update member, and a
/// request that changes nothing ("a no-op update returns the current version number")
/// starts no update, so it states no update narrative either.
#[tokio::test]
async fn an_update_narrative_is_reported_only_where_an_update_status_is() {
    let case = "a connector no update has touched";
    let (router, engine) = emulator_with_engine();
    let connector = connector_ready_to_update(&router, &engine, case, "never-updated").await;

    for form in EVERY_FORM {
        let case = format!("{case}, named by {form:?}");
        let reported = get(&router, connector.spelled(form)).await;
        assert_every_pair(&case, &reported.body);
        assert_eq!(
            at(&reported.body, "/LastUpdateStatusReason"),
            Value::Null,
            "case {case:?}: there is no update status to explain, so no narrative is \
             reported, got {:?}",
            at(&reported.body, "/LastUpdateStatusReason")
        );
    }

    // The no-op: the very configuration and role the connector already holds.
    let case = "an update that changes nothing";
    let unchanged = update(
        &router,
        &connector.id,
        json!({
            "Configuration": minimal_configuration(),
            "OperatorRole": OPERATOR_ROLE,
        }),
    )
    .await;
    assert_eq!(
        unchanged.status, 202,
        "case {case:?}: a no-op update is accepted, got {} with {:?}",
        unchanged.status, unchanged.body
    );
    assert_paired(
        case,
        &unchanged.body,
        "/LastUpdateStatus",
        "/LastUpdateStatusReason",
    );
    let reported = get(&router, &connector.name).await;
    assert_every_pair(case, &reported.body);
    assert_eq!(
        at(&reported.body, "/LastUpdateStatus"),
        Value::Null,
        "case {case:?}: a request that changed nothing started no update, got {:?}",
        at(&reported.body, "/LastUpdateStatus")
    );
    assert_eq!(
        at(&reported.body, "/LastUpdateStatusReason"),
        Value::Null,
        "case {case:?}: with no update status there is nothing for a narrative to explain, \
         got {:?}",
        at(&reported.body, "/LastUpdateStatusReason")
    );
}

/// The `State` and `StateReason` the versions listing reports for the version numbered
/// `version`, found by its `Version` member rather than by a position a case assumed.
fn listed_version(case: &str, versions: &Answer, version: i64) -> (String, String) {
    assert_eq!(
        versions.status, 200,
        "case {case:?}: expected HTTP 200 from ListNetworkConnectorVersions, got {} with {:?}",
        versions.status, versions.body
    );
    let entries = versions.body["NetworkConnectorVersions"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: ListNetworkConnectorVersions must return a list, got {:?}",
                versions.body
            )
        });
    let index = entries
        .iter()
        .position(|summary| summary["Version"].as_i64() == Some(version))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the listing must report version {version}, got {:?}",
                versions.body
            )
        });
    state_and_reason(
        case,
        &versions.body,
        &format!("/NetworkConnectorVersions/{index}"),
    )
}

/// RULE 2 across the resources ONE lifecycle event touches: an event has one narrative, so
/// the version an update appends ("each configuration update creates a new immutable
/// version") reports the same `StateReason` for it as the connector that update moved, and
/// not a second, differently worded text.
///
/// The rows span what the rule generalises over: what the update changed (each member
/// `UpdateNetworkConnector` takes, alone and together), which identifier form named the
/// connector, which side of the instant the appended version's own provisioning falls due
/// the listing is taken on, and how many times the listing is re-read (a read neither
/// rewrites nor spends the narrative).
#[tokio::test]
async fn the_version_an_update_appends_reports_that_updates_narrative() {
    for (change, request) in changing_requests() {
        for form in EVERY_FORM {
            let case = format!("{change}, named by {form:?}");
            let (router, engine) = emulator_with_engine();
            let connector =
                connector_ready_to_update(&router, &engine, &case, "narrated-version").await;

            // What version 1 carries before any update: the creation that minted it, which
            // is the narrative the update must NOT leave on the version it appends.
            let before = list_versions(&router, &connector.name, "").await;
            let (_, creation_narrative) = listed_version(&case, &before, 1);

            let updated = update(&router, connector.spelled(form), request.clone()).await;
            assert_eq!(
                updated.status, 202,
                "case {case:?}: the update is accepted, got {} with {:?}",
                updated.status, updated.body
            );

            for (phase, expected_state, due) in [
                ("in flight", "PENDING", SystemTime::now() + NOT_YET_DUE),
                ("settled", "ACTIVE", SystemTime::now() - ALREADY_DUE),
            ] {
                reschedule(&engine, &connector.name, &case, due);
                for look in 1..=2 {
                    let case = format!("{case}, {phase}, look {look}");
                    let versions = list_versions(&router, &connector.name, "").await;
                    let (state, appended_narrative) = listed_version(&case, &versions, 2);
                    assert_eq!(
                        state, expected_state,
                        "case {case:?}: the appended version's own provisioning decides its \
                         state, got {state:?}"
                    );
                    assert_eq!(
                        appended_narrative, UPDATE_INITIATED_NARRATIVE,
                        "case {case:?}: the version the update created is explained by that \
                         update, not by {creation_narrative:?}, got {appended_narrative:?}"
                    );
                    let reported = get(&router, &connector.name).await;
                    assert_eq!(
                        appended_narrative,
                        text(&reported.body, "/StateReason"),
                        "case {case:?}: the update is one event, so the version it created \
                         and the connector it moved report one narrative for it, got \
                         {appended_narrative:?} and {:?}",
                        at(&reported.body, "/StateReason")
                    );
                    // The update was not the event that put version 1 where it is, so that
                    // version keeps the narrative of the event that did.
                    let (_, first_narrative) = listed_version(&case, &versions, 1);
                    assert_eq!(
                        first_narrative, creation_narrative,
                        "case {case:?}: a version the update did not touch keeps its own \
                         narrative, got {first_narrative:?}"
                    );
                }
            }
        }
    }
}

/// RULE 2 over time: the narrative is a property of the RECORD, so every later listing
/// reports the one stored for that version — through further updates, through the
/// version-scoped delete of another version ("the connector and its remaining versions are
/// unaffected") and through the connector's own move to `DELETING`, which restates the
/// connector's narrative and leaves each version's alone.
#[tokio::test]
async fn a_versions_narrative_is_reported_unchanged_by_later_activity() {
    let case = "later activity on the connector";
    let (router, engine) = emulator_with_engine();
    let connector = connector_ready_to_update(&router, &engine, case, "narrated-history").await;
    let before = list_versions(&router, &connector.name, "").await;
    let (_, creation_narrative) = listed_version(case, &before, 1);

    // Three updates, each a configuration the connector does not already hold, so each
    // appends a version of its own: versions 2, 3 and 4.
    for subnet in [
        "subnet-0fedcba9876543210",
        "subnet-00112233445566778",
        "subnet-0a1b2c3d4e5f60718",
    ] {
        let answered = update(
            &router,
            &connector.name,
            json!({ "Configuration": vpc_egress(&[subnet], &[], None, None) }),
        )
        .await;
        assert_eq!(
            answered.status, 202,
            "case {case:?}: the update to {subnet:?} is accepted, got {} with {:?}",
            answered.status, answered.body
        );
    }
    reschedule(&engine, &connector.name, case, SystemTime::now() - ALREADY_DUE);

    // Every version an update appended is explained by an update; the one the create minted
    // still by the creation.
    let updated_versions = [2, 3, 4];
    let listed = list_versions(&router, &connector.name, "").await;
    for version in updated_versions {
        let case = format!("{case}, version {version} after three updates");
        let (_, narrative) = listed_version(&case, &listed, version);
        assert_eq!(
            narrative, UPDATE_INITIATED_NARRATIVE,
            "case {case:?}: each version an update created reports that update, got \
             {narrative:?}"
        );
    }
    let (_, first_narrative) = listed_version(case, &listed, 1);
    assert_eq!(
        first_narrative, creation_narrative,
        "case {case:?}: the version the create minted still reports the creation, got \
         {first_narrative:?}"
    );

    // A version-scoped delete of two other versions is unrelated activity: what is left is
    // reported exactly as it was stored.
    for removed in [1, 2] {
        let answered = delete(&router, &format!("{}:{removed}", connector.arn)).await;
        assert_eq!(
            answered.status, 202,
            "case {case:?}: deleting version {removed} is accepted, got {} with {:?}",
            answered.status, answered.body
        );
        let remaining = list_versions(&router, &connector.name, "").await;
        for version in updated_versions.iter().filter(|left| **left > removed) {
            let case = format!("{case}, version {version} after version {removed} was deleted");
            let (_, narrative) = listed_version(&case, &remaining, *version);
            assert_eq!(
                narrative, UPDATE_INITIATED_NARRATIVE,
                "case {case:?}: the narrative stored for a version survives a delete of \
                 another version, got {narrative:?}"
            );
        }
    }

    // And the connector's own transition: it restates the CONNECTOR's narrative, while each
    // remaining version keeps the one the event that created it stated.
    let deleted = delete(&router, &connector.name).await;
    assert_eq!(
        deleted.status, 202,
        "case {case:?}: deleting the connector is accepted, got {} with {:?}",
        deleted.status, deleted.body
    );
    // The record is there for as long as the clock owes its removal, which the case holds
    // open rather than racing.
    reschedule(&engine, &connector.name, case, SystemTime::now() + NOT_YET_DUE);
    let reported = get(&router, &connector.name).await;
    let (connector_state, connector_narrative) =
        state_and_reason(case, &reported.body, "");
    assert_eq!(
        connector_state, "DELETING",
        "case {case:?}: the delete moved the connector, got {connector_state:?}"
    );
    assert_ne!(
        connector_narrative, UPDATE_INITIATED_NARRATIVE,
        "case {case:?}: the connector's latest event is its deletion, so its narrative \
         states that instead, got {connector_narrative:?}"
    );
    let remaining = list_versions(&router, &connector.name, "").await;
    for version in [3, 4] {
        let case = format!("{case}, version {version} while the connector is DELETING");
        let (_, narrative) = listed_version(&case, &remaining, version);
        assert_eq!(
            narrative, UPDATE_INITIATED_NARRATIVE,
            "case {case:?}: the connector's own transition does not rewrite a version's \
             narrative, got {narrative:?}"
        );
    }
}
