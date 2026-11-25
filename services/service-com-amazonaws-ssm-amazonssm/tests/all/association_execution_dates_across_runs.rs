// SPDX-License-Identifier: CC0-1.0

//! What an association reports for its EXECUTION once a further run has been dispatched, end
//! to end over the front door.
//!
//! RULE: `LastExecutionDate` and `LastSuccessfulExecutionDate` report the most recent run of
//! the association that has FINISHED, over its whole run history — "the date on which the
//! association was last run" and "the last date on which the association was successfully
//! run" (`smithy_query(query="shape",shape="AssociationDescription")`) — so a run dispatched
//! since, by the update that applied a new version or by `StartAssociationsOnce`, leaves
//! those dates where the finished run put them until it finishes itself. The detail of an
//! association whose version in force has just been applied is `Creating`, whichever call
//! applied it, so the version an update appends reports it the way the one a create mints
//! does.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body (`OPERATIONS.md`, "Wire
//! protocol") — over the calling convention [`crate::state_manager_associations`] publishes,
//! and reads the association back through both read paths (`DescribeAssociation` and
//! `ListAssociations`), so the two cannot disagree about what has executed.
//!
//! Nothing here waits for a span to pass: what the clock owes is brought due by AGEING the
//! stored association, the way the suites beside this one do, because what the store holds is
//! what a read reports.
//!
//! The table's rows differ along what the rule generalises over: which call dispatches the
//! further run, whether that call dispatches one at all, how many registered nodes the run
//! that finished reached (none, one, two), how many the run dispatched after it reaches, and
//! the targeting form (node-id targets, a node named the deprecated way, a node id nothing is
//! registered under).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    associations, ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{
    call, created, described, service, AGENT, NODE, OTHER_NODE, REGION,
};

/// A node id of the form the modelled `InstanceId` pattern admits that no node is registered
/// under, so a target naming it selects nothing.
const UNREGISTERED: &str = "i-0dead999";

/// How much further back than a span's own length a case ages the store, so the instant the
/// clock owed is comfortably past rather than exactly due.
const WELL_PAST: Duration = Duration::from_secs(1);

/// How long ago the run a case brings to ITS end finished. Longer than the millisecond this
/// protocol states an instant to (`state::wire_instant`), so the date that run moves the
/// association onto is a different value from the one the run before it reported, without any
/// case resting on how fast the host runs.
const FINISHED_AGO: Duration = Duration::from_millis(100);

/// Register one node of [`REGION`], the way its own registration would — long enough ago that
/// nothing about the registration is still settling.
fn register(engine: &Arc<engine::InMemoryEngine>, instance_id: &str) {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(instance_id, RESOURCE_TYPE_EC2_INSTANCE, at);
    record.properties.platform_type = Some("Linux".to_string());
    record.properties.ping_status = Some("Online".to_string());
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(REGION, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// A router with this service mounted and two registered nodes of [`REGION`], so a case can
/// span how many nodes a run reaches.
fn service_with_nodes() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = service();
    register(&engine, NODE);
    register(&engine, OTHER_NODE);
    (router, engine)
}

/// Move the association's whole history back by `span`: its creation, every version it holds
/// and every run it has had — so everything the clock owed as of `span` ago has come due.
fn age_everything(engine: &Arc<engine::InMemoryEngine>, association_id: &str, span: Duration) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let record = store
        .association_mut(REGION, association_id)
        .expect("the create persisted the association");
    record.created_at -= span;
    for version in &mut record.versions {
        version.created_at -= span;
    }
    for run in &mut record.executions {
        run.started_at -= span;
    }
}

/// Bring the association's create, and the run that create dispatched, to their end.
fn settle_the_create(engine: &Arc<engine::InMemoryEngine>, association_id: &str) {
    let span = associations::ASSOCIATION_CREATION_DELAY.max(associations::ASSOCIATION_EXECUTION_DELAY)
        + WELL_PAST;
    age_everything(engine, association_id, span);
}

/// Bring the association's LATEST run alone to its end, leaving every earlier run and the
/// version in force where they are.
fn finish_latest_run(engine: &Arc<engine::InMemoryEngine>, association_id: &str) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let run = store
        .association_mut(REGION, association_id)
        .and_then(associations::AssociationRecord::last_run_mut)
        .expect("the call that applied the version dispatched a run");
    run.started_at -= associations::ASSOCIATION_EXECUTION_DELAY + FINISHED_AGO;
}

