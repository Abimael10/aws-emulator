// SPDX-License-Identifier: CC0-1.0

//! Running a State Manager association on the call that applies a version of it, end to end:
//! `CreateAssociation` and `UpdateAssociation` dispatching the association to the managed
//! nodes its configuration selects, and `DescribeAssociation` and
//! `DescribeInstanceAssociationsStatus` reporting the run that reached them.
//!
//! RULE: the call that applies a version of an association — the create that mints it or the
//! update that replaces it — immediately runs the association against the managed nodes the
//! targets of THAT version select, unless the applied version asks to be run only on its
//! schedule (`ApplyOnlyAtCronInterval`), in which case the call dispatches nothing and the
//! association keeps whatever it last ran.
//!
//! Every case goes through the front door a client uses, over the dispatch
//! [`crate::state_manager_associations`] publishes, so this suite shares one calling
//! convention and one reading of the store with the create and update suites.
//!
//! The node inventory is seeded through the service's own state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_managed_node`, the one node store every
//! dispatch resolves against), because a node enters it only by REGISTERING and no public API
//! of this service registers one.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over —
//! which targeting form the applied version uses, how many nodes it selects, whether it names
//! the node the deprecated way, and whether it leaves the next run to the schedule — rather
//! than the single update a recording happened to make.

use std::collections::BTreeMap;
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

/// The tag key the seeded nodes carry a value for, so a tag target selects a subset of the
/// inventory rather than all of it.
const TAG_KEY: &str = "Env";

/// The tag value [`NODE`] carries.
const PROD: &str = "prod";

/// The tag value [`OTHER_NODE`] carries.
const DEV: &str = "dev";

/// Register one node of [`REGION`] carrying `tag_value`, the way its own registration would.
fn register(engine: &Arc<engine::InMemoryEngine>, instance_id: &str, tag_value: &str) {
    // Registered long enough ago that nothing about the registration is still settling.
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(instance_id, RESOURCE_TYPE_EC2_INSTANCE, at);
    record.tags = [(TAG_KEY.to_string(), tag_value.to_string())]
        .into_iter()
        .collect::<BTreeMap<String, String>>();
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(REGION, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// A router with this service mounted and two registered nodes of [`REGION`], spanning tag
/// value so a tag target selects one of them or both.
fn service_with_nodes() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = service();
    register(&engine, NODE, PROD);
    register(&engine, OTHER_NODE, DEV);
    (router, engine)
}

/// `UpdateAssociation` with `body`, signed for [`REGION`]; the description it answered, for an
/// update that must have succeeded.
async fn updated(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, parsed) = call(router, "UpdateAssociation", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateAssociation, got {status} with {parsed:?}"
    );
    parsed["AssociationDescription"].clone()
}

/// The managed nodes the association's latest run was dispatched to, read from the one store
/// the dispatch wrote it to.
fn dispatched_nodes(engine: &Arc<engine::InMemoryEngine>, association_id: &str) -> Vec<String> {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let store = state.lock();
    store
        .associations
        .values()
        .find(|held| held.association_id == association_id)
        .and_then(|held| held.last_run())
        .map(|run| run.nodes.clone())
        .unwrap_or_default()
}

/// Age every run the store holds past the span a run takes to finish, so the result the clock
/// owes is the one a read reports. The store is aged rather than the clock: what the store
/// holds is what a read reports, so a run dispatched that long ago is this run.
fn age_runs(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    for held in store.associations.values_mut() {
        if let Some(run) = held.last_run_mut() {
            run.started_at -= associations::ASSOCIATION_EXECUTION_DELAY + Duration::from_secs(1);
        }
    }
}

