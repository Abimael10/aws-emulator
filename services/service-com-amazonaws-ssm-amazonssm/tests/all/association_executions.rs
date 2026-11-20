// SPDX-License-Identifier: CC0-1.0

//! The executions of a State Manager association, end to end: `DescribeAssociationExecutions`
//! reporting the runs `CreateAssociation` and `StartAssociationsOnce` dispatched, and
//! `DescribeAssociationExecutionTargets` reporting the managed nodes one of those runs reached.
//!
//! RULE: an association reports one execution per run State Manager has dispatched it in,
//! most recent first, each carrying the id that dispatch minted, the association version it
//! applied, the instant it started, the status the clock has brought that run to and the count
//! of its targets in that status — a run that reached no node reporting no result at all; an
//! execution reports one target per node it was dispatched to, carrying that node's id and the
//! kind the node store holds for it, with the execution's own status; both reads narrow by
//! every filter the request carries, serve pages of at most `MaxResults` resumed by the id of
//! the item the next page starts at, write nothing, and refuse an association id that names
//! none of the Region's associations with `AssociationDoesNotExist` naming the association
//! asked about, and an execution id that names none of that association's executions with
//! `AssociationExecutionDoesNotExist`.
//!
//! Every case goes through the front door a client uses, over the dispatch
//! [`crate::state_manager_associations`] publishes, so this suite shares one calling
//! convention and one reading of the store with the create, listing and status suites.
//!
//! The node inventory is seeded through the service's own state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_managed_node`, the one node store every node
//! read goes through), because a node enters it only by REGISTERING and no public API of this
//! service registers one. Where a case needs a run to have FINISHED, the instant it was
//! dispatched at is moved back through the store past the span a run takes
//! ([`associations::ASSOCIATION_EXECUTION_DELAY`]): the due instant is driven rather than
//! waited for, so nothing here sleeps.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over —
//! how many dispatches an association has had, which targeting form selects which nodes, which
//! node kind a target is, which version a dispatch applied, which filter narrows the report,
//! how big a page is and which shapes a resume token can take — rather than the one report the
//! recorded unit happened to take.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    association_executions, associations, ComAmazonawsSsmAmazonssmState, ManagedNodeRecord,
    RESOURCE_TYPE_EC2_INSTANCE, RESOURCE_TYPE_MANAGED_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{
    call, call_in_region, created, error_code, service, AGENT, NODE, OTHER_NODE, OTHER_REGION,
    REGION,
};

/// A hybrid node of [`REGION`] (`mi-` and 17 characters, as the modelled `InstanceId` pattern
/// spells an on-premises node), so a target spans node KIND as the node store holds it.
const HYBRID: &str = "mi-01234567890123456";

/// A well-formed association id the store holds no association under, to pin that an id that
/// resolves to nothing is refused rather than served as an association with no executions.
const UNKNOWN_ASSOCIATION: &str = "11111111-2222-3333-4444-555555555555";

/// A well-formed execution id no execution of an association carries, for the same reason.
const UNKNOWN_EXECUTION: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";

/// One registered node of `region`: its id and the kind the node store holds for it.
fn register(engine: &Arc<engine::InMemoryEngine>, region: &str, id: &str, resource_type: &str) {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, resource_type, at);
    record.properties.ping_status = Some("Online".to_string());
    record.tags = BTreeMap::new();
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// The inventory every suite below dispatches against: two EC2 nodes and one hybrid node in
/// [`REGION`], spanning node kind.
fn seed_nodes(engine: &Arc<engine::InMemoryEngine>) {
    register(engine, REGION, NODE, RESOURCE_TYPE_EC2_INSTANCE);
    register(engine, REGION, OTHER_NODE, RESOURCE_TYPE_EC2_INSTANCE);
    register(engine, REGION, HYBRID, RESOURCE_TYPE_MANAGED_INSTANCE);
}

/// A create that targets `nodes` by node id with a document whose parameters all carry
/// defaults, so the create needs to supply none.
fn create_targeting(nodes: &[&str]) -> Value {
    json!({"Name": AGENT, "Targets": [{"Key": "InstanceIds", "Values": nodes}]})
}

/// The `AssociationId` a create reported.
fn association_id(description: &Value) -> String {
    description["AssociationId"]
        .as_str()
        .unwrap_or("<unset>")
        .to_string()
}

/// `DescribeAssociationExecutions` with `body`, signed for [`REGION`].
async fn describe_executions(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "DescribeAssociationExecutions", body).await
}

/// `DescribeAssociationExecutionTargets` with `body`, signed for [`REGION`].
async fn describe_targets(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "DescribeAssociationExecutionTargets", body).await
}

/// The whole response of an execution report that must have succeeded.
async fn executions_response(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, parsed) = describe_executions(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAssociationExecutions, got {status} \
         with {parsed:?}"
    );
    parsed
}

