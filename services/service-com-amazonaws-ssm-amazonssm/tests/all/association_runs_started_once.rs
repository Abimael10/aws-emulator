// SPDX-License-Identifier: CC0-1.0

//! `StartAssociationsOnce` end to end: "Runs an association immediately and only one time".
//!
//! RULE: one call dispatches a FRESH run of every association the Region the request was
//! signed for holds under the ids it names — to the managed nodes that association's targets
//! select at the instant of the call, whose result the clock makes due
//! (`state::associations::ASSOCIATION_EXECUTION_DELAY`) rather than this call — and an id that
//! Region holds no association under is `AssociationDoesNotExist` with NOTHING dispatched for
//! the request at all, so a run starts for every association a request names or for none.
//!
//! Every case goes through the front door a client uses, over the dispatch
//! [`crate::state_manager_associations`] publishes, so this suite shares one calling
//! convention with the create, listing and update suites, and reads every run back through
//! `DescribeAssociation`.
//!
//! The node inventory is seeded through the service's own state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_managed_node`, the one node store a
//! dispatch resolves its targets against), because a node enters it only by REGISTERING and
//! no public API of this service registers one.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over —
//! how many ids one call names, which targeting form selects the nodes, how many nodes that
//! is, whether any node is selected at all, where in the request an unresolvable id sits and
//! which Region holds the association — rather than the single call a recorded unit happened
//! to make.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{
    call_in_region, created, error_code, service, AGENT, NODE, OTHER_NODE, OTHER_REGION, REGION,
    SHELL,
};

/// How long a run is looked for before the case gives up. Generous on purpose: the result of
/// a run is owed by the clock (`state::associations::ASSOCIATION_EXECUTION_DELAY`), and a case
/// looks AGAIN for it rather than bounding how fast this host gets there.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a case waits between two looks.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// The tag key the seeded nodes carry a value for, so a tag target selects more than one node.
const TAG_KEY: &str = "Env";

/// The tag value both nodes of [`REGION`] carry.
const PROD: &str = "prod";

/// A node id the modelled `InstanceId` pattern admits that no Region holds a node under, so
/// a target naming it selects none.
const UNREGISTERED: &str = "i-0dead999";

/// A node registered only in [`OTHER_REGION`], so no target taken through [`REGION`] resolves
/// it.
const ELSEWHERE: &str = "i-0eeee222";

/// Well-formed association ids (the modelled `AssociationId` pattern) nothing was ever minted
/// under, so neither resolves to an association of any Region.
const UNMINTED: [&str; 2] = [
    "00000000-0000-4000-8000-000000000000",
    "ffffffff-ffff-4fff-bfff-ffffffffffff",
];

/// How `AssociationDoesNotExist` is worded where the request carries the id it could not
/// resolve: "Association <AssociationId> does not exist." — fixed text around that id, so a
/// call naming several associations says which of them is gone.
fn does_not_exist(association_id: &str) -> String {
    format!("Association {association_id} does not exist.")
}

/// Register a node of `region` the way its own registration would, carrying `tag_value` under
/// [`TAG_KEY`] so a tag target selects it.
fn register(engine: &Arc<engine::InMemoryEngine>, region: &str, id: &str, tag_value: &str) {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, RESOURCE_TYPE_EC2_INSTANCE, at);
    record.properties.ping_status = Some("Online".to_string());
    record.tags = [(TAG_KEY.to_string(), tag_value.to_string())]
        .into_iter()
        .collect::<BTreeMap<String, String>>();
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// The inventory every case below dispatches against: two nodes of [`REGION`] carrying the
/// same tag value, and one node registered only in [`OTHER_REGION`].
fn seed_nodes(engine: &Arc<engine::InMemoryEngine>) {
    register(engine, REGION, NODE, PROD);
    register(engine, REGION, OTHER_NODE, PROD);
    register(engine, OTHER_REGION, ELSEWHERE, PROD);
}

