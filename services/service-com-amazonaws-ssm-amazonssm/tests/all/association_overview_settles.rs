// SPDX-License-Identifier: CC0-1.0

//! How an association's `Overview` SETTLES, end to end over the front door.
//!
//! RULE under test: State Manager applies an association as soon as it is created, and that
//! execution is over once its targets have been evaluated and the document applied on the
//! nodes they selected — so the overview an account polls reaches a terminal value within
//! one transient window, `Status`/`DetailedStatus` `Success`, whether or not any managed
//! node matched the targets. The count of targets by status is a count over the nodes the
//! execution reached, so an association that reached no node settles with an empty count and
//! one that reached nodes settles with one count per node.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body (`OPERATIONS.md`, "Wire
//! protocol") — and the overview is reached by POLLING `DescribeAssociation` the way a
//! client waits for an association to converge, not by waiting out a fixed span.
//!
//! The table's rows differ along what the rule generalises over: the targeting form
//! (a tag no node carries, a tag a node carries, node ids, every node of the Region), how
//! many registered nodes the targets select (none, one, two) and the document applied.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{call, created, described, service, AGENT, REGION};

/// The `AssociationStatusName` values an association's progress can settle on: "Status can
/// be: Pending, Success, or Failed" (`smithy_query(query="shape", shape="AssociationStatusName")`),
/// of which these two are terminal.
const TERMINAL: &[&str] = &["Success", "Failed"];

/// How long a case waits for the overview to settle before failing. Far longer than the
/// transient window the move is owed after, so a slow or loaded host cannot fail a case that
/// is about WHETHER it settles.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often it looks again while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The tag key the targets below select on.
const TAG: &str = "sim2real-target";

/// A tag value no registered node carries, so targets naming it select nothing.
const UNMATCHED: &str = "no-node-carries-this";

/// The tag value the registered nodes carry.
const MATCHED: &str = "fleet";

/// A registered node id, and a second one, so a case can span how many nodes the targets
/// select.
const NODE: &str = "i-0abc1234";

/// See [`NODE`].
const OTHER_NODE: &str = "i-0abc5678";

/// Register a node carrying the tag the targets select on.
fn register(engine: &Arc<engine::InMemoryEngine>, id: &str) {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, RESOURCE_TYPE_EC2_INSTANCE, at);
    record.properties.platform_type = Some("Linux".to_string());
    record.properties.ping_status = Some("Online".to_string());
    record.tags = [(TAG.to_string(), MATCHED.to_string())]
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

/// Look at the association until its overview reaches a terminal status, up to
/// [`SETTLE_DEADLINE`] — the way a client waits for an association to converge.
async fn settled(router: &MultiServiceRouter, case: &str, association_id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let description = described(
            router,
            case,
            &json!({ "AssociationId": association_id }),
        )
        .await;
        let status = description["Overview"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if TERMINAL.contains(&status.as_str()) {
            return description;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the overview to settle with nothing but the clock \
             running, still {status:?}: {description}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// An association settles its overview whatever its targets selected, and the count of
/// targets by status is the count of the nodes its run reached.
#[tokio::test]
async fn an_association_settles_its_overview_whatever_its_targets_select() {
    let (router, engine) = service();
    register(&engine, NODE);
    register(&engine, OTHER_NODE);

    // (case, the targets the create carries, the count of targets by status it settles with)
    let rows: &[(&str, Value, Value)] = &[
        (
            "a tag no registered node carries",
            json!([{ "Key": format!("tag:{TAG}"), "Values": [UNMATCHED] }]),
            json!({}),
        ),
        (
            "a tag both registered nodes carry",
            json!([{ "Key": format!("tag:{TAG}"), "Values": [MATCHED] }]),
            json!({ "Success": 2 }),
        ),
        (
            "one node named by id",
            json!([{ "Key": "InstanceIds", "Values": [NODE] }]),
            json!({ "Success": 1 }),
        ),
        (
            "every node of the Region",
            json!([{ "Key": "InstanceIds", "Values": ["*"] }]),
            json!({ "Success": 2 }),
        ),
        (
            "a node id no node is registered under",
            json!([{ "Key": "InstanceIds", "Values": ["i-0dead999"] }]),
            json!({}),
        ),
    ];

    for (case, targets, expected_count) in rows {
        let description = created(
            &router,
            case,
            &json!({
                "Name": AGENT,
                "Targets": targets,
                "AssociationName": format!("settles-{}", case.replace(' ', "-")),
            }),
        )
        .await;
        // The create answers before the association is in place, so a caller reading
        // straight after it sees the transient state rather than the settled one.
        assert_eq!(
            description["Overview"]["Status"].as_str(),
            Some("Pending"),
            "case {case:?}: a create answers before the run has finished, got {description}"
        );
        assert_eq!(
            description["Overview"]["DetailedStatus"].as_str(),
            Some("Creating"),
            "case {case:?}: a create answers while the association is still being created, \
             got {description}"
        );
        let association_id = description["AssociationId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a create reports an AssociationId"))
            .to_string();

        let settled = settled(&router, case, &association_id).await;
        assert_eq!(
            settled["Overview"]["Status"].as_str(),
            Some("Success"),
            "case {case:?}: expected the overview to settle Success, got {settled}"
        );
        assert_eq!(
            settled["Overview"]["DetailedStatus"].as_str(),
            Some("Success"),
            "case {case:?}: expected the detail of a finished run, got {settled}"
        );
        assert_eq!(
            settled["Overview"]["AssociationStatusAggregatedCount"], *expected_count,
            "case {case:?}: expected the count of the nodes the run reached, got {settled}"
        );
        assert!(
            !settled["LastExecutionDate"].is_null(),
            "case {case:?}: a settled association reports when its run finished, got {settled}"
        );

        // The settled overview is what a LISTING reports too, so the two reads of one
        // association cannot disagree about whether it converged.
        let (status, listed) = call(
            &router,
            "ListAssociations",
            &json!({
                "AssociationFilterList": [
                    { "key": "AssociationId", "value": association_id },
                ],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the listing must be served, got {status} with {listed}"
        );
        let entry = listed["Associations"]
            .as_array()
            .and_then(|associations| associations.first().cloned())
            .unwrap_or(Value::Null);
        assert_eq!(
            entry["Overview"]["Status"].as_str(),
            Some("Success"),
            "case {case:?}: the listing reports the settled overview, got {listed}"
        );
    }
}