/// The `AssociationExecutions` such a report carries, in the order it reported them.
async fn executions(router: &MultiServiceRouter, case: &str, body: &Value) -> Vec<Value> {
    executions_response(router, case, body).await["AssociationExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The whole response of a targets report that must have succeeded.
async fn targets_response(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, parsed) = describe_targets(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAssociationExecutionTargets, got \
         {status} with {parsed:?}"
    );
    parsed
}

/// The `AssociationExecutionTargets` such a report carries, in the order it reported them.
async fn targets(router: &MultiServiceRouter, case: &str, body: &Value) -> Vec<Value> {
    targets_response(router, case, body).await["AssociationExecutionTargets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The refusal message of a failure body.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// A string member of a reported entry.
fn text(entry: &Value, member: &str) -> String {
    entry[member].as_str().unwrap_or("<unset>").to_string()
}

/// The `ExecutionId`s a report carries, in the order it reported them.
fn execution_ids(reported: &[Value]) -> Vec<String> {
    reported
        .iter()
        .map(|entry| text(entry, "ExecutionId"))
        .collect()
}

/// The `ResourceId`s a targets report carries, in the order it reported them.
fn resource_ids(reported: &[Value]) -> Vec<String> {
    reported
        .iter()
        .map(|entry| text(entry, "ResourceId"))
        .collect()
}

/// Dispatch `association_id` again, the way `StartAssociationsOnce` does.
async fn start_once(router: &MultiServiceRouter, case: &str, association_id: &str) {
    let (status, parsed) = call(
        router,
        "StartAssociationsOnce",
        &json!({"AssociationIds": [association_id]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from StartAssociationsOnce, got {status} with \
         {parsed:?}"
    );
}

/// The runs of `association_id` as the store holds them, oldest first:
/// `(execution id, the instant it was dispatched, the nodes it reached)`.
fn stored_runs(
    engine: &Arc<engine::InMemoryEngine>,
    association_id: &str,
) -> Vec<(String, SystemTime, Vec<String>)> {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let store = state.lock();
    store
        .association(REGION, association_id)
        .expect("the create persisted the association")
        .executions
        .iter()
        .map(|run| (run.execution_id.clone(), run.started_at, run.nodes.clone()))
        .collect()
}

/// Move every run of `association_id` back past the span a run takes to finish
/// ([`associations::ASSOCIATION_EXECUTION_DELAY`]), so the clock owes each one's completion.
///
/// The relative order of the runs is kept, and nothing waits for the span: the due instant is
/// driven through the store, which is the one place it is read from.
fn finish_runs(engine: &Arc<engine::InMemoryEngine>, association_id: &str) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let record = store
        .association_mut(REGION, association_id)
        .expect("the create persisted the association");
    for run in &mut record.executions {
        run.started_at -= associations::ASSOCIATION_EXECUTION_DELAY + Duration::from_secs(1);
    }
}

/// Move the run at `index` of `association_id` further back by `span`, so two runs of one
/// association fall in distinct seconds whatever instants the dispatches read the clock at.
fn backdate_run(
    engine: &Arc<engine::InMemoryEngine>,
    association_id: &str,
    index: usize,
    span: Duration,
) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let record = store
        .association_mut(REGION, association_id)
        .expect("the create persisted the association");
    record.executions[index].started_at -= span;
}

/// The status an execution whose run has FINISHED reports, and the date it finished at — the
/// members an execution and one of its targets both carry.
fn assert_finished_status(entry: &Value, case: &str) {
    assert_eq!(
        entry["Status"],
        json!("Success"),
        "case {case:?}: a finished run reports Success, got {entry:?}"
    );
    assert_eq!(
        entry["DetailedStatus"],
        json!("Success"),
        "case {case:?}: a finished run's detail is its result, got {entry:?}"
    );
    assert!(
        entry["LastExecutionDate"].as_f64().is_some(),
        "case {case:?}: a finished run reports the date it finished at, got {entry:?}"
    );
}

/// The whole of what a FINISHED execution reports: that status, plus the count of the nodes it
/// reached in it — the member only an execution carries, asserted unconditionally.
fn assert_finished_execution(entry: &Value, nodes: usize, case: &str) {
    assert_finished_status(entry, case);
    assert_eq!(
        entry["ResourceCountByStatus"],
        json!(format!("{{Success={nodes}}}")),
        "case {case:?}: expected the {nodes} nodes it reached counted as Success, got {entry:?}"
    );
}

/// The status a just-dispatched run reports, for an execution or one of its targets.
///
/// Its completion is owed [`associations::ASSOCIATION_EXECUTION_DELAY`] after the dispatch and
/// nothing here waits for it, so the in-progress reading is asserted only while the clock has
/// not reached the due instant the store holds — the one case in which that answer is settled —
/// and a run the clock has since brought to its end is held to the finished reading instead.
fn assert_dispatched_status(entry: &Value, due: SystemTime, case: &str) -> bool {
    if SystemTime::now() >= due {
        assert_finished_status(entry, case);
        return false;
    }
    assert_eq!(
        entry["Status"],
        json!("Pending"),
        "case {case:?}: a run no node has reported a result for is Pending, got {entry:?}"
    );
    assert_eq!(
        entry["DetailedStatus"],
        json!("InProgress"),
        "case {case:?}: a dispatched run is being worked through, got {entry:?}"
    );
    assert!(
        entry["LastExecutionDate"].is_null(),
        "case {case:?}: a run that has not finished reports no date for it, got {entry:?}"
    );
    true
}

/// The whole of what a just-dispatched EXECUTION reports: that status, plus the count of the
/// nodes it reached in it, asserted unconditionally for whichever reading held.
fn assert_dispatched_execution(entry: &Value, due: SystemTime, nodes: usize, case: &str) {
    let status = if assert_dispatched_status(entry, due, case) {
        "Pending"
    } else {
        "Success"
    };
    assert_eq!(
        entry["ResourceCountByStatus"],
        json!(format!("{{{status}={nodes}}}")),
        "case {case:?}: expected the {nodes} nodes it reached counted as {status}, got {entry:?}"
    );
}

/// One association's dispatch history: which nodes its targeting selects and how many times it
/// is dispatched.
struct History {
    case: &'static str,
    create: Value,
    /// How many times `StartAssociationsOnce` dispatches it after the create.
    starts: usize,
    /// The nodes every one of those runs reaches, ascending — the order a run holds them in.
    reaches: &'static [&'static str],
}

/// One row per dispatch history, along the axes "how many runs has this association had" and
/// "which nodes does each of them reach".
fn histories() -> Vec<History> {
    vec![
        History {
            case: "one node, dispatched by the create alone",
            create: create_targeting(&[NODE]),
            starts: 0,
            reaches: &[NODE],
        },
        History {
            case: "one node, dispatched again once",
            create: create_targeting(&[NODE]),
            starts: 1,
            reaches: &[NODE],
        },
        History {
            case: "two nodes, dispatched again twice",
            create: create_targeting(&[OTHER_NODE, NODE]),
            starts: 2,
            reaches: &[NODE, OTHER_NODE],
        },
        History {
            case: "every node of the Region, dispatched by the create alone",
            create: json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": ["*"]}],
            }),
            starts: 0,
            reaches: &[NODE, OTHER_NODE, HYBRID],
        },
        History {
            case: "the hybrid node named through the deprecated InstanceId member",
            create: json!({"Name": AGENT, "InstanceId": HYBRID}),
            starts: 1,
            reaches: &[HYBRID],
        },
    ]
}