/// `StartAssociationsOnce` over `association_ids`, signed for `region`.
async fn start(
    router: &MultiServiceRouter,
    region: &str,
    association_ids: &[String],
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "StartAssociationsOnce",
        &json!({ "AssociationIds": association_ids }),
    )
    .await
}

/// The `AssociationDescription` `region` reports for `association_id`.
async fn description(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    association_id: &str,
) -> Value {
    let (status, parsed) = call_in_region(
        router,
        region,
        "DescribeAssociation",
        &json!({ "AssociationId": association_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAssociation, got {status} with {parsed}"
    );
    parsed["AssociationDescription"].clone()
}

/// The instant a description reports its last execution at, or `None` for one reporting none.
fn executed_at(description: &Value) -> Option<f64> {
    description["LastExecutionDate"].as_f64()
}

/// Look AGAIN until `association_id` reports a finished run later than `after`, or
/// [`DEADLINE`] passes: a run finishes on the clock and nothing drives it but the clock.
async fn association_when_run_finished(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    association_id: &str,
    after: f64,
) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        let held = description(router, case, region, association_id).await;
        if held["Overview"]["Status"] == json!("Success")
            && executed_at(&held).is_some_and(|ran_at| ran_at > after)
        {
            return held;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected a run finishing later than {after}, got {held}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// An association whose run reached NO node settles like any other: State Manager has
/// nothing to dispatch the document to, so the execution is over as soon as the targets have
/// been evaluated — `Success`, over no count of targets by status, because a count of targets
/// by status is a count over executions and there were none.
///
/// Polled like every other settling read here ([`association_when_run_finished`]), so the
/// case is about WHETHER it settles rather than about the instant it is read at.
async fn assert_settled_over_no_node(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    association_id: &str,
) {
    let until = Instant::now() + DEADLINE;
    let held = loop {
        let held = description(router, case, region, association_id).await;
        if held["Overview"]["Status"] == json!("Success") {
            break held;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected an association that reached no node to settle Success, \
             got {held}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    };
    assert_eq!(
        held["Overview"]["DetailedStatus"],
        json!("Success"),
        "case {case:?}: expected the detail of a finished run, got {held}"
    );
    assert!(
        held["Overview"]["AssociationStatusAggregatedCount"].is_null(),
        "case {case:?}: expected no count of targets by status, got {held}"
    );
}

/// RULE: a call runs each association it names again, on the managed nodes that association's
/// targets select — the result owed by the clock, over the count of nodes the run reached —
/// and leaves every association it does not name reporting the run it already had.
#[tokio::test]
async fn a_request_runs_every_association_it_names_on_the_nodes_its_targets_select() {
    /// The calls the table's rows are named by, each spanning one id count within the
    /// modelled 1..10 (`AssociationIdList`).
    const CALLS: &[(&str, usize)] = &[
        ("one association named by a call", 0),
        ("two associations named by one call", 1),
        ("three associations named by one call", 2),
    ];

    let (router, engine) = service();
    seed_nodes(&engine);

    // (case, the create, how many registered nodes its targets select, which call names it)
    let rows: Vec<(&str, Value, usize, Option<usize>)> = vec![
        (
            "one node an InstanceIds target names",
            json!({"Name": AGENT, "Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
            1,
            Some(0),
        ),
        (
            "every node a tag target selects",
            json!({
                "Name": SHELL,
                "Targets": [{"Key": format!("tag:{TAG_KEY}"), "Values": [PROD]}],
                "Parameters": {"commands": ["echo hi"]},
            }),
            2,
            Some(1),
        ),
        (
            "the node the deprecated InstanceId member names",
            json!({"Name": AGENT, "InstanceId": NODE}),
            1,
            Some(1),
        ),
        (
            "two nodes one target names",
            json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [NODE, OTHER_NODE]}],
            }),
            2,
            Some(2),
        ),
        (
            "a target naming a node no Region holds",
            json!({"Name": AGENT, "Targets": [{"Key": "InstanceIds", "Values": [UNREGISTERED]}]}),
            0,
            Some(2),
        ),
        (
            "a target naming a node another Region holds",
            json!({"Name": AGENT, "Targets": [{"Key": "InstanceIds", "Values": [ELSEWHERE]}]}),
            0,
            Some(2),
        ),
        (
            "an association no call names",
            json!({"Name": AGENT, "Targets": [{"Key": "InstanceIds", "Values": [OTHER_NODE]}]}),
            1,
            None,
        ),
    ];

    // The creates: each one dispatches its own first run, so a later call's run is the one
    // that comes AFTER the instant recorded here.
    let mut ids: Vec<String> = Vec::new();
    for (case, request, _, _) in &rows {
        let created = created(&router, case, request).await;
        assert_eq!(
            created["Overview"]["Status"],
            json!("Pending"),
            "case {case:?}: a create answers before its run has finished, got {created}"
        );
        let id = created["AssociationId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a create reports an AssociationId, got {created}"))
            .to_string();
        ids.push(id);
    }

    let mut first: Vec<Option<f64>> = Vec::new();
    for ((case, _, nodes, _), id) in rows.iter().zip(&ids) {
        if *nodes == 0 {
            assert_settled_over_no_node(&router, case, REGION, id).await;
            first.push(None);
            continue;
        }
        let held = association_when_run_finished(&router, case, REGION, id, 0.0).await;
        assert_eq!(
            held["Overview"]["AssociationStatusAggregatedCount"],
            json!({ "Success": nodes }),
            "case {case:?}: expected the create's run counted over {nodes} node(s), got {held}"
        );
        first.push(executed_at(&held));
    }

    // One call per group, each naming a different number of ids.
    for (case, group) in CALLS {
        let named: Vec<String> = rows
            .iter()
            .zip(&ids)
            .filter(|((_, _, _, named_by), _)| *named_by == Some(*group))
            .map(|(_, id)| id.clone())
            .collect();
        assert!(
            !named.is_empty(),
            "case {case:?}: the table must name at least one association for this call"
        );
        let (status, body) = start(&router, REGION, &named).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from StartAssociationsOnce, got {status} with \
             {body}"
        );
        assert_eq!(
            body,
            json!({}),
            "case {case:?}: the modelled output carries no members, got {body}"
        );
    }

    for (((case, _, nodes, named_by), id), earlier) in rows.iter().zip(&ids).zip(&first) {
        if named_by.is_none() {
            // Only what a request NAMES runs: this association still reports its create's run.
            let held = description(&router, case, REGION, id).await;
            assert_eq!(
                executed_at(&held),
                *earlier,
                "case {case:?}: expected an association no call named to still report the run \
                 it had, got {held}"
            );
            continue;
        }
        if *nodes == 0 {
            // Dispatched to no node: this run settles over no count of targets by status,
            // the way the create's did.
            assert_settled_over_no_node(&router, case, REGION, id).await;
            continue;
        }
        let earlier = earlier.unwrap_or_else(|| {
            panic!("case {case:?}: a run that finished must have reported the instant it did")
        });
        let held = association_when_run_finished(&router, case, REGION, id, earlier).await;
        assert_eq!(
            held["Overview"]["AssociationStatusAggregatedCount"],
            json!({ "Success": nodes }),
            "case {case:?}: expected the new run counted over {nodes} node(s), got {held}"
        );
    }
}