/// The `Status` the node `instance_id` reports for the association `association_id`, or `None`
/// when that node's report names no such association.
async fn status_on_node(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    association_id: &str,
) -> Option<String> {
    let (status, parsed) = call(
        router,
        "DescribeInstanceAssociationsStatus",
        &json!({"InstanceId": instance_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeInstanceAssociationsStatus, got {status} \
         with {parsed:?}"
    );
    parsed["InstanceAssociationStatusInfos"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|info| info["AssociationId"].as_str() == Some(association_id))
        .map(|info| info["Status"].as_str().unwrap_or("<unset>").to_string())
}

/// One update of one association, and the nodes the update's own run reaches.
struct Row {
    case: &'static str,
    /// The members the create carries, beside the document it applies.
    created_with: Value,
    /// The members the update carries, beside the `AssociationId`.
    update_with: Value,
    /// The managed nodes the run the update dispatched was sent to, in the store's ascending
    /// node-id order — empty for a configuration that selects none.
    reaches: Vec<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an update that re-targets the association from one node to another",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
            update_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [OTHER_NODE]}]}),
            reaches: vec![OTHER_NODE],
        },
        Row {
            case: "an update that widens the targets to every node carrying either tag value",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
            update_with: json!({"Targets": [{"Key": "tag:Env", "Values": [PROD, DEV]}]}),
            reaches: vec![NODE, OTHER_NODE],
        },
        Row {
            case: "an update that names a tag value one node carries",
            created_with: json!({"Targets": [{"Key": "tag:Env", "Values": [PROD, DEV]}]}),
            update_with: json!({"Targets": [{"Key": "tag:Env", "Values": [DEV]}]}),
            reaches: vec![OTHER_NODE],
        },
        Row {
            case: "an update restating no target at all, which selects no node",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
            update_with: json!({"ScheduleExpression": "rate(2 hours)"}),
            reaches: vec![],
        },
        Row {
            case: "an update of an association created for a node the deprecated way, which \
                   keeps that node and adds the one the update targets",
            created_with: json!({"InstanceId": NODE}),
            update_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [OTHER_NODE]}]}),
            reaches: vec![NODE, OTHER_NODE],
        },
        Row {
            case: "an update that asks for the schedule to be the only trigger, which \
                   dispatches nothing and keeps what the association last ran",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
            update_with: json!({
                "Targets": [{"Key": "InstanceIds", "Values": [OTHER_NODE]}],
                "ScheduleExpression": "cron(0 2 ? * SUN *)",
                "ApplyOnlyAtCronInterval": true
            }),
            reaches: vec![NODE],
        },
    ]
}

/// RULE: an update runs the association against the nodes the version it applied selects, and
/// the association then reports that run — unless the applied version leaves the next run to
/// its schedule, in which case the run the association keeps is the one it last had.
#[tokio::test]
async fn an_update_runs_the_association_against_the_nodes_it_applied() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = service_with_nodes();
        let mut create_body = json!({"Name": AGENT});
        merge(&mut create_body, &row.created_with);
        let created = created(&router, case, &create_body).await;
        let association_id = created["AssociationId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let mut update_body = json!({"AssociationId": association_id});
        merge(&mut update_body, &row.update_with);
        let applied = updated(&router, case, &update_body).await;
        assert_eq!(
            applied["AssociationVersion"], json!("2"),
            "case {case:?}: expected the update to apply the second version, got {applied:?}"
        );

        let reached = dispatched_nodes(&engine, &association_id);
        assert_eq!(
            as_refs(&reached),
            row.reaches,
            "case {case:?}: expected the run to reach {:?}, got {reached:?}",
            row.reaches
        );

        // Once the clock owes the run's result, the association reports it: a count of the
        // nodes the run reached, and the instant it finished. A configuration that reached no
        // node settles the same way, over no count of targets by status, because a count of
        // targets by status is a count over executions and there were none.
        age_runs(&engine);
        let read = described(&router, case, &json!({"AssociationId": association_id})).await;
        if row.reaches.is_empty() {
            assert_eq!(
                read["Overview"]["Status"], json!("Success"),
                "case {case:?}: expected a run that reached no node to settle Success, got \
                 {read:?}"
            );
            assert!(
                read["Overview"]["AssociationStatusAggregatedCount"].is_null(),
                "case {case:?}: expected no count of targets by status for a run that reached \
                 no node, got {read:?}"
            );
        } else {
            assert_eq!(
                read["Overview"]["Status"], json!("Success"),
                "case {case:?}: expected the finished run to be the association's result, got \
                 {read:?}"
            );
            assert_eq!(
                read["Overview"]["AssociationStatusAggregatedCount"],
                json!({"Success": row.reaches.len()}),
                "case {case:?}: expected one success per node the run reached, got {read:?}"
            );
            assert!(
                read["LastExecutionDate"].is_number()
                    && read["LastSuccessfulExecutionDate"].is_number(),
                "case {case:?}: expected the finished run's dates, got {read:?}"
            );
        }

        // The node's own report agrees about which nodes the run reached: every node it
        // reached reports the association as having succeeded, and a node it did not reach
        // reports no such run.
        for node in [NODE, OTHER_NODE] {
            let reported = status_on_node(&router, case, node, &association_id).await;
            if row.reaches.contains(&node) {
                assert_eq!(
                    reported.as_deref(),
                    Some("Success"),
                    "case {case:?}: expected {node} to report the finished run, got {reported:?}"
                );
            } else {
                assert!(
                    reported.is_none_or(|status| status != "Success"),
                    "case {case:?}: expected {node} to report no finished run of this \
                     association"
                );
            }
        }
    }
}