/// RULE: every dispatch is an execution of the association, reported most recent first with
/// the id that dispatch minted, and each one resolves to exactly the nodes it was dispatched
/// to.
#[tokio::test]
async fn every_dispatch_is_reported_as_an_execution() {
    for row in histories() {
        let (router, engine) = service();
        seed_nodes(&engine);
        let created = created(&router, row.case, &row.create).await;
        let id = association_id(&created);
        for _ in 0..row.starts {
            start_once(&router, row.case, &id).await;
        }

        let reported = executions(&router, row.case, &json!({"AssociationId": id})).await;
        assert_eq!(
            reported.len(),
            row.starts + 1,
            "case {:?}: expected one execution per dispatch, got {reported:?}",
            row.case
        );

        let held = stored_runs(&engine, &id);
        let mut newest_first = held.clone();
        newest_first.reverse();
        assert_eq!(
            execution_ids(&reported),
            newest_first
                .iter()
                .map(|(execution_id, _, _)| execution_id.clone())
                .collect::<Vec<String>>(),
            "case {:?}: expected the dispatched runs most recent first, got {reported:?}",
            row.case
        );

        let mut dispatch_instants: Vec<f64> = Vec::new();
        for entry in &reported {
            assert_eq!(
                text(entry, "AssociationId"),
                id,
                "case {:?}: an execution belongs to the association it ran, got {entry:?}",
                row.case
            );
            assert_eq!(
                text(entry, "AssociationVersion"),
                "1",
                "case {:?}: expected the version in force at the dispatch, got {entry:?}",
                row.case
            );
            assert!(
                associations::is_association_id(&text(entry, "ExecutionId")),
                "case {:?}: expected an execution id of the modelled form, got {entry:?}",
                row.case
            );
            let created_time = entry["CreatedTime"].as_f64().unwrap_or(f64::NAN);
            assert!(
                created_time.is_finite(),
                "case {:?}: an execution reports the instant it started, got {entry:?}",
                row.case
            );
            dispatch_instants.push(created_time);

            // ROUND TRIP: the execution the report hands out resolves to the nodes the
            // dispatch persisted.
            let reached = targets(
                &router,
                row.case,
                &json!({"AssociationId": id, "ExecutionId": text(entry, "ExecutionId")}),
            )
            .await;
            assert_eq!(
                resource_ids(&reached),
                row.reaches
                    .iter()
                    .map(|node| (*node).to_string())
                    .collect::<Vec<String>>(),
                "case {:?}: expected the nodes the run was dispatched to, got {reached:?}",
                row.case
            );
        }
        assert!(
            dispatch_instants.windows(2).all(|pair| pair[0] >= pair[1]),
            "case {:?}: expected the executions ordered by when they started, got \
             {dispatch_instants:?}",
            row.case
        );
    }
}