/// `UpdateAssociation` with `body`: the description it answered, for an update that must have
/// succeeded.
async fn updated(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, parsed) = call(router, "UpdateAssociation", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateAssociation, got {status} with {parsed}"
    );
    parsed["AssociationDescription"].clone()
}

/// The entry `ListAssociations` reports for `association_id`.
async fn listed(router: &MultiServiceRouter, case: &str, association_id: &str) -> Value {
    let (status, parsed) = call(
        router,
        "ListAssociations",
        &json!({
            "AssociationFilterList": [{ "key": "AssociationId", "value": association_id }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListAssociations, got {status} with {parsed}"
    );
    parsed["Associations"]
        .as_array()
        .and_then(|associations| associations.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: the listing reports the association: {parsed}"))
}

/// The instant a projection reports its last execution at, as the epoch seconds this protocol
/// states a timestamp in.
fn executed_at(projection: &Value, case: &str, read: &str) -> f64 {
    projection["LastExecutionDate"].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: expected {read} to report a LastExecutionDate: {projection}")
    })
}

/// The count of targets by status a run over `nodes` nodes settles with: one count per node
/// it reached, and the count over no node at all for a run that reached none.
fn count_over(nodes: usize) -> Value {
    if nodes == 0 {
        json!({})
    } else {
        json!({ "Success": nodes })
    }
}

/// One association, the run its create had, and the run a further call dispatches.
struct Row {
    case: &'static str,
    /// The members the create carries, beside the document it applies.
    created_with: Value,
    /// How many registered nodes the create's run reaches.
    reaches: usize,
    /// The members the update carries, beside the `AssociationId`.
    update_with: Value,
    /// Whether that update runs the association again, or leaves its next run to the schedule
    /// the version it applied carries.
    dispatches: bool,
    /// How many registered nodes the update's own run reaches, where it dispatched one.
    reaches_after: usize,
    /// The `Targets` the association reports once the update has been applied.
    targets_after: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an update that re-targets the association onto the other node",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
            reaches: 1,
            update_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [OTHER_NODE]}]}),
            dispatches: true,
            reaches_after: 1,
            targets_after: json!([{"Key": "InstanceIds", "Values": [OTHER_NODE]}]),
        },
        Row {
            case: "an update that changes only the schedule of an association targeting both \
                   nodes",
            created_with: json!({
                "Targets": [{"Key": "InstanceIds", "Values": [NODE, OTHER_NODE]}]
            }),
            reaches: 2,
            update_with: json!({"ScheduleExpression": "rate(12 hours)"}),
            dispatches: true,
            reaches_after: 2,
            targets_after: json!([{"Key": "InstanceIds", "Values": [NODE, OTHER_NODE]}]),
        },
        Row {
            case: "an update of an association whose target selects no registered node",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [UNREGISTERED]}]}),
            reaches: 0,
            update_with: json!({"ScheduleExpression": "cron(0 3 ? * MON *)"}),
            dispatches: true,
            reaches_after: 0,
            targets_after: json!([{"Key": "InstanceIds", "Values": [UNREGISTERED]}]),
        },
        Row {
            case: "an update of an association created for a node the deprecated way, which \
                   adds the node it targets",
            created_with: json!({"InstanceId": NODE}),
            reaches: 1,
            update_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [OTHER_NODE]}]}),
            dispatches: true,
            reaches_after: 2,
            targets_after: json!([{"Key": "InstanceIds", "Values": [OTHER_NODE]}]),
        },
        Row {
            case: "an update that leaves the next run to the schedule, which dispatches \
                   nothing",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
            reaches: 1,
            update_with: json!({
                "ScheduleExpression": "cron(0 2 ? * SUN *)",
                "ApplyOnlyAtCronInterval": true
            }),
            dispatches: false,
            reaches_after: 1,
            targets_after: json!([{"Key": "InstanceIds", "Values": [NODE]}]),
        },
    ]
}

