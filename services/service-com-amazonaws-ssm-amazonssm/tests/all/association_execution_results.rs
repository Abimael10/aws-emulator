// SPDX-License-Identifier: CC0-1.0

//! What ONE execution of a State Manager association reports, end to end, and WHEN a version
//! applied to an association adds an execution of its own:
//! `CreateAssociation`/`UpdateAssociation`/`StartAssociationsOnce` dispatching the runs and
//! `DescribeAssociationExecutions` reporting them.
//!
//! RULE: an execution reports the result of the run it is, over the resources that run reached
//! — its status, the same result as the detail of that status, and the count of those resources
//! by it — so a run that reached no resource is over at the instant it was dispatched
//! (`Success`, `Success`, the empty aggregate `{}` and no date of a last execution, because the
//! document was executed nowhere), and a run that reached resources is in progress until the
//! span it takes has passed and reports its own count afterwards; and the run a version applied
//! to an association ALREADY THERE asks for is State Manager's own, so the association's
//! executions grow when the clock makes that dispatch due
//! (`associations::ASSOCIATION_DISPATCH_DELAY`) and a read taken before then reports the
//! executions the association already had, each with the version IT ran.
//!
//! Every case goes through the front door a client uses, over the dispatch
//! [`crate::state_manager_associations`] publishes, so this suite shares one calling convention
//! and one reading of the store with the create, update and execution suites.
//!
//! The node inventory is seeded through the service's own state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_managed_node`, the one node store every
//! dispatch resolves against), because a node enters it only by REGISTERING and no public API
//! of this service registers one.
//!
//! Every instant a case needs to have passed is DRIVEN through the store — the one place the
//! due instants are read from — so nothing here sleeps, waits on the wall clock or rests on how
//! fast the host runs.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over: how
//! many resources the run reached, which targeting form selected them, and whether the read
//! comes before or after the dispatch the clock owes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    associations, ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{
    call, created, dispatch_owed_runs, service, AGENT, NODE, OTHER_NODE, REGION,
};

/// A node id of the form the modelled `InstanceId` pattern admits that no node is registered
/// under, so a target naming it selects no resource at all.
const UNREGISTERED: &str = "i-0dead9999999999de";

/// Register one node of [`REGION`], the way its own registration would — long enough ago that
/// nothing about the registration is still settling.
fn register(engine: &Arc<engine::InMemoryEngine>, instance_id: &str) {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(instance_id, RESOURCE_TYPE_EC2_INSTANCE, at);
    record.properties.ping_status = Some("Online".to_string());
    record.tags = BTreeMap::new();
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(REGION, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// A router with this service mounted and two registered nodes of [`REGION`], so a row can span
/// how many resources one run reaches.
fn service_with_nodes() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = service();
    register(&engine, NODE);
    register(&engine, OTHER_NODE);
    (router, engine)
}

/// The `AssociationExecutions` `DescribeAssociationExecutions` reports for `association_id`, in
/// the order it reported them.
async fn executions(router: &MultiServiceRouter, case: &str, association_id: &str) -> Vec<Value> {
    let (status, parsed) = call(
        router,
        "DescribeAssociationExecutions",
        &json!({"AssociationId": association_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAssociationExecutions, got {status} with \
         {parsed}"
    );
    parsed["AssociationExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: a report carries an array: {parsed}"))
}

/// The one execution such a report must carry.
async fn only_execution(router: &MultiServiceRouter, case: &str, association_id: &str) -> Value {
    let reported = executions(router, case, association_id).await;
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected the one run the association has had, got {reported:?}"
    );
    reported[0].clone()
}

/// The `AssociationId` a create reported.
fn association_id(description: &Value) -> String {
    description["AssociationId"]
        .as_str()
        .unwrap_or_else(|| panic!("a create reports an AssociationId: {description}"))
        .to_string()
}

/// Age every run `association_id` has had past the span a run takes to finish, so the result the
/// clock owes each of them is the one a read reports.
fn finish_the_runs(engine: &Arc<engine::InMemoryEngine>, association_id: &str) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let record = store
        .association_mut(REGION, association_id)
        .expect("the create persisted the association");
    for run in &mut record.executions {
        run.started_at -= associations::ASSOCIATION_EXECUTION_DELAY + Duration::from_secs(1);
    }
}

/// One association's targeting and the resources its run reaches.
struct Row {
    case: &'static str,
    /// The targeting the create carries, beside the document it applies.
    created_with: Value,
    /// How many registered nodes that targeting selects.
    reaches: usize,
}

/// One row per targeting form, spanning the count of resources one run reaches: none, one and
/// more than one.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a node id target naming no registered node",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [UNREGISTERED]}]}),
            reaches: 0,
        },
        Row {
            case: "an association with no targeting at all",
            created_with: json!({}),
            reaches: 0,
        },
        Row {
            case: "a node id target naming one registered node",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
            reaches: 1,
        },
        Row {
            case: "the node named the deprecated way",
            created_with: json!({"InstanceId": OTHER_NODE}),
            reaches: 1,
        },
        Row {
            case: "a node id target naming both registered nodes",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [NODE, OTHER_NODE]}]}),
            reaches: 2,
        },
        Row {
            case: "a wildcard target selecting every node of the Region",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": ["*"]}]}),
            reaches: 2,
        },
    ]
}

