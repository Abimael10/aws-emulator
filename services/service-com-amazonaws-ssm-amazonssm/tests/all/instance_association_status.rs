// SPDX-License-Identifier: CC0-1.0

//! The status of the State Manager associations that apply to ONE managed node, end to end:
//! `DescribeInstanceAssociationsStatus` reporting the associations `CreateAssociation`
//! persisted and how far the run each of them was dispatched to that node has got.
//!
//! RULE: the report answers, for the node it names, one entry per association of the Region
//! the request was signed for which applies to that node — the association was created FOR
//! the node (the deprecated `InstanceId` member), its latest run was dispatched to the node,
//! or its targets select it — each entry carrying that association's identity from the
//! version in force, the instant the run that reached the node was dispatched and the status
//! that run has reached; a well-formed node id no association applies to is reported as
//! applying none rather than refused; and the entries are served in the store's own ascending
//! association-id order in pages of at most `MaxResults`, where the `NextToken` is the id the
//! next page starts at and a token shaped like no association id is `InvalidNextToken`.
//!
//! Every case goes through the front door a client uses, over the dispatch
//! [`crate::state_manager_associations`] publishes, so this suite shares one calling
//! convention and one reading of the store with the create, listing and update suites.
//!
//! The node inventory is seeded through the service's own state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_managed_node`, the one node store every
//! node describe reads), because a node enters it only by REGISTERING and no public API of
//! this service registers one.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over —
//! which targeting form selects the node, which node kind and tag set it has, which Region
//! the node registered in, which members the create carried, how far the run has got, how
//! big a page is and which shapes a resume token can take — rather than the single report the
//! recorded unit happened to take.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    associations, ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
    RESOURCE_TYPE_MANAGED_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{
    call_in_region, created, error_code, service, AGENT, NODE, OTHER_NODE, OTHER_REGION, REGION,
    SHELL,
};

/// A hybrid node of [`REGION`] (`mi-` and 17 characters, as the modelled `InstanceId` pattern
/// spells an on-premises node), carrying the same tag value as [`NODE`] so a tag target spans
/// node kind.
const HYBRID: &str = "mi-01234567890123456";

/// A node registered only in [`OTHER_REGION`], so neither a target nor a report taken through
/// [`REGION`] resolves it.
const ELSEWHERE: &str = "i-0eeee222";

/// The tag key every seeded node carries a value for, so a tag target selects a subset of the
/// inventory rather than all of it.
const TAG_KEY: &str = "Env";

/// The tag value [`NODE`] and [`HYBRID`] carry.
const PROD: &str = "prod";

/// The tag value [`OTHER_NODE`] carries.
const DEV: &str = "dev";

/// Register `record` in `region` the way a node's own registration would.
fn register(engine: &Arc<engine::InMemoryEngine>, region: &str, record: ManagedNodeRecord) {
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// One registered node: its id, kind and the one tag a target narrows on.
fn node(id: &str, resource_type: &str, tag_value: &str) -> ManagedNodeRecord {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, resource_type, at);
    record.properties.ping_status = Some("Online".to_string());
    record.properties.computer_name = Some(format!("{id}.compute.internal"));
    record.tags = [(TAG_KEY.to_string(), tag_value.to_string())]
        .into_iter()
        .collect::<BTreeMap<String, String>>();
    record
}

/// The inventory every suite below reports against: two EC2 nodes and one hybrid node in
/// [`REGION`], spanning node kind and tag value, and one node registered only in
/// [`OTHER_REGION`].
fn seed_nodes(engine: &Arc<engine::InMemoryEngine>) {
    register(engine, REGION, node(NODE, RESOURCE_TYPE_EC2_INSTANCE, PROD));
    register(
        engine,
        REGION,
        node(OTHER_NODE, RESOURCE_TYPE_EC2_INSTANCE, DEV),
    );
    register(
        engine,
        REGION,
        node(HYBRID, RESOURCE_TYPE_MANAGED_INSTANCE, PROD),
    );
    register(
        engine,
        OTHER_REGION,
        node(ELSEWHERE, RESOURCE_TYPE_EC2_INSTANCE, PROD),
    );
}