/// RULE: the same flag decides the create's own run — a create runs the association straight
/// away unless it asks for the schedule to be the only trigger, and an update then applies the
/// flag the version IT supplied rather than the one before it.
#[tokio::test]
async fn the_schedule_only_flag_decides_the_run_of_whichever_call_applied_it() {
    // (case, the members the applying call carries, whether that call dispatches a run)
    let rows: Vec<(&str, Value, bool)> = vec![
        ("no flag at all, which is the modelled default", json!({}), true),
        (
            "the flag at the modelled default, stated",
            json!({"ApplyOnlyAtCronInterval": false}),
            true,
        ),
        (
            "the flag set beside a cron schedule",
            json!({
                "ScheduleExpression": "cron(0 2 ? * SUN *)",
                "ApplyOnlyAtCronInterval": true
            }),
            false,
        ),
    ];
    for (case, members, runs) in rows {
        // The CREATE applying it: the association runs on the create, or not at all.
        let (router, engine) = service_with_nodes();
        let mut create_body = json!({
            "Name": AGENT,
            "Targets": [{"Key": "InstanceIds", "Values": [NODE]}]
        });
        merge(&mut create_body, &members);
        let on_create = created(&router, case, &create_body).await;
        let association_id = on_create["AssociationId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let reached = dispatched_nodes(&engine, &association_id);
        let expected: Vec<&str> = if runs { vec![NODE] } else { Vec::new() };
        assert_eq!(
            as_refs(&reached),
            expected,
            "case {case:?}: expected a create carrying it to reach {expected:?}, got {reached:?}"
        );

        // The UPDATE applying it, over an association whose create left the flag alone: the
        // run is the applied version's, so a restated flag suppresses the update's run and the
        // association keeps the create's.
        let (router, engine) = service_with_nodes();
        let on_update = created(
            &router,
            case,
            &json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [NODE]}]
            }),
        )
        .await;
        let association_id = on_update["AssociationId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mut update_body = json!({
            "AssociationId": association_id,
            "Targets": [{"Key": "tag:Env", "Values": [DEV]}]
        });
        merge(&mut update_body, &members);
        updated(&router, case, &update_body).await;
        let reached = dispatched_nodes(&engine, &association_id);
        let expected: Vec<&str> = if runs { vec![OTHER_NODE] } else { vec![NODE] };
        assert_eq!(
            as_refs(&reached),
            expected,
            "case {case:?}: expected an update carrying it to leave the run on {expected:?}, \
             got {reached:?}"
        );
    }
}

/// The node ids as slices, so a row's expectation reads as written.
fn as_refs(nodes: &[String]) -> Vec<&str> {
    nodes.iter().map(String::as_str).collect()
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