/// RULE: an execution reports the result of its run over the resources it reached — the status,
/// the same result as that status's detail, and the count of those resources by it — and a run
/// that reached NO resource is over at the instant it was dispatched, with the empty count and
/// no date of a last execution.
///
/// A run that DID reach resources takes the span a run takes ([`finish_the_runs`]) and nothing
/// here waits for it, so its in-flight reading is asserted only while the clock has not reached
/// the due instant the store holds — the one case in which that answer is settled. A run over no
/// resource is over whatever the clock says, so that reading is asserted outright.
#[tokio::test]
async fn an_execution_reports_its_result_over_the_resources_it_reached() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = service_with_nodes();
        let mut body = json!({"Name": AGENT});
        merge(&mut body, &row.created_with);
        let id = association_id(&created(&router, case, &body).await);

        let due = run_due(&engine, &id);
        let dispatched = only_execution(&router, case, &id).await;
        assert_eq!(
            dispatched["AssociationVersion"]
                .as_str()
                .unwrap_or_default(),
            "1",
            "case {case:?}: expected the version in force at the dispatch, got {dispatched}"
        );
        if row.reaches == 0 {
            // Nothing to apply the document on: the execution is over at the instant it was
            // dispatched, and it executed the document nowhere to report a date for.
            assert_result(&dispatched, case, "Success", "{}", false);
        } else if SystemTime::now() < due {
            assert_result(
                &dispatched,
                case,
                "Pending",
                &format!("{{Pending={}}}", row.reaches),
                false,
            );
        }

        // Once the clock owes the run's result, every run is over: `Success` over the resources
        // it reached, with the date it finished at where it reached any.
        finish_the_runs(&engine, &id);
        let finished = only_execution(&router, case, &id).await;
        let count = if row.reaches == 0 {
            "{}".to_string()
        } else {
            format!("{{Success={}}}", row.reaches)
        };
        assert_result(&finished, case, "Success", &count, row.reaches > 0);
    }
}

/// Hold `entry` to the result a run reports: that status, the detail of it, the count of the
/// resources it reached by it, and whether it reports the date the document was last executed at
/// — which only a run that executed it somewhere has.
fn assert_result(entry: &Value, case: &str, status: &str, count: &str, executed: bool) {
    let detail = if status == "Success" {
        "Success"
    } else {
        "InProgress"
    };
    assert_eq!(
        (
            entry["Status"].as_str().unwrap_or_default(),
            entry["DetailedStatus"].as_str().unwrap_or_default(),
            entry["ResourceCountByStatus"].as_str().unwrap_or_default(),
            entry["LastExecutionDate"].is_number(),
        ),
        (status, detail, count, executed),
        "case {case:?}: expected the status {status}, its detail {detail}, the count {count} \
         and {} date of a last execution, got {entry}",
        if executed { "a" } else { "no" }
    );
}

/// When the run the association is in is owed its result
/// ([`associations::ASSOCIATION_EXECUTION_DELAY`]), read from the store the dispatch wrote it to.
fn run_due(engine: &Arc<engine::InMemoryEngine>, association_id: &str) -> SystemTime {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let store = state.lock();
    store
        .association(REGION, association_id)
        .and_then(associations::AssociationRecord::last_run)
        .map(associations::AssociationRunRecord::finished_at)
        .expect("the create dispatched a run of the association")
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

/// RULE: the run a version applied to an association ALREADY THERE asks for is State Manager's
/// own: the association's executions grow when the clock makes that dispatch due, so a read
/// taken before then reports the executions the association already had — each with the version
/// IT ran — and the run the dispatch starts reports the version in force when it was dispatched.
#[tokio::test]
async fn the_run_an_update_asks_for_is_dispatched_when_the_clock_makes_it_due() {
    // One row per form of the update, spanning whether it re-targets the association and whether
    // it leaves the next run to the schedule the version it applies carries.
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "an update that changes only the schedule",
            json!({"ScheduleExpression": "rate(12 hours)"}),
            true,
        ),
        (
            "an update that re-targets the association onto the other node",
            json!({"Targets": [{"Key": "InstanceIds", "Values": [OTHER_NODE]}]}),
            true,
        ),
        (
            "an update that leaves the next run to its schedule",
            json!({
                "ScheduleExpression": "cron(0 2 ? * SUN *)",
                "ApplyOnlyAtCronInterval": true,
            }),
            false,
        ),
    ];
    for (case, update_with, owes_a_run) in rows {
        let (router, engine) = service_with_nodes();
        let id = association_id(
            &created(
                &router,
                case,
                &json!({
                    "Name": AGENT,
                    "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
                }),
            )
            .await,
        );
        let created_execution = only_execution(&router, case, &id).await;

        let mut body = json!({"AssociationId": id});
        merge(&mut body, &update_with);
        let (status, applied) = call(&router, "UpdateAssociation", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateAssociation, got {status} with {applied}"
        );

        // The read taken straight after the update: the association has had exactly the runs it
        // had before, the create's among them, reporting the version that run applied.
        let after_update = only_execution(&router, case, &id).await;
        assert_eq!(
            (
                after_update["ExecutionId"].clone(),
                after_update["AssociationVersion"].clone()
            ),
            (
                created_execution["ExecutionId"].clone(),
                json!("1")
            ),
            "case {case:?}: expected the execution the create dispatched, of the version it \
             applied, got {after_update}"
        );

        // Once the clock makes the dispatch due, the run it starts is one more execution, of the
        // version the update applied — and an update that left the next run to the schedule owes
        // no run at all, so the association's executions do not grow.
        assert_eq!(
            dispatch_owed_runs(&engine),
            usize::from(owes_a_run),
            "case {case:?}: expected the update to owe State Manager a run only where the \
             version it applied runs when applied"
        );
        let reported = executions(&router, case, &id).await;
        let versions: Vec<&str> = reported
            .iter()
            .map(|entry| entry["AssociationVersion"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            versions,
            if owes_a_run {
                vec!["2", "1"]
            } else {
                vec!["1"]
            },
            "case {case:?}: expected each execution to report the version in force when it was \
             dispatched, got {reported:?}"
        );
    }
}