/// The nodes of [`REGION`] every row of the targeting table is read against.
const SEEDED: &[&str] = &[NODE, OTHER_NODE, HYBRID];

/// `DescribeInstanceAssociationsStatus` with `body`, signed for `region`.
async fn report_in_region(
    router: &MultiServiceRouter,
    region: &str,
    body: &Value,
) -> (u16, Value) {
    call_in_region(router, region, "DescribeInstanceAssociationsStatus", body).await
}

/// The whole response of a report that must have succeeded.
async fn reported_in_region(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = report_in_region(router, region, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeInstanceAssociationsStatus, got \
         {status} with {parsed:?}"
    );
    parsed
}

/// The `InstanceAssociationStatusInfos` a report of `instance_id` in [`REGION`] carries.
async fn infos(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Vec<Value> {
    let response =
        reported_in_region(router, case, REGION, &json!({"InstanceId": instance_id})).await;
    response["InstanceAssociationStatusInfos"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `AssociationId`s a response reports, in the order it reported them.
fn ids(response: &Value) -> Vec<String> {
    response["InstanceAssociationStatusInfos"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|info| {
            info["AssociationId"]
                .as_str()
                .unwrap_or("<unset>")
                .to_string()
        })
        .collect()
}

/// The refusal message of a failure body.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// The `AssociationId` a create reported.
fn association_id(description: &Value) -> String {
    description["AssociationId"]
        .as_str()
        .unwrap_or("<unset>")
        .to_string()
}

/// Move the run of `association_id` back past the span a run takes to finish
/// ([`associations::ASSOCIATION_EXECUTION_DELAY`]), so the clock owes its completion — the
/// due instant is driven through the store rather than waited for, and nothing sleeps.
fn finish_run(engine: &Arc<engine::InMemoryEngine>, association_id: &str) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let record = store
        .association_mut(REGION, association_id)
        .expect("the create persisted the association");
    let run = record
        .last_run_mut()
        .expect("the create dispatched a run");
    run.started_at -= associations::ASSOCIATION_EXECUTION_DELAY + Duration::from_secs(1);
}

/// One create, and which of the seeded nodes the association it persists applies to.
struct Targeting {
    case: &'static str,
    request: Value,
    applies_to: &'static [&'static str],
}

/// One row per targeting form a create can take, along the axis "which nodes does this
/// association apply to".
fn targetings() -> Vec<Targeting> {
    vec![
        Targeting {
            case: "the node named through the deprecated InstanceId member",
            request: json!({"Name": AGENT, "InstanceId": NODE}),
            applies_to: &[NODE],
        },
        Targeting {
            case: "one node id target",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [OTHER_NODE]}],
            }),
            applies_to: &[OTHER_NODE],
        },
        Targeting {
            case: "two node ids in one target",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [NODE, HYBRID]}],
            }),
            applies_to: &[NODE, HYBRID],
        },
        Targeting {
            case: "a target selecting every node of the Region",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": ["*"]}],
            }),
            applies_to: SEEDED,
        },
        Targeting {
            case: "the EC2-instance target key, which selects only the EC2 nodes",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": "AWS::EC2::Instance", "Values": ["*"]}],
            }),
            applies_to: &[NODE, OTHER_NODE],
        },
        Targeting {
            case: "a tag target, which selects the nodes carrying that tag value",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": format!("tag:{TAG_KEY}"), "Values": [PROD]}],
            }),
            applies_to: &[NODE, HYBRID],
        },
        Targeting {
            case: "a tag target carrying the other value",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": format!("tag:{TAG_KEY}"), "Values": [DEV]}],
            }),
            applies_to: &[OTHER_NODE],
        },
        Targeting {
            case: "a resource-group target, which no node of this emulator is in",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": "resource-groups:Name", "Values": ["web-tier"]}],
            }),
            applies_to: &[],
        },
        Targeting {
            case: "a node id target naming a node registered only in another Region",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [ELSEWHERE]}],
            }),
            applies_to: &[],
        },
        Targeting {
            case: "another document, with the parameters it declares, on one node id target",
            request: json!({
                "Name": SHELL,
                "Parameters": {"commands": ["echo one"]},
                "Targets": [{"Key": "InstanceIds", "Values": [HYBRID]}],
            }),
            applies_to: &[HYBRID],
        },
    ]
}