/// RULE: an id the Region the request was signed for holds no association under is
/// `AssociationDoesNotExist`, and nothing is dispatched for that request — wherever in the
/// request the unresolvable id sits, and whether it was never minted or belongs to another
/// Region.
#[tokio::test]
async fn an_id_the_region_holds_no_association_under_refuses_the_whole_request() {
    let (router, engine) = service();
    seed_nodes(&engine);

    // Three associations of REGION, each reaching one node, spanning the targeting forms a
    // create persists.
    let creates: Vec<(&str, Value)> = vec![
        (
            "an InstanceIds target",
            json!({"Name": AGENT, "Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
        ),
        (
            "a document carrying parameters",
            json!({
                "Name": SHELL,
                "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
                "Parameters": {"commands": ["echo hi"]},
            }),
        ),
        (
            "the deprecated InstanceId member",
            json!({"Name": AGENT, "InstanceId": NODE}),
        ),
    ];
    let mut held: Vec<String> = Vec::new();
    for (case, request) in &creates {
        let created = created(&router, case, request).await;
        held.push(
            created["AssociationId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a create reports an AssociationId"))
                .to_string(),
        );
    }

    // One association of ANOTHER Region, reaching the node that Region holds.
    let (status, body) = call_in_region(
        &router,
        OTHER_REGION,
        "CreateAssociation",
        &json!({"Name": AGENT, "Targets": [{"Key": "InstanceIds", "Values": [ELSEWHERE]}]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"another Region's association\": CreateAssociation must succeed, got {status} \
         with {body}"
    );
    let other_region_id = body["AssociationDescription"]["AssociationId"]
        .as_str()
        .expect("a create reports an AssociationId")
        .to_string();

    // Every association's first run is let finish, so the execution each of them reports is
    // the same at every instant from here on and a dispatch is the only thing that can change
    // it.
    let mut settled: Vec<(String, String, String, Value)> = Vec::new();
    for ((case, _), id) in creates.iter().zip(&held) {
        let description = association_when_run_finished(&router, case, REGION, id, 0.0).await;
        settled.push((
            case.to_string(),
            REGION.to_string(),
            id.clone(),
            description,
        ));
    }
    let elsewhere = association_when_run_finished(
        &router,
        "another Region's association",
        OTHER_REGION,
        &other_region_id,
        0.0,
    )
    .await;
    settled.push((
        "another Region's association".to_string(),
        OTHER_REGION.to_string(),
        other_region_id.clone(),
        elsewhere,
    ));

    // (case, the ids one call names, the first of them that resolves to nothing — which is
    // the id the refusal names, the call being refused at the first id it cannot resolve)
    let rows: Vec<(&str, Vec<String>, String)> = vec![
        (
            "an id nothing was minted under, alone",
            vec![UNMINTED[0].to_string()],
            UNMINTED[0].to_string(),
        ),
        (
            "an unresolvable id first",
            vec![UNMINTED[0].to_string(), held[0].clone(), held[1].clone()],
            UNMINTED[0].to_string(),
        ),
        (
            "an unresolvable id in the middle",
            vec![held[0].clone(), UNMINTED[1].to_string(), held[1].clone()],
            UNMINTED[1].to_string(),
        ),
        (
            "an unresolvable id last",
            vec![held[0].clone(), held[1].clone(), held[2].clone(), UNMINTED[1].to_string()],
            UNMINTED[1].to_string(),
        ),
        (
            "an association of another Region",
            vec![held[2].clone(), other_region_id.clone()],
            other_region_id.clone(),
        ),
        (
            "every id unresolvable",
            vec![UNMINTED[0].to_string(), UNMINTED[1].to_string()],
            UNMINTED[0].to_string(),
        ),
    ];

    for (case, named, unresolvable) in &rows {
        let expected = does_not_exist(unresolvable);
        let (status, body) = start(&router, REGION, named).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "AssociationDoesNotExist",
            "case {case:?}: expected AssociationDoesNotExist, got {body}"
        );
        let message = body["message"]
            .as_str()
            .or_else(|| body["Message"].as_str())
            .unwrap_or("<no message>");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // Nothing is dispatched for a refused request: every association it named beside the
        // unresolvable id, and every other one, still reports the run it already had.
        for (settled_case, region, id, before) in &settled {
            let after = description(&router, case, region, id).await;
            assert_eq!(
                &after, before,
                "case {case:?}: expected the association of {settled_case:?} to be untouched, \
                 got {after}"
            );
        }
    }

    // The run every association reports is still its create's, which the clock made due
    // before any of the refused calls: a refusal neither started one nor took one away.
    for (settled_case, _, _, before) in &settled {
        assert!(
            executed_at(before).is_some() && before["Overview"]["Status"] == json!("Success"),
            "case {settled_case:?}: expected a finished run to compare against, got {before}"
        );
    }
}