/// RULE: the association version an execution reports is the one in force when it was
/// dispatched, so an earlier execution keeps reporting the version it applied after an update
/// has moved the association on — and an update, which dispatches a run of its own, is one
/// more execution rather than a rewrite of the first.
#[tokio::test]
async fn an_execution_reports_the_version_the_dispatch_applied() {
    let case = "a dispatch after an update applies the new version";
    let (router, engine) = service();
    seed_nodes(&engine);
    let created = created(&router, case, &create_targeting(&[NODE])).await;
    let id = association_id(&created);

    let (status, parsed) = call(
        &router,
        "UpdateAssociation",
        &json!({
            "AssociationId": id,
            "AssociationName": "renamed",
            "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateAssociation, got {status} with {parsed:?}"
    );
    start_once(&router, case, &id).await;

    let reported = executions(&router, case, &json!({"AssociationId": id})).await;
    assert_eq!(
        reported
            .iter()
            .map(|entry| text(entry, "AssociationVersion"))
            .collect::<Vec<String>>(),
        // Newest first: the explicit dispatch and the update's own, both of the version the
        // update minted, then the create's, which keeps reporting the version it applied.
        vec!["2".to_string(), "2".to_string(), "1".to_string()],
        "case {case:?}: expected each execution to report the version in force when it was \
         dispatched, got {reported:?}"
    );
}

/// RULE: an execution's status, the count of its targets by that status and the date it
/// reports are derived from the instant it was dispatched at — `Pending`/`InProgress` until the
/// run is owed to have finished and `Success` over the nodes it reached afterwards.
#[tokio::test]
async fn an_execution_reports_the_status_its_run_has_reached() {
    let rows: Vec<(&str, Value, &[&str])> = vec![
        ("one node", create_targeting(&[NODE]), &[NODE]),
        (
            "two nodes",
            create_targeting(&[NODE, OTHER_NODE]),
            &[NODE, OTHER_NODE],
        ),
        (
            "every node of the Region",
            json!({"Name": AGENT, "Targets": [{"Key": "InstanceIds", "Values": ["*"]}]}),
            &[NODE, OTHER_NODE, HYBRID],
        ),
    ];
    for (case, create, reaches) in rows {
        let (router, engine) = service();
        seed_nodes(&engine);
        let created = created(&router, case, &create).await;
        let id = association_id(&created);

        let dispatched = executions(&router, case, &json!({"AssociationId": id})).await;
        let due = stored_runs(&engine, &id)
            .first()
            .map(|(_, started_at, _)| *started_at + associations::ASSOCIATION_EXECUTION_DELAY)
            .expect("the create dispatched a run");
        assert_eq!(
            dispatched.len(),
            1,
            "case {case:?}: the create dispatched one run, got {dispatched:?}"
        );
        assert_dispatched_execution(&dispatched[0], due, reaches.len(), case);
        // The targets of that run report the same reading of its status.
        let reached = targets(
            &router,
            case,
            &json!({
                "AssociationId": id,
                "ExecutionId": text(&dispatched[0], "ExecutionId"),
            }),
        )
        .await;
        for target in &reached {
            assert_dispatched_status(target, due, case);
        }

        finish_runs(&engine, &id);
        let finished = executions(&router, case, &json!({"AssociationId": id})).await;
        assert_finished_execution(&finished[0], reaches.len(), case);
        let reached = targets(
            &router,
            case,
            &json!({
                "AssociationId": id,
                "ExecutionId": text(&finished[0], "ExecutionId"),
            }),
        )
        .await;
        assert_eq!(
            resource_ids(&reached),
            reaches
                .iter()
                .map(|node| (*node).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the nodes the run reached, got {reached:?}"
        );
        for target in &reached {
            assert_finished_status(target, case);
        }
    }
}

/// RULE: a target reports the kind the node store holds for the node it names, so an execution
/// of one association over nodes of two kinds reports each node's own kind.
#[tokio::test]
async fn a_target_reports_the_kind_the_node_store_holds_for_it() {
    let case = "a run over an EC2 node and a hybrid node";
    let (router, engine) = service();
    seed_nodes(&engine);
    let created = created(&router, case, &create_targeting(&[NODE, HYBRID])).await;
    let id = association_id(&created);
    let execution_id = stored_runs(&engine, &id)
        .first()
        .map(|(execution_id, _, _)| execution_id.clone())
        .expect("the create dispatched a run");

    let reached = targets(
        &router,
        case,
        &json!({"AssociationId": id, "ExecutionId": execution_id}),
    )
    .await;
    let kinds: Vec<(String, String)> = reached
        .iter()
        .map(|target| (text(target, "ResourceId"), text(target, "ResourceType")))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (NODE.to_string(), RESOURCE_TYPE_EC2_INSTANCE.to_string()),
            (HYBRID.to_string(), RESOURCE_TYPE_MANAGED_INSTANCE.to_string()),
        ],
        "case {case:?}: expected each node's own registered kind, got {reached:?}"
    );
    for target in &reached {
        assert_eq!(
            text(target, "ExecutionId"),
            execution_id,
            "case {case:?}: a target belongs to the execution it ran in, got {target:?}"
        );
        assert_eq!(
            text(target, "AssociationVersion"),
            "1",
            "case {case:?}: a target reports the version its execution applied, got {target:?}"
        );
        assert!(
            target["OutputSource"].is_null(),
            "case {case:?}: nothing has uploaded this execution's details, got {target:?}"
        );
    }
}

/// RULE: the executions reported are the ones every filter the request carries admits, where
/// the id and the status compare whole values and the dispatch instant is ordered.
#[tokio::test]
async fn the_executions_reported_are_the_ones_every_filter_admits() {
    let case = "two finished runs of one association";
    let (router, engine) = service();
    seed_nodes(&engine);
    let created = created(&router, case, &create_targeting(&[NODE])).await;
    let id = association_id(&created);
    start_once(&router, case, &id).await;
    finish_runs(&engine, &id);
    // The two dispatches fall in whatever second the host clock was at, so the first run is
    // moved further back: the rows below order instants, and an ordering only has something
    // to say about two runs a filter value can fall between.
    backdate_run(&engine, &id, 0, Duration::from_secs(3_600));

    let held = stored_runs(&engine, &id);
    assert_eq!(held.len(), 2, "two dispatches are two runs, got {held:?}");
    let (first_id, first_at, _) = held[0].clone();
    let (second_id, second_at, _) = held[1].clone();
    let seconds = |at: SystemTime| {
        at.duration_since(SystemTime::UNIX_EPOCH)
            .expect("an instant after the epoch")
            .as_secs()
            .to_string()
    };
    // A value BETWEEN the two dispatches, so each ordering operator admits exactly one of
    // them whatever the host clock reads.
    let between = seconds(first_at + Duration::from_secs(60));

    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "the id of the first execution",
            json!([{"Key": "ExecutionId", "Value": first_id, "Type": "EQUAL"}]),
            vec![first_id.clone()],
        ),
        (
            "the id of the second execution",
            json!([{"Key": "ExecutionId", "Value": second_id, "Type": "EQUAL"}]),
            vec![second_id.clone()],
        ),
        (
            "an execution id neither of them carries",
            json!([{"Key": "ExecutionId", "Value": UNKNOWN_EXECUTION, "Type": "EQUAL"}]),
            vec![],
        ),
        (
            "the status both of them have reached",
            json!([{"Key": "Status", "Value": "Success", "Type": "EQUAL"}]),
            vec![second_id.clone(), first_id.clone()],
        ),
        (
            "a status neither of them has",
            json!([{"Key": "Status", "Value": "Pending", "Type": "EQUAL"}]),
            vec![],
        ),
        (
            "dispatched before an instant between the two",
            json!([{"Key": "CreatedTime", "Value": between, "Type": "LESS_THAN"}]),
            vec![first_id.clone()],
        ),
        (
            "dispatched after that same instant",
            json!([{"Key": "CreatedTime", "Value": between, "Type": "GREATER_THAN"}]),
            vec![second_id.clone()],
        ),
        (
            "dispatched in the second the first run started in",
            json!([{"Key": "CreatedTime", "Value": seconds(first_at), "Type": "EQUAL"}]),
            vec![first_id.clone()],
        ),
        (
            "dispatched in the second the second run started in",
            json!([{"Key": "CreatedTime", "Value": seconds(second_at), "Type": "EQUAL"}]),
            vec![second_id.clone()],
        ),
        (
            "two filters that cannot both hold",
            json!([
                {"Key": "ExecutionId", "Value": first_id, "Type": "EQUAL"},
                {"Key": "Status", "Value": "Pending", "Type": "EQUAL"},
            ]),
            vec![],
        ),
        (
            "two filters that both hold of one execution",
            json!([
                {"Key": "ExecutionId", "Value": second_id, "Type": "EQUAL"},
                {"Key": "Status", "Value": "Success", "Type": "EQUAL"},
            ]),
            vec![second_id.clone()],
        ),
        (
            "an instant no run was dispatched after",
            json!([{"Key": "CreatedTime", "Value": seconds(second_at + Duration::from_secs(60)), "Type": "GREATER_THAN"}]),
            vec![],
        ),
    ];
    for (row, filters, wanted) in rows {
        let reported = executions(
            &router,
            row,
            &json!({"AssociationId": id, "Filters": filters}),
        )
        .await;
        assert_eq!(
            execution_ids(&reported),
            wanted,
            "case {row:?}: expected exactly the executions the filter admits, got {reported:?}"
        );
    }
}