/// The report of a node lists exactly the associations that apply to THAT node, whatever
/// targeting form selected it, and no association that selects another node.
#[tokio::test]
async fn the_report_lists_the_associations_that_apply_to_the_node() {
    for row in targetings() {
        let (router, engine) = service();
        seed_nodes(&engine);

        let created = created(&router, row.case, &row.request).await;
        let id = association_id(&created);
        let wanted: BTreeSet<&str> = row.applies_to.iter().copied().collect();

        for seeded in SEEDED {
            let reported = infos(&router, row.case, seeded).await;
            let carries = reported
                .iter()
                .any(|info| info["AssociationId"].as_str() == Some(id.as_str()));
            assert_eq!(
                carries,
                wanted.contains(seeded),
                "case {:?}: expected node {seeded} to report the association: {}, got \
                 {reported:?}",
                row.case,
                wanted.contains(seeded)
            );
            assert_eq!(
                reported.len(),
                usize::from(wanted.contains(seeded)),
                "case {:?}: node {seeded} applies at most this one association, got \
                 {reported:?}",
                row.case
            );
            // Every entry names the node it was asked about, not the node a create happened
            // to name.
            for info in &reported {
                assert_eq!(
                    info["InstanceId"].as_str(),
                    Some(*seeded),
                    "case {:?}: expected the entry to name node {seeded}, got {info:?}",
                    row.case
                );
            }
        }
    }
}

/// One create, and what the entry it leads to reports for the association's identity.
struct Identity {
    case: &'static str,
    /// The members the create carries beside the document and the target.
    members: Value,
    /// The `DocumentVersion` the entry reports.
    document_version: Value,
    /// The `AssociationName` the entry reports.
    association_name: Value,
}

/// One row per form the identity members of a create can take.
fn identities() -> Vec<Identity> {
    vec![
        Identity {
            case: "neither a document version nor an association name",
            members: json!({}),
            // A create that names no version associates the document's DEFAULT version, so
            // the association is pinned to that selector and the entry reports it.
            document_version: json!("$DEFAULT"),
            association_name: Value::Null,
        },
        Identity {
            case: "an association name",
            members: json!({"AssociationName": "nightly-agent"}),
            document_version: json!("$DEFAULT"),
            association_name: json!("nightly-agent"),
        },
        Identity {
            case: "the document's default version",
            members: json!({"DocumentVersion": "$DEFAULT"}),
            document_version: json!("$DEFAULT"),
            association_name: Value::Null,
        },
        Identity {
            case: "the document's first version, by number",
            members: json!({"DocumentVersion": "1", "AssociationName": "pinned-agent"}),
            document_version: json!("1"),
            association_name: json!("pinned-agent"),
        },
    ]
}

/// An entry reports the association the create persisted: its id, the document it applies,
/// the version in force and the association name — the same values the create answered with.
#[tokio::test]
async fn an_entry_reports_the_association_the_create_persisted() {
    for row in identities() {
        let (router, engine) = service();
        seed_nodes(&engine);

        let mut request = json!({
            "Name": AGENT,
            "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
        });
        for (member, value) in row
            .members
            .as_object()
            .expect("the row's members are an object")
        {
            request[member] = value.clone();
        }
        let created = created(&router, row.case, &request).await;
        let id = association_id(&created);

        let reported = infos(&router, row.case, NODE).await;
        assert_eq!(
            reported.len(),
            1,
            "case {:?}: expected the one association the create persisted, got {reported:?}",
            row.case
        );
        let entry = &reported[0];
        // (member, what the entry must report)
        let expected: &[(&str, Value)] = &[
            ("AssociationId", json!(id)),
            ("Name", json!(AGENT)),
            ("AssociationVersion", json!("1")),
            ("InstanceId", json!(NODE)),
            ("DocumentVersion", row.document_version.clone()),
            ("AssociationName", row.association_name.clone()),
        ];
        for (member, wanted) in expected {
            assert_eq!(
                &entry[member], wanted,
                "case {:?}: expected {member} to be {wanted:?}, got {:?}",
                row.case, entry[member]
            );
        }
        assert_eq!(
            entry["AssociationVersion"].as_str(),
            created["AssociationVersion"].as_str(),
            "case {:?}: the entry reports the version the create minted, got {entry:?}",
            row.case
        );
    }
}