/// RULE: the update's own 200 reports the association in full — the version it resolved, the
/// targeting it applied and the execution the association has ALREADY had — and the run it
/// dispatched does not un-report that execution: the dates stay where the finished run put
/// them until the new run finishes, at which point they move onto it.
#[tokio::test]
async fn an_update_reports_the_execution_the_association_has_already_had() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = service_with_nodes();

        let mut create_body = json!({"Name": AGENT});
        merge(&mut create_body, &row.created_with);
        let created = created(&router, case, &create_body).await;
        let association_id = created["AssociationId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a create reports an AssociationId"))
            .to_string();

        // The create's run, once the clock owes its result: the association's first execution.
        settle_the_create(&engine, &association_id);
        let settled = described(&router, case, &json!({"AssociationId": association_id})).await;
        assert_eq!(
            settled["Overview"]["Status"],
            json!("Success"),
            "case {case:?}: expected the create's run to have finished, got {settled}"
        );
        assert_eq!(
            settled["Overview"]["AssociationStatusAggregatedCount"],
            count_over(row.reaches),
            "case {case:?}: expected the count of the nodes that run reached, got {settled}"
        );
        let ran_at = executed_at(&settled, case, "the settled description");
        assert_eq!(
            settled["LastSuccessfulExecutionDate"].as_f64(),
            Some(ran_at),
            "case {case:?}: a run that succeeded is the last successful one, got {settled}"
        );

        // The UPDATE: its own response reports the association as it now stands, including the
        // execution it has already had.
        let mut update_body = json!({"AssociationId": association_id});
        merge(&mut update_body, &row.update_with);
        let applied = updated(&router, case, &update_body).await;
        assert_eq!(
            applied["AssociationVersion"],
            json!("2"),
            "case {case:?}: expected the update to apply the second version, got {applied}"
        );
        assert_eq!(
            applied["Targets"], row.targets_after,
            "case {case:?}: expected the targeting the update applied, got {applied}"
        );
        assert_eq!(
            applied["DocumentVersion"],
            json!("$DEFAULT"),
            "case {case:?}: expected the document version the update resolved, got {applied}"
        );
        assert_eq!(
            applied["LastExecutionDate"].as_f64(),
            Some(ran_at),
            "case {case:?}: expected the update to report the run that had already finished, \
             got {applied}"
        );
        assert_eq!(
            applied["LastSuccessfulExecutionDate"].as_f64(),
            Some(ran_at),
            "case {case:?}: expected the update to report that run as the last successful one, \
             got {applied}"
        );
        if row.dispatches {
            assert_eq!(
                applied["Overview"]["Status"],
                json!("Pending"),
                "case {case:?}: the run the update dispatched has reported no result yet, got \
                 {applied}"
            );
            assert_eq!(
                applied["Overview"]["DetailedStatus"],
                json!("Creating"),
                "case {case:?}: expected the detail of a version just applied, got {applied}"
            );
        } else {
            assert_eq!(
                applied["Overview"]["Status"],
                json!("Success"),
                "case {case:?}: an update that dispatched nothing keeps the run the \
                 association last had, got {applied}"
            );
            assert_eq!(
                applied["Overview"]["DetailedStatus"],
                json!("Success"),
                "case {case:?}: expected the detail of that finished run, got {applied}"
            );
        }

        // Both read paths report the same execution while the update's run is in flight: a
        // dispatched run un-reports nothing.
        let read = described(&router, case, &json!({"AssociationId": association_id})).await;
        assert_eq!(
            read["LastExecutionDate"].as_f64(),
            Some(ran_at),
            "case {case:?}: expected the read to keep reporting that run, got {read}"
        );
        let entry = listed(&router, case, &association_id).await;
        assert_eq!(
            entry["LastExecutionDate"].as_f64(),
            Some(ran_at),
            "case {case:?}: expected the listing to agree with the read, got {entry}"
        );

        // Once the run the update dispatched has finished, the dates move ONTO it. An update
        // that dispatched nothing has no later run to move them to, so the one execution the
        // association has had stays its last.
        if row.dispatches {
            finish_latest_run(&engine, &association_id);
        }
        let finished = described(&router, case, &json!({"AssociationId": association_id})).await;
        assert_eq!(
            finished["Overview"]["Status"],
            json!("Success"),
            "case {case:?}: expected the run in force to have finished, got {finished}"
        );
        assert_eq!(
            finished["Overview"]["AssociationStatusAggregatedCount"],
            count_over(row.reaches_after),
            "case {case:?}: expected the count of the nodes the run in force reached, got \
             {finished}"
        );
        let finished_at = executed_at(&finished, case, "the description of the finished run");
        if row.dispatches {
            assert!(
                finished_at > ran_at,
                "case {case:?}: expected a later execution than {ran_at}, got {finished_at} in \
                 {finished}"
            );
        } else {
            assert_eq!(
                finished_at, ran_at,
                "case {case:?}: expected the only run the association had, got {finished_at} \
                 in {finished}"
            );
        }
        assert_eq!(
            finished["LastSuccessfulExecutionDate"].as_f64(),
            Some(finished_at),
            "case {case:?}: expected that run to be the last successful one, got {finished}"
        );
        assert_eq!(
            listed(&router, case, &association_id).await["LastExecutionDate"].as_f64(),
            Some(finished_at),
            "case {case:?}: expected the listing to report the same execution as the read"
        );
    }
}