/// RULE: the targets reported are the ones every filter the request carries admits, over the
/// node, its kind and the status of the execution.
#[tokio::test]
async fn the_targets_reported_are_the_ones_every_filter_admits() {
    let case = "a finished run over an EC2 node and a hybrid node";
    let (router, engine) = service();
    seed_nodes(&engine);
    let created = created(&router, case, &create_targeting(&[NODE, HYBRID])).await;
    let id = association_id(&created);
    finish_runs(&engine, &id);
    let execution_id = stored_runs(&engine, &id)
        .first()
        .map(|(execution_id, _, _)| execution_id.clone())
        .expect("the create dispatched a run");

    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "the EC2 node by id",
            json!([{"Key": "ResourceId", "Value": NODE}]),
            vec![NODE.to_string()],
        ),
        (
            "the hybrid node by id",
            json!([{"Key": "ResourceId", "Value": HYBRID}]),
            vec![HYBRID.to_string()],
        ),
        (
            "a node the run never reached",
            json!([{"Key": "ResourceId", "Value": OTHER_NODE}]),
            vec![],
        ),
        (
            "the EC2 node kind",
            json!([{"Key": "ResourceType", "Value": RESOURCE_TYPE_EC2_INSTANCE}]),
            vec![NODE.to_string()],
        ),
        (
            "the hybrid node kind",
            json!([{"Key": "ResourceType", "Value": RESOURCE_TYPE_MANAGED_INSTANCE}]),
            vec![HYBRID.to_string()],
        ),
        (
            "the status the finished run reached",
            json!([{"Key": "Status", "Value": "Success"}]),
            vec![NODE.to_string(), HYBRID.to_string()],
        ),
        (
            "a status the run has not reached",
            json!([{"Key": "Status", "Value": "Pending"}]),
            vec![],
        ),
        (
            "a kind and a node that are the same target",
            json!([
                {"Key": "ResourceType", "Value": RESOURCE_TYPE_MANAGED_INSTANCE},
                {"Key": "ResourceId", "Value": HYBRID},
            ]),
            vec![HYBRID.to_string()],
        ),
        (
            "a kind and a node that are different targets",
            json!([
                {"Key": "ResourceType", "Value": RESOURCE_TYPE_MANAGED_INSTANCE},
                {"Key": "ResourceId", "Value": NODE},
            ]),
            vec![],
        ),
    ];
    for (row, filters, wanted) in rows {
        let reported = targets(
            &router,
            row,
            &json!({
                "AssociationId": id,
                "ExecutionId": execution_id,
                "Filters": filters,
            }),
        )
        .await;
        assert_eq!(
            resource_ids(&reported),
            wanted,
            "case {row:?}: expected exactly the targets the filter admits, got {reported:?}"
        );
    }
}