/// How far a run has got, and what the entry reports for it.
struct Progress {
    case: &'static str,
    /// Whether the node is registered BEFORE the create, so the create's dispatch reaches it.
    registered_before_create: bool,
    /// Whether the clock is moved past the span a run takes to finish.
    run_finished: bool,
    status: Value,
    detailed_status: Value,
    /// Whether the entry reports the instant the run was dispatched.
    reports_execution_date: bool,
}

/// One row per state the run of an association on a node can be in.
fn progressions() -> Vec<Progress> {
    vec![
        Progress {
            case: "a run the clock has not finished yet",
            registered_before_create: true,
            run_finished: false,
            status: json!("Pending"),
            detailed_status: json!("InProgress"),
            reports_execution_date: true,
        },
        Progress {
            case: "a run the clock has finished",
            registered_before_create: true,
            run_finished: true,
            status: json!("Success"),
            detailed_status: json!("Success"),
            reports_execution_date: true,
        },
        Progress {
            case: "an association no run has reached this node for",
            registered_before_create: false,
            run_finished: false,
            status: json!("Pending"),
            detailed_status: Value::Null,
            reports_execution_date: false,
        },
    ]
}

/// The status an entry reports is the run that reached the node: `Pending`/`InProgress` while
/// the dispatched run is still being worked through, `Success` once the clock has finished it,
/// and the status of an association nothing has run — with no execution date and no detail —
/// for a node no run of it has reached.
#[tokio::test]
async fn the_status_an_entry_reports_is_the_run_that_reached_the_node() {
    for row in progressions() {
        let (router, engine) = service();
        if row.registered_before_create {
            seed_nodes(&engine);
        }

        let id = association_id(
            &created(
                &router,
                row.case,
                &json!({
                    "Name": AGENT,
                    "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
                }),
            )
            .await,
        );
        if !row.registered_before_create {
            // The node registers only after the create, so the association's dispatch
            // reached no node — the targets select it now, which is what makes it an
            // association the node applies with no execution to report.
            seed_nodes(&engine);
        }
        if row.run_finished {
            finish_run(&engine, &id);
        }

        let reported = infos(&router, row.case, NODE).await;
        assert_eq!(
            reported.len(),
            1,
            "case {:?}: expected the one association that applies to the node, got {reported:?}",
            row.case
        );
        let entry = &reported[0];
        assert_eq!(
            entry["Status"], row.status,
            "case {:?}: expected Status {:?}, got {:?}",
            row.case, row.status, entry["Status"]
        );
        assert_eq!(
            entry["DetailedStatus"], row.detailed_status,
            "case {:?}: expected DetailedStatus {:?}, got {:?}",
            row.case, row.detailed_status, entry["DetailedStatus"]
        );
        assert_eq!(
            !entry["ExecutionDate"].is_null(),
            row.reports_execution_date,
            "case {:?}: expected an execution date to be reported: {}, got {:?}",
            row.case,
            row.reports_execution_date,
            entry["ExecutionDate"]
        );
        // Nothing in this emulator uploads a run's output or reports a failure for it, so the
        // members only the agent in the node's guest fills are reported by nobody.
        for member in ["ExecutionSummary", "ErrorCode", "OutputUrl"] {
            assert!(
                entry[member].is_null(),
                "case {:?}: expected nothing at {member}, got {:?}",
                row.case,
                entry[member]
            );
        }
    }
}