/// RULE, over the OTHER call that dispatches a run: "runs an association immediately and only
/// one time" (`StartAssociationsOnce`) adds an execution, and until that execution finishes
/// the association keeps reporting the one that already did.
#[tokio::test]
async fn starting_an_association_again_keeps_the_execution_it_already_reported() {
    // (case, the targets the create carries, how many registered nodes they select)
    let rows: Vec<(&str, Value, usize)> = vec![
        (
            "targets selecting no registered node",
            json!([{"Key": "InstanceIds", "Values": [UNREGISTERED]}]),
            0,
        ),
        (
            "a target selecting one registered node",
            json!([{"Key": "InstanceIds", "Values": [NODE]}]),
            1,
        ),
        (
            "a target selecting every node of the Region",
            json!([{"Key": "InstanceIds", "Values": ["*"]}]),
            2,
        ),
    ];

    for (case, targets, reaches) in rows {
        let (router, engine) = service_with_nodes();
        let created = created(
            &router,
            case,
            &json!({"Name": AGENT, "Targets": targets}),
        )
        .await;
        let association_id = created["AssociationId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a create reports an AssociationId"))
            .to_string();

        settle_the_create(&engine, &association_id);
        let settled = described(&router, case, &json!({"AssociationId": association_id})).await;
        let ran_at = executed_at(&settled, case, "the settled description");
        assert_eq!(
            settled["Overview"]["AssociationStatusAggregatedCount"],
            count_over(reaches),
            "case {case:?}: expected the count of the nodes the create's run reached, got \
             {settled}"
        );

        let (status, body) = call(
            &router,
            "StartAssociationsOnce",
            &json!({"AssociationIds": [association_id]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from StartAssociationsOnce, got {status} with \
             {body}"
        );

        // The run just dispatched has reported nothing, and the execution that finished is
        // still the association's last.
        let polled = described(&router, case, &json!({"AssociationId": association_id})).await;
        assert_eq!(
            polled["Overview"]["Status"],
            json!("Pending"),
            "case {case:?}: the run just dispatched has reported no result, got {polled}"
        );
        assert_eq!(
            polled["LastExecutionDate"].as_f64(),
            Some(ran_at),
            "case {case:?}: expected the finished execution to be reported still, got {polled}"
        );
        assert_eq!(
            polled["LastSuccessfulExecutionDate"].as_f64(),
            Some(ran_at),
            "case {case:?}: expected it to be the last successful one still, got {polled}"
        );
        assert_eq!(
            listed(&router, case, &association_id).await["LastExecutionDate"].as_f64(),
            Some(ran_at),
            "case {case:?}: expected the listing to agree with the read"
        );

        // And once that run has finished, the dates are its.
        finish_latest_run(&engine, &association_id);
        let finished = described(&router, case, &json!({"AssociationId": association_id})).await;
        let finished_at = executed_at(&finished, case, "the description of the second run");
        assert!(
            finished_at > ran_at,
            "case {case:?}: expected the second run's later date than {ran_at}, got \
             {finished_at} in {finished}"
        );
        assert_eq!(
            finished["Overview"]["AssociationStatusAggregatedCount"],
            count_over(reaches),
            "case {case:?}: expected the second run's count of targets by status, got \
             {finished}"
        );
    }
}

/// Merge the members of `extra` onto the object `body`.
fn merge(body: &mut Value, extra: &Value) {
    let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (member, value) in source {
        target.insert(member.clone(), value.clone());
    }
}