/// RULE: a filter whose key does not accept the operator it carries, or whose value is no
/// instant for the key that orders instants, narrows the report by nothing, so it is refused
/// and nothing is reported as if it had been applied.
#[tokio::test]
async fn a_filter_the_report_cannot_apply_is_refused() {
    let case = "a filter the report cannot apply";
    let (router, engine) = service();
    seed_nodes(&engine);
    let created = created(&router, case, &create_targeting(&[NODE])).await;
    let id = association_id(&created);

    // The operators each key accepts are the ones the request's own documentation lists:
    // "ExecutionId (EQUAL) Status (EQUAL) CreatedTime (EQUAL, GREATER_THAN, LESS_THAN)".
    let refused: Vec<(&str, Value)> = vec![
        (
            "an id compared with an ordering operator",
            json!([{"Key": "ExecutionId", "Value": UNKNOWN_EXECUTION, "Type": "GREATER_THAN"}]),
        ),
        (
            "a status compared with an ordering operator",
            json!([{"Key": "Status", "Value": "Success", "Type": "LESS_THAN"}]),
        ),
        (
            "an instant key carrying a value that names no instant",
            json!([{"Key": "CreatedTime", "Value": "yesterday", "Type": "GREATER_THAN"}]),
        ),
    ];
    for (row, filters) in refused {
        let (status, parsed) = describe_executions(
            &router,
            &json!({"AssociationId": id, "Filters": filters}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400, got {status} with {parsed:?}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {row:?}: expected the refusal of a request this report cannot apply, got \
             {parsed:?}"
        );
    }

    let accepted: Vec<(&str, Value)> = vec![
        (
            "an id compared for equality",
            json!([{"Key": "ExecutionId", "Value": UNKNOWN_EXECUTION, "Type": "EQUAL"}]),
        ),
        (
            "a status compared for equality",
            json!([{"Key": "Status", "Value": "Success", "Type": "EQUAL"}]),
        ),
        (
            "an instant compared for equality",
            json!([{"Key": "CreatedTime", "Value": "2024-01-01T00:00:00Z", "Type": "EQUAL"}]),
        ),
        (
            "an instant ordered before",
            json!([{"Key": "CreatedTime", "Value": "2024-01-01T00:00:00Z", "Type": "LESS_THAN"}]),
        ),
        (
            "an instant ordered after, as epoch seconds",
            json!([{"Key": "CreatedTime", "Value": "1704067200", "Type": "GREATER_THAN"}]),
        ),
    ];
    for (row, filters) in accepted {
        let (status, parsed) = describe_executions(
            &router,
            &json!({"AssociationId": id, "Filters": filters}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {row:?}: expected the filter to be applied, got {status} with {parsed:?}"
        );
    }
}

/// RULE: both reports serve at most `MaxResults` items, handing back the id of the item the
/// next page starts at, so walking the pages reports every item once and in the one order.
#[tokio::test]
async fn the_reports_are_served_in_pages_of_at_most_max_results() {
    let case = "three runs over three nodes";
    let (router, engine) = service();
    seed_nodes(&engine);
    let created = created(
        &router,
        case,
        &json!({"Name": AGENT, "Targets": [{"Key": "InstanceIds", "Values": ["*"]}]}),
    )
    .await;
    let id = association_id(&created);
    start_once(&router, case, &id).await;
    start_once(&router, case, &id).await;
    finish_runs(&engine, &id);

    let whole = executions(&router, case, &json!({"AssociationId": id})).await;
    let all_executions = execution_ids(&whole);
    assert_eq!(
        all_executions.len(),
        3,
        "case {case:?}: three dispatches are three executions, got {whole:?}"
    );
    for page_size in [1usize, 2, 3, 50] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..=all_executions.len() {
            let mut body = json!({"AssociationId": id, "MaxResults": page_size});
            if let Some(token) = &token {
                body["NextToken"] = json!(token);
            }
            let response = executions_response(&router, case, &body).await;
            let page = response["AssociationExecutions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page of {page_size} holds no more, got {page:?}"
            );
            walked.extend(execution_ids(&page));
            token = response["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {case:?}: walking pages of {page_size} ends with no token, got {token:?}"
        );
        assert_eq!(
            walked, all_executions,
            "case {case:?}: pages of {page_size} report every execution once, in the one \
             order, got {walked:?}"
        );
    }

    let execution_id = all_executions[0].clone();
    let whole = targets(
        &router,
        case,
        &json!({"AssociationId": id, "ExecutionId": execution_id}),
    )
    .await;
    let all_targets = resource_ids(&whole);
    assert_eq!(
        all_targets.len(),
        3,
        "case {case:?}: the run reached three nodes, got {whole:?}"
    );
    for page_size in [1usize, 2, 3, 50] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..=all_targets.len() {
            let mut body = json!({
                "AssociationId": id,
                "ExecutionId": execution_id,
                "MaxResults": page_size,
            });
            if let Some(token) = &token {
                body["NextToken"] = json!(token);
            }
            let response = targets_response(&router, case, &body).await;
            let page = response["AssociationExecutionTargets"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page of {page_size} holds no more, got {page:?}"
            );
            walked.extend(resource_ids(&page));
            token = response["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, all_targets,
            "case {case:?}: pages of {page_size} report every target once, in the one order, \
             got {walked:?}"
        );
    }
}

/// RULE: a resume token naming nothing the report holds was not one this service issued, so it
/// is refused with `InvalidNextToken` rather than served as an empty page.
#[tokio::test]
async fn a_resume_token_naming_nothing_the_report_holds_is_refused() {
    let case = "a token naming nothing the report holds";
    let (router, engine) = service();
    seed_nodes(&engine);
    let created = created(&router, case, &create_targeting(&[NODE])).await;
    let id = association_id(&created);
    let execution_id = stored_runs(&engine, &id)
        .first()
        .map(|(execution_id, _, _)| execution_id.clone())
        .expect("the create dispatched a run");

    for token in [UNKNOWN_EXECUTION, "not-a-token", "i-0000000000000000"] {
        let (status, parsed) = describe_executions(
            &router,
            &json!({"AssociationId": id, "NextToken": token}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {token:?}: expected HTTP 400 from the execution report, got {status} with \
             {parsed:?}"
        );
        assert_eq!(
            error_code(&parsed),
            "InvalidNextToken",
            "case {token:?}: expected the invalid-token refusal, got {parsed:?}"
        );

        let (status, parsed) = describe_targets(
            &router,
            &json!({
                "AssociationId": id,
                "ExecutionId": execution_id,
                "NextToken": token,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {token:?}: expected HTTP 400 from the targets report, got {status} with \
             {parsed:?}"
        );
        assert_eq!(
            error_code(&parsed),
            "InvalidNextToken",
            "case {token:?}: expected the invalid-token refusal, got {parsed:?}"
        );
    }

    // An empty token is a request for the first page, the way every other read of this
    // service serves one.
    let reported = executions(
        &router,
        case,
        &json!({"AssociationId": id, "NextToken": ""}),
    )
    .await;
    assert_eq!(
        execution_ids(&reported),
        vec![execution_id],
        "case {case:?}: an empty token starts at the first execution, got {reported:?}"
    );
}

/// RULE: an association id that names none of the Region's associations is
/// `AssociationDoesNotExist` — whose message, on every operation that resolves an association
/// by the id its request carries, NAMES the association asked about — and an execution id that
/// names none of the association's executions is `AssociationExecutionDoesNotExist`.
#[tokio::test]
async fn an_id_that_resolves_to_nothing_is_refused() {
    let case = "an id that resolves to nothing";
    let (router, engine) = service();
    seed_nodes(&engine);
    let created = created(&router, case, &create_targeting(&[NODE])).await;
    let id = association_id(&created);
    let execution_id = stored_runs(&engine, &id)
        .first()
        .map(|(execution_id, _, _)| execution_id.clone())
        .expect("the create dispatched a run");

    // One row per operation that resolves an association by the id its request carries —
    // the reads of it and the writes that dispatch or change it: every one of them refuses
    // an id that resolves to nothing the same way, and names it.
    let addressed_by_id = |asked_about: &str| -> Vec<(&'static str, Value)> {
        vec![
            (
                "DescribeAssociationExecutions",
                json!({"AssociationId": asked_about}),
            ),
            (
                "DescribeAssociationExecutionTargets",
                json!({"AssociationId": asked_about, "ExecutionId": execution_id}),
            ),
            ("DescribeAssociation", json!({"AssociationId": asked_about})),
            (
                "StartAssociationsOnce",
                json!({"AssociationIds": [asked_about]}),
            ),
            ("UpdateAssociation", json!({"AssociationId": asked_about})),
            ("DeleteAssociation", json!({"AssociationId": asked_about})),
        ]
    };
    // The two ways an id resolves to no association of the endpoint it is asked through: one
    // nothing was ever created under, and the id of an association of ANOTHER Region.
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "an association no create minted",
            REGION,
            UNKNOWN_ASSOCIATION.to_string(),
        ),
        ("another Region's view of an id", OTHER_REGION, id.clone()),
    ];
    for (row, region, asked_about) in rows {
        for (operation, body) in addressed_by_id(&asked_about) {
            let (status, parsed) = call_in_region(&router, region, operation, &body).await;
            assert_eq!(
                (status, error_code(&parsed).as_str()),
                (400, "AssociationDoesNotExist"),
                "case {row:?} on {operation}: expected the association refusal, got {status} \
                 with {parsed:?}"
            );
            assert_eq!(
                error_message(&parsed),
                associations::association_does_not_exist(&asked_about),
                "case {row:?} on {operation}: expected the refusal to name the association \
                 asked about, got {parsed:?}"
            );
        }
    }

    let (status, parsed) = describe_targets(
        &router,
        &json!({"AssociationId": id, "ExecutionId": UNKNOWN_EXECUTION}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for an execution the association never had, got \
         {status} with {parsed:?}"
    );
    assert_eq!(
        error_code(&parsed),
        "AssociationExecutionDoesNotExist",
        "case {case:?}: expected the execution refusal, got {parsed:?}"
    );
    assert_eq!(
        error_message(&parsed),
        association_executions::ASSOCIATION_EXECUTION_DOES_NOT_EXIST,
        "case {case:?}: expected the wording of the execution refusal, got {parsed:?}"
    );
}

/// RULE: a dispatch that reached no registered node is an execution of the association all the
/// same — State Manager dispatched it — reporting no target, no detail and no count of targets
/// by status, because a result is reported by the nodes a run reached.
#[tokio::test]
async fn a_dispatch_that_reached_no_node_reports_no_result() {
    let rows: Vec<(&str, Value)> = vec![
        (
            "a node id target naming no registered node",
            create_targeting(&["i-0999999999999999"]),
        ),
        (
            "a resource-group target no node of this account is in",
            json!({
                "Name": AGENT,
                "Targets": [{"Key": "resource-groups:Name", "Values": ["web-tier"]}],
            }),
        ),
    ];
    for (case, create) in rows {
        let (router, engine) = service();
        seed_nodes(&engine);
        let created = created(&router, case, &create).await;
        let id = association_id(&created);
        start_once(&router, case, &id).await;

        let reported = executions(&router, case, &json!({"AssociationId": id})).await;
        assert_eq!(
            reported.len(),
            2,
            "case {case:?}: both dispatches are executions of the association, got {reported:?}"
        );
        for entry in &reported {
            assert_eq!(
                entry["Status"],
                json!("Pending"),
                "case {case:?}: a run with no target has no result to report, got {entry:?}"
            );
            assert!(
                entry["DetailedStatus"].is_null()
                    && entry["LastExecutionDate"].is_null(),
                "case {case:?}: a detail over no resource is none, got {entry:?}"
            );
            assert_eq!(
                entry["ResourceCountByStatus"],
                json!("{}"),
                "case {case:?}: a count over no resource is the empty aggregate, got {entry:?}"
            );
            // The execution resolves, and reports the nothing it reached.
            let reached = targets(
                &router,
                case,
                &json!({
                    "AssociationId": id,
                    "ExecutionId": text(entry, "ExecutionId"),
                }),
            )
            .await;
            assert!(
                reached.is_empty(),
                "case {case:?}: the run reached no node, got {reached:?}"
            );
        }
    }
}

/// RULE: an association holds at most [`associations::MAX_ASSOCIATION_EXECUTIONS`] executions —
/// the store's own ceiling — so a dispatch past it drops the association's OLDEST execution,
/// which then resolves to none, while the dispatch itself still succeeds.
#[tokio::test]
async fn an_association_holds_at_most_the_executions_the_store_keeps() {
    let case = "more dispatches than the store keeps executions of one association";
    let (router, engine) = service();
    seed_nodes(&engine);
    let created = created(&router, case, &create_targeting(&[NODE])).await;
    let id = association_id(&created);
    let oldest = stored_runs(&engine, &id)
        .first()
        .map(|(execution_id, _, _)| execution_id.clone())
        .expect("the create dispatched a run");

    // The create dispatched one, so two more than the ceiling have been dispatched in all.
    for _ in 0..associations::MAX_ASSOCIATION_EXECUTIONS + 1 {
        start_once(&router, case, &id).await;
    }

    let held = stored_runs(&engine, &id);
    assert_eq!(
        held.len(),
        associations::MAX_ASSOCIATION_EXECUTIONS,
        "case {case:?}: expected the association to hold the ceiling's worth of executions, \
         got {}",
        held.len()
    );
    assert!(
        held.iter().all(|(execution_id, _, _)| *execution_id != oldest),
        "case {case:?}: expected the oldest execution to have been dropped, got {held:?}"
    );

    let (status, parsed) = describe_targets(
        &router,
        &json!({"AssociationId": id, "ExecutionId": oldest}),
    )
    .await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "AssociationExecutionDoesNotExist"),
        "case {case:?}: an execution the store no longer holds resolves to none, got {status} \
         with {parsed:?}"
    );

    let newest = held
        .last()
        .map(|(execution_id, _, _)| execution_id.clone())
        .expect("the association holds executions");
    let reached = targets(
        &router,
        case,
        &json!({"AssociationId": id, "ExecutionId": newest}),
    )
    .await;
    assert_eq!(
        resource_ids(&reached),
        vec![NODE.to_string()],
        "case {case:?}: the newest execution still reports the node it reached, got {reached:?}"
    );
}

/// RULE: both reports are reads — they leave the runs the store holds exactly as the dispatches
/// wrote them, so a later caller sees what the first one saw.
#[tokio::test]
async fn reading_the_executions_changes_nothing_a_later_caller_sees() {
    let case = "two reads of one association's executions";
    let (router, engine) = service();
    seed_nodes(&engine);
    let created = created(&router, case, &create_targeting(&[NODE, OTHER_NODE])).await;
    let id = association_id(&created);
    start_once(&router, case, &id).await;
    finish_runs(&engine, &id);

    let before = stored_runs(&engine, &id);
    let first = executions(&router, case, &json!({"AssociationId": id})).await;
    let execution_id = text(&first[0], "ExecutionId");
    let first_targets = targets(
        &router,
        case,
        &json!({"AssociationId": id, "ExecutionId": execution_id}),
    )
    .await;
    let second = executions(&router, case, &json!({"AssociationId": id})).await;
    let second_targets = targets(
        &router,
        case,
        &json!({"AssociationId": id, "ExecutionId": execution_id}),
    )
    .await;
    let after = stored_runs(&engine, &id);

    assert_eq!(
        before, after,
        "case {case:?}: a read starts, finishes or re-dates no run, got {after:?}"
    );
    assert_eq!(
        first, second,
        "case {case:?}: two reads of finished runs report the same executions, got {second:?}"
    );
    assert_eq!(
        first_targets, second_targets,
        "case {case:?}: two reads of one execution report the same targets, got \
         {second_targets:?}"
    );
}