/// A node id this Region holds no registration under, or one no association applies to, is
/// reported as applying none — a well-formed node id is not a refusal — while an id outside
/// the modelled pattern is refused before any association is read.
#[tokio::test]
async fn a_node_no_association_applies_to_is_reported_as_applying_none() {
    let (router, engine) = service();
    seed_nodes(&engine);
    created(
        &router,
        "the association every row is read against",
        &json!({
            "Name": AGENT,
            "Targets": [{"Key": "InstanceIds", "Values": ["*"]}],
        }),
    )
    .await;

    // (case, the Region the report is signed for, the node id it names) — each id satisfies
    // the modelled `InstanceId` pattern `^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`, and none of
    // them is a node this Region holds an association for.
    let rows: &[(&str, &str, &str)] = &[
        (
            "an 8-character EC2 id no node is registered under",
            REGION,
            "i-0dead999",
        ),
        (
            "a 17-character EC2 id no node is registered under",
            REGION,
            "i-0deadbeefdeadbeef",
        ),
        (
            "a hybrid id no node is registered under",
            REGION,
            "mi-09999999999999999",
        ),
        (
            "a node registered only in another Region",
            REGION,
            ELSEWHERE,
        ),
        (
            "a node of this Region, read through another Region's endpoint",
            OTHER_REGION,
            NODE,
        ),
    ];
    for (case, region, instance_id) in rows {
        let response =
            reported_in_region(&router, case, region, &json!({"InstanceId": instance_id})).await;
        assert_eq!(
            ids(&response),
            Vec::<String>::new(),
            "case {case:?}: expected no association to apply, got {response:?}"
        );
        assert!(
            response["NextToken"].is_null(),
            "case {case:?}: expected no resume token, got {response:?}"
        );
    }

    // The nodes this Region DID register apply the association targeting every node, so each
    // empty answer above is the node and not the request.
    for seeded in SEEDED {
        let reported = infos(&router, "a registered node", seeded).await;
        assert_eq!(
            reported.len(),
            1,
            "case \"a registered node\": expected node {seeded} to report the association \
             targeting every node, got {reported:?}"
        );
    }

    // A node id outside the modelled `InstanceId` pattern is refused as a request validation,
    // stated against every constraint of that shape the value broke — its length and its
    // pattern — rather than against the first of them.
    for (malformed, violated) in [
        (
            "instance-1",
            concat!(
                "1 validation error detected: Value 'instance-1' at 'instanceId' failed to ",
                r"satisfy constraint: Member must satisfy regular expression pattern: ",
                r"(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)"
            ),
        ),
        (
            "i-xyz",
            concat!(
                "2 validation errors detected: Value 'i-xyz' at 'instanceId' failed to satisfy ",
                "constraint: Member must have length greater than or equal to 10; Value ",
                "'i-xyz' at 'instanceId' failed to satisfy constraint: Member must satisfy ",
                r"regular expression pattern: (^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)"
            ),
        ),
        (
            "i-0deadbeefdeadbeef01",
            concat!(
                "2 validation errors detected: Value 'i-0deadbeefdeadbeef01' at 'instanceId' ",
                "failed to satisfy constraint: Member must have length less than or equal to ",
                "20; Value 'i-0deadbeefdeadbeef01' at 'instanceId' failed to satisfy ",
                r"constraint: Member must satisfy regular expression pattern: ",
                r"(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)"
            ),
        ),
    ] {
        let (status, parsed) =
            report_in_region(&router, REGION, &json!({"InstanceId": malformed})).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ValidationException"),
            "case {malformed:?}: expected 400 ValidationException for a node id outside the \
             modelled pattern, got {status} with {parsed:?}"
        );
        assert_eq!(
            error_message(&parsed),
            violated,
            "case {malformed:?}: expected the violated constraints {violated:?}, got {:?}",
            error_message(&parsed)
        );
    }
}

/// The node a create named through the deprecated `InstanceId` member applies that
/// association whether or not the node has registered: the member is an identity the
/// association keeps, not criteria resolved per run — and with nothing dispatched to an
/// unregistered node, the entry reports no execution.
#[tokio::test]
async fn the_node_a_create_named_applies_the_association_before_it_registers() {
    let (router, engine) = service();
    // No node is registered at all, so the create dispatches to none.
    let created = created(
        &router,
        "a create naming a node that has not registered",
        &json!({"Name": AGENT, "InstanceId": NODE}),
    )
    .await;
    let id = association_id(&created);

    let reported = infos(&router, "the node the create named", NODE).await;
    assert_eq!(
        reported.len(),
        1,
        "case \"the node the create named\": expected the association created for it, got \
         {reported:?}"
    );
    let entry = &reported[0];
    assert_eq!(
        entry["AssociationId"], json!(id),
        "case \"the node the create named\": expected the created association, got {entry:?}"
    );
    assert_eq!(
        (entry["Status"].clone(), entry["DetailedStatus"].clone()),
        (json!("Pending"), Value::Null),
        "case \"the node the create named\": nothing has run it on that node, got {entry:?}"
    );
    assert!(
        entry["ExecutionDate"].is_null(),
        "case \"the node the create named\": expected no execution date, got {entry:?}"
    );

    // Another node applies nothing: the legacy member names ONE node.
    let other = infos(&router, "a node the create did not name", OTHER_NODE).await;
    assert_eq!(
        other,
        Vec::<Value>::new(),
        "case \"a node the create did not name\": expected no association to apply, got \
         {other:?}"
    );

    // Once that node registers, the association is dispatched to it by the next run and the
    // entry still reports the one association, now with the node registered.
    seed_nodes(&engine);
    let after = infos(&router, "the node the create named, now registered", NODE).await;
    assert_eq!(
        after.len(),
        1,
        "case \"the node the create named, now registered\": expected the one association, \
         got {after:?}"
    );
}

/// Create `held` associations of [`REGION`], each targeting every node, and answer their ids
/// in ascending order.
async fn with_associations(
    router: &MultiServiceRouter,
    held: usize,
) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 0..held {
        let case = format!("the association {index} of the page walk");
        let created = created(
            router,
            &case,
            &json!({
                "Name": AGENT,
                "AssociationName": format!("walk-{index}"),
                "Targets": [{"Key": "InstanceIds", "Values": ["*"]}],
            }),
        )
        .await;
        ids.push(association_id(&created));
    }
    ids.sort();
    ids
}

/// A page holds at most `MaxResults` entries and hands back the id the next page starts at, so
/// walking the pages reports every association that applies to the node exactly once, in the
/// store's own ascending order.
#[tokio::test]
async fn paging_reports_every_association_that_applies_exactly_once() {
    let (router, engine) = service();
    seed_nodes(&engine);
    let held = 5;
    let whole = with_associations(&router, held).await;

    // The page sizes span the modelled range (`MaxResults`, 1..50): the minimum, sizes that
    // do and do not divide the held count, the count itself, the maximum, and the request
    // that names no `MaxResults` at all.
    let sizes: &[Option<usize>] = &[Some(1), Some(2), Some(3), Some(held), Some(50), None];
    for size in sizes {
        let case = format!("a page size of {size:?}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut request = json!({"InstanceId": NODE});
            if let Some(size) = size {
                request["MaxResults"] = json!(size);
            }
            if let Some(token) = &token {
                request["NextToken"] = json!(token);
            }
            let response = reported_in_region(&router, &case, REGION, &request).await;
            let page = ids(&response);
            assert!(
                page.len() <= size.unwrap_or(held),
                "case {case:?}: expected at most {:?} entries in a page, got {page:?}",
                size.unwrap_or(held)
            );
            walked.extend(page);
            pages += 1;
            token = response["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                pages <= held + 1,
                "case {case:?}: the walk must terminate, got {walked:?}"
            );
        }
        assert_eq!(
            walked, whole,
            "case {case:?}: expected every association exactly once in ascending id order, \
             got {walked:?}"
        );
        let expected_pages = held.div_ceil(size.unwrap_or(held));
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: expected {expected_pages} pages, got {pages}"
        );
    }

    // A token of the modelled form names a position in the store's own order, so a report
    // resumed from one carries the associations at or after it and nothing before it.
    for (index, id) in whole.iter().enumerate() {
        let case = format!("a resume at the association {index}");
        let response = reported_in_region(
            &router,
            &case,
            REGION,
            &json!({"InstanceId": NODE, "NextToken": id}),
        )
        .await;
        assert_eq!(
            ids(&response),
            whole[index..].to_vec(),
            "case {case:?}: expected the walk to resume at {id}, got {:?}",
            ids(&response)
        );
    }
}

/// A `NextToken` shaped like no association id was not issued here, and a page size outside
/// the modelled range is refused before anything is reported.
#[tokio::test]
async fn a_token_this_report_never_issued_is_refused() {
    let (router, engine) = service();
    seed_nodes(&engine);
    with_associations(&router, 2).await;

    // Each row is a token outside the modelled `AssociationId` pattern
    // `^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$`: a
    // word, an id of the right groups but the wrong widths, one group short, one group too
    // many, and a group that is not hexadecimal.
    let tokens: &[(&str, &str)] = &[
        ("a word", "next-page"),
        ("the groups too short", "0123456-0000-0000-0000-000000000000"),
        ("one group short", "01234567-0000-0000-000000000000"),
        (
            "one group too many",
            "01234567-0000-0000-0000-000000000000-0000",
        ),
        (
            "a group that is not hexadecimal",
            "0123456g-0000-0000-0000-000000000000",
        ),
    ];
    for (case, token) in tokens {
        let (status, parsed) =
            report_in_region(&router, REGION, &json!({"InstanceId": NODE, "NextToken": token}))
                .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a token this report never issued, got \
             {status} with {parsed:?}"
        );
        let code = error_code(&parsed);
        assert_eq!(
            code, "InvalidNextToken",
            "case {case:?}: expected InvalidNextToken, got {code} with {parsed:?}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message, "The NextToken value is not valid",
            "case {case:?}: expected the service's own sentence, got {message:?}"
        );
    }

    // The page size the model bounds: one below the minimum and one above the maximum.
    for (case, max_results) in [("below the modelled minimum", 0), ("above the modelled maximum", 51)]
    {
        let (status, parsed) = report_in_region(
            &router,
            REGION,
            &json!({"InstanceId": NODE, "MaxResults": max_results}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a page size {max_results}, got {status} with \
             {parsed:?}"
        );
    }

    // An empty token is read the same way as one the request omitted: the first page.
    let response = reported_in_region(
        &router,
        "an empty token",
        REGION,
        &json!({"InstanceId": NODE, "NextToken": ""}),
    )
    .await;
    assert_eq!(
        ids(&response).len(),
        2,
        "case \"an empty token\": expected the first page, got {response:?}"
    );
}

/// The report is a READ: two reports taken of one node carry the same entries, and the store
/// holds afterwards exactly what it held before — no run is started, finished or re-dated by
/// being asked about.
#[tokio::test]
async fn a_report_changes_nothing_a_later_caller_sees() {
    let (router, engine) = service();
    seed_nodes(&engine);
    let held = with_associations(&router, 3).await;
    // Two of the three runs are finished through the store, so every entry's status is one
    // the clock no longer moves and two reports are comparable whole.
    for id in held.iter().take(2) {
        finish_run(&engine, id);
    }

    let before = stored_runs(&engine);
    let first = reported_in_region(
        &router,
        "the first report",
        REGION,
        &json!({"InstanceId": NODE}),
    )
    .await;
    let second = reported_in_region(
        &router,
        "the second report",
        REGION,
        &json!({"InstanceId": NODE}),
    )
    .await;
    let after = stored_runs(&engine);

    assert_eq!(
        ids(&first),
        held,
        "the report carries the associations the creates persisted, got {first:?}"
    );
    assert_eq!(
        statuses(&first),
        vec![
            "Success".to_string(),
            "Success".to_string(),
            "Pending".to_string(),
        ],
        "the two finished runs report Success and the unfinished one Pending, got {first:?}"
    );
    assert_eq!(
        first, second,
        "two reports of one node report the same thing, got {second:?}"
    );
    assert_eq!(
        before, after,
        "a report starts, finishes or re-dates no run, got {after:?}"
    );
}

/// Every association's stored run, as `(association id, dispatch instant, nodes)` — the state
/// a read must leave alone.
fn stored_runs(engine: &Arc<engine::InMemoryEngine>) -> Vec<(String, Option<SystemTime>, Vec<String>)> {
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .associations
        .values()
        .map(|record| {
            (
                record.association_id.clone(),
                record.last_run().map(|run| run.started_at),
                record
                    .last_run()
                    .map(|run| run.nodes.clone())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// The `Status` of every entry a response carries, in order.
fn statuses(response: &Value) -> Vec<String> {
    response["InstanceAssociationStatusInfos"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|info| info["Status"].as_str().unwrap_or("<unset>").to_string())
        .collect()
}
