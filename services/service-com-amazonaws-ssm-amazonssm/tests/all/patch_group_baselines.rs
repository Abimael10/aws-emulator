// SPDX-License-Identifier: CC0-1.0

//! The patch baseline a PATCH GROUP is patched against, and the compliance a node of that
//! group reports, end to end: `RegisterPatchBaselineForPatchGroup` pairs a group with a
//! baseline, `GetPatchBaselineForPatchGroup` reports the baseline that should be used for a
//! group, a run of the published patch-baseline document is performed against that baseline,
//! and `ListComplianceItems` reports the compliance that run gathered.
//!
//! RULE under test: the baseline a node is patched against is the one its patch group is
//! REGISTERED with, and the default baseline of the node's operating system when its group
//! has none — a node's group being the value of its `Patch Group` or `PatchGroup` tag. One
//! group holds one baseline per operating system: registering a second one for that operating
//! system is refused, deleting a baseline a group still holds is refused, and deregistering
//! the pairing puts the group back on the default. What a run of the patch-baseline document
//! reports for the node is the compliance `ListComplianceItems` then reports, one item per
//! patch, COMPLIANT exactly when the patch is not in a state the baseline wants it out of.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol").
//!
//! The node inventory is seeded through the service's own state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_managed_node`, the one node store every
//! read of this service projects), because a node enters it only by REGISTERING and no public
//! API of this service registers one — the same seam
//! `patch_baseline_runs.rs` seeds. Everything else a case needs is done through the API, so
//! what is asserted is computed from what the emulator persisted.
//!
//! The suites are tables: each row is one input along the axis the rule generalises over
//! (which tag spelling names the group, whether the group is registered, which operating
//! system the baseline applies to, whether an association's targets select a node and whether
//! the run it dispatched has finished, the filter key and comparison a compliance read narrows
//! by), rather than the single estate one recorded deployment happened to build.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    associations::ASSOCIATION_EXECUTION_DELAY, ComAmazonawsSsmAmazonssmState, ManagedNodeRecord,
    RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for. Deliberately not the emulator's default, so what is
/// read here proves the Region came from the request's SigV4 credential scope.
const REGION: &str = "eu-west-1";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// The Command document Patch Manager publishes, which exists in every account and Region
/// without anybody creating it (`state::patch_runs::RUN_PATCH_BASELINE_DOCUMENT`).
const PATCH_DOCUMENT: &str = "AWS-RunPatchBaseline";

/// The two tag keys whose value names a node's patch group ("the tag key must be `Patch
/// Group` or `PatchGroup`", AWS Systems Manager User Guide, "Add nodes to a patch group").
const PATCH_GROUP_TAG: &str = "Patch Group";
const PATCH_GROUP_TAG_COMPACT: &str = "PatchGroup";

/// The group a baseline is registered for in these cases.
const REGISTERED_GROUP: &str = "production fleet";

/// A group nothing registers a baseline for.
const UNREGISTERED_GROUP: &str = "staging fleet";

/// The `OperatingSystem` a Linux node is patched against here: a managed node reports no
/// `PlatformName`, and nothing in this suite's engine holds an Amazon EC2 image to read a
/// distribution off, so a Linux node is patched against the Amazon Linux release
/// `state::patch_runs::FALLBACK_LINUX_OPERATING_SYSTEM` names.
const LINUX_OPERATING_SYSTEM: &str = "AMAZON_LINUX_2023";

/// The operating system a read that names none is served for: "the default value is WINDOWS"
/// (`GetPatchBaselineForPatchGroupRequest$OperatingSystem`).
const DEFAULT_OPERATING_SYSTEM: &str = "WINDOWS";

/// The `PlatformType` of the nodes below.
const LINUX_PLATFORM: &str = "Linux";

/// The compliance level the baselines of these cases assign the patches they approve, which
/// is what a compliance item reports as its `Severity`.
const COMPLIANCE_LEVEL: &str = "HIGH";

/// The compliance type of an item Patch Manager reports, and the one resource type the
/// compliance reads support.
const COMPLIANCE_TYPE_PATCH: &str = "Patch";
const RESOURCE_TYPE_MANAGED_INSTANCE: &str = "ManagedInstance";

/// A router with just this service mounted, and the engine it was mounted on — the handle a
/// case seeds the node inventory through.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what these tests are about; the router still reads the SigV4
    // credential scope below, which is where the Region comes from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Call `operation` signed for [`REGION`], returning `(status, body)`.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/ssm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    // The error shape travels in `__type`; fold in the header the framework also sets so one
    // accessor reads either.
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// Call `operation`, asserting it succeeded.
async fn call_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to succeed, got {status} with {parsed:?}"
    );
    parsed
}

/// The wire error code of a failure body (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .split('#')
        .next_back()
        .unwrap_or(rendered)
        .to_string()
}

/// One registered node: its id and the tags it carries.
fn node(id: &str, tags: &[(&str, &str)]) -> ManagedNodeRecord {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, RESOURCE_TYPE_EC2_INSTANCE, at);
    record.properties.platform_type = Some(LINUX_PLATFORM.to_string());
    record.properties.ping_status = Some("Online".to_string());
    record.tags = tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect::<BTreeMap<String, String>>();
    record
}

/// Register `record` the way a node's own registration would.
fn register_node(engine: &Arc<engine::InMemoryEngine>, record: ManagedNodeRecord) {
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(REGION, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// A baseline of `operating_system` that approves every patch published for it on the day it
/// was released — "the filter values can be exact values for the patch property given as a
/// key, or a wildcard (*), which matches all values"
/// (`smithy_query(query="shape", shape="PatchFilter")`) — and the id its create minted.
async fn baseline(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    operating_system: &str,
) -> String {
    let created = call_ok(
        router,
        case,
        "CreatePatchBaseline",
        &json!({
            "Name": name,
            "OperatingSystem": operating_system,
            "ApprovalRules": {
                "PatchRules": [{
                    "PatchFilterGroup": { "PatchFilters": [{ "Key": "PRODUCT", "Values": ["*"] }] },
                    "ApproveAfterDays": 0,
                    "ComplianceLevel": COMPLIANCE_LEVEL,
                    "EnableNonSecurity": true,
                }],
            },
        }),
    )
    .await;
    created["BaselineId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports a BaselineId, got {created}"))
        .to_string()
}

/// Register `baseline_id` for `patch_group`, asserting the register was served.
async fn register_for_group(
    router: &MultiServiceRouter,
    case: &str,
    baseline_id: &str,
    patch_group: &str,
) -> Value {
    call_ok(
        router,
        case,
        "RegisterPatchBaselineForPatchGroup",
        &json!({ "BaselineId": baseline_id, "PatchGroup": patch_group }),
    )
    .await
}

/// The `BaselineId` `GetPatchBaselineForPatchGroup` reports for `patch_group`, with the
/// operating system a row names where it names one.
async fn baseline_for_group(
    router: &MultiServiceRouter,
    case: &str,
    patch_group: &str,
    operating_system: Option<&str>,
) -> Value {
    let mut body = json!({ "PatchGroup": patch_group });
    if let Some(operating_system) = operating_system {
        body["OperatingSystem"] = json!(operating_system);
    }
    call_ok(router, case, "GetPatchBaselineForPatchGroup", &body).await
}

/// Send the published patch-baseline document to `instance_id` as a scan, asserting the send
/// was accepted.
async fn scan(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    call_ok(
        router,
        case,
        "SendCommand",
        &json!({
            "DocumentName": PATCH_DOCUMENT,
            "InstanceIds": [instance_id],
            "Parameters": { "Operation": ["Scan"] },
        }),
    )
    .await;
}

/// The one `InstancePatchState` `DescribeInstancePatchStates` reports for `instance_id`.
async fn patch_state(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Value {
    let reported = call_ok(
        router,
        case,
        "DescribeInstancePatchStates",
        &json!({ "InstanceIds": [instance_id] }),
    )
    .await;
    let states = reported["InstancePatchStates"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        states.len(),
        1,
        "case {case:?}: expected one report for {instance_id}, got {reported}"
    );
    states[0].clone()
}

/// The compliance items `ListComplianceItems` reports for `instance_id` under `filters`.
async fn compliance_items(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    filters: Value,
) -> Vec<Value> {
    let reported = call_ok(
        router,
        case,
        "ListComplianceItems",
        &json!({
            "ResourceIds": [instance_id],
            "ResourceTypes": [RESOURCE_TYPE_MANAGED_INSTANCE],
            "Filters": filters,
            "MaxResults": 50,
        }),
    )
    .await;
    reported["ComplianceItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// RULE: the baseline that should be used for a patch group is the one registered for it and
/// that operating system, and the default baseline of the operating system asked about for a
/// group nothing has registered one for — which is also what a baseline reports its patch
/// groups as, and what a deregister puts back.
///
/// The rows differ along what the rule generalises over: whether the group is registered,
/// which operating system the read asks about, a group registered for one operating system
/// read for another, a group nothing registered at all, and the group as it stands after the
/// registration is taken away.
#[tokio::test]
async fn the_baseline_for_a_patch_group_is_the_one_registered_for_it() {
    let (router, _engine) = service();
    let case = "a group registered with a baseline of its own";
    let linux = baseline(&router, case, "Group.Linux", LINUX_OPERATING_SYSTEM).await;
    let windows = baseline(&router, case, "Group.Windows", DEFAULT_OPERATING_SYSTEM).await;

    // What the register answers back is the pair it recorded.
    let registered = register_for_group(&router, case, &linux, REGISTERED_GROUP).await;
    assert_eq!(
        (
            registered["BaselineId"].as_str(),
            registered["PatchGroup"].as_str()
        ),
        (Some(linux.as_str()), Some(REGISTERED_GROUP)),
        "case {case:?}: expected the register to report the pair it recorded, got {registered}"
    );

    // The default of each operating system, for the rows below that fall back on it.
    let default_linux = call_ok(
        &router,
        case,
        "GetDefaultPatchBaseline",
        &json!({ "OperatingSystem": LINUX_OPERATING_SYSTEM }),
    )
    .await["BaselineId"]
        .as_str()
        .expect("a default baseline is reported for every operating system")
        .to_string();
    let default_windows = call_ok(
        &router,
        case,
        "GetDefaultPatchBaseline",
        &json!({ "OperatingSystem": DEFAULT_OPERATING_SYSTEM }),
    )
    .await["BaselineId"]
        .as_str()
        .expect("a default baseline is reported for every operating system")
        .to_string();

    // (case, the group, the operating system the read names, the baseline it must report)
    let rows: Vec<(&str, &str, Option<&str>, String)> = vec![
        (
            "the registered group, read for its own operating system",
            REGISTERED_GROUP,
            Some(LINUX_OPERATING_SYSTEM),
            linux.clone(),
        ),
        (
            "the registered group, read for another operating system",
            REGISTERED_GROUP,
            Some(DEFAULT_OPERATING_SYSTEM),
            default_windows.clone(),
        ),
        (
            "the registered group, read naming no operating system",
            REGISTERED_GROUP,
            None,
            default_windows.clone(),
        ),
        (
            "a group nothing has registered a baseline for",
            UNREGISTERED_GROUP,
            Some(LINUX_OPERATING_SYSTEM),
            default_linux.clone(),
        ),
    ];
    for (row, group, operating_system, expected) in &rows {
        let reported = baseline_for_group(&router, row, group, *operating_system).await;
        assert_eq!(
            reported["BaselineId"].as_str(),
            Some(expected.as_str()),
            "case {row:?}: expected the baseline {expected}, got {reported}"
        );
        assert_eq!(
            reported["PatchGroup"].as_str(),
            Some(*group),
            "case {row:?}: expected the group {group} to be reported back, got {reported}"
        );
    }

    // The baseline reports the group it was registered for among its own patch groups.
    let read = call_ok(
        &router,
        case,
        "GetPatchBaseline",
        &json!({ "BaselineId": linux }),
    )
    .await;
    assert_eq!(
        read["PatchGroups"].as_array().map(Vec::as_slice),
        Some([json!(REGISTERED_GROUP)].as_slice()),
        "case {case:?}: expected the baseline to report its one patch group, got {read}"
    );

    // A SECOND baseline of that operating system for the same group is refused, and the
    // pairing is left as it was.
    let case = "a second baseline for a group that already holds one";
    let other = baseline(&router, case, "Group.Linux.Other", LINUX_OPERATING_SYSTEM).await;
    let (status, refused) = call(
        &router,
        "RegisterPatchBaselineForPatchGroup",
        &json!({ "BaselineId": other, "PatchGroup": REGISTERED_GROUP }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "AlreadyExistsException"),
        "case {case:?}: expected the register to be refused, got {status} with {refused}"
    );
    assert!(
        refused["Message"]
            .as_str()
            .is_some_and(|message| message.contains(REGISTERED_GROUP) && message.contains(&linux)),
        "case {case:?}: expected the refusal to name the group and the baseline it holds, got \
         {refused}"
    );

    // Registering the baseline the group ALREADY holds is the state the call asks for.
    let case = "the baseline the group already holds";
    register_for_group(&router, case, &linux, REGISTERED_GROUP).await;
    let reported = baseline_for_group(&router, case, REGISTERED_GROUP, Some(LINUX_OPERATING_SYSTEM))
        .await;
    assert_eq!(
        reported["BaselineId"].as_str(),
        Some(linux.as_str()),
        "case {case:?}: expected the group to keep its baseline, got {reported}"
    );

    // The baseline a group still holds cannot be deleted.
    let case = "deleting a baseline a group still holds";
    let (status, refused) = call(
        &router,
        "DeletePatchBaseline",
        &json!({ "BaselineId": linux }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ResourceInUseException"),
        "case {case:?}: expected the delete to be refused, got {status} with {refused}"
    );
    assert!(
        refused["Message"]
            .as_str()
            .is_some_and(|message| message.contains(REGISTERED_GROUP)),
        "case {case:?}: expected the refusal to name the group holding it, got {refused}"
    );

    // Deregistering puts the group back on the default of its operating system, and the
    // baseline reports no group — then the delete goes through.
    let case = "the group after its registration is taken away";
    let deregistered = call_ok(
        &router,
        case,
        "DeregisterPatchBaselineForPatchGroup",
        &json!({ "BaselineId": linux, "PatchGroup": REGISTERED_GROUP }),
    )
    .await;
    assert_eq!(
        (
            deregistered["BaselineId"].as_str(),
            deregistered["PatchGroup"].as_str()
        ),
        (Some(linux.as_str()), Some(REGISTERED_GROUP)),
        "case {case:?}: expected the deregister to report the pair it took away, got \
         {deregistered}"
    );
    let reported = baseline_for_group(&router, case, REGISTERED_GROUP, Some(LINUX_OPERATING_SYSTEM))
        .await;
    assert_eq!(
        reported["BaselineId"].as_str(),
        Some(default_linux.as_str()),
        "case {case:?}: expected the group to be back on the default baseline, got {reported}"
    );
    let read = call_ok(
        &router,
        case,
        "GetPatchBaseline",
        &json!({ "BaselineId": linux }),
    )
    .await;
    assert_eq!(
        read["PatchGroups"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected the baseline to report no patch group, got {read}"
    );
    call_ok(
        &router,
        case,
        "DeletePatchBaseline",
        &json!({ "BaselineId": linux }),
    )
    .await;

    // The Windows baseline was never registered for the group, so it is deletable and the
    // group's Windows read still falls back on the default.
    call_ok(
        &router,
        "an unregistered baseline",
        "DeletePatchBaseline",
        &json!({ "BaselineId": windows }),
    )
    .await;
}

/// RULE: a patching operation on a node is performed against the baseline in force for that
/// node's patch group — the registered one, and the default of the node's operating system
/// when its group has none — and the report it leaves carries both that baseline and the
/// group the node belongs to.
///
/// The rows differ along what the rule generalises over: which of the two tag spellings names
/// the node's group, whether that group is registered with a baseline, and a node carrying no
/// patch-group tag at all.
#[tokio::test]
async fn a_node_is_patched_against_the_baseline_its_patch_group_holds() {
    let (router, engine) = service();
    let case = "the estate";
    let registered = baseline(&router, case, "Fleet.Registered", LINUX_OPERATING_SYSTEM).await;
    let fallback = baseline(&router, case, "Fleet.Default", LINUX_OPERATING_SYSTEM).await;
    register_for_group(&router, case, &registered, REGISTERED_GROUP).await;
    call_ok(
        &router,
        case,
        "RegisterDefaultPatchBaseline",
        &json!({ "BaselineId": fallback }),
    )
    .await;

    // One row per node of the estate: what it is tagged with, the group that puts it in, and
    // the baseline in force for that group.
    struct Node {
        case: &'static str,
        instance_id: &'static str,
        tags: Vec<(&'static str, &'static str)>,
        patch_group: &'static str,
        baseline: String,
    }
    let rows: Vec<Node> = vec![
        Node {
            case: "a node whose `Patch Group` tag names the registered group",
            instance_id: "i-0aaa1111",
            tags: vec![(PATCH_GROUP_TAG, REGISTERED_GROUP)],
            patch_group: REGISTERED_GROUP,
            baseline: registered.clone(),
        },
        Node {
            case: "a node whose `PatchGroup` tag names the same group",
            instance_id: "i-0bbb2222",
            tags: vec![(PATCH_GROUP_TAG_COMPACT, REGISTERED_GROUP)],
            patch_group: REGISTERED_GROUP,
            baseline: registered.clone(),
        },
        Node {
            case: "a node in a group nothing registered a baseline for",
            instance_id: "i-0ccc3333",
            tags: vec![(PATCH_GROUP_TAG, UNREGISTERED_GROUP)],
            patch_group: UNREGISTERED_GROUP,
            baseline: fallback.clone(),
        },
        Node {
            case: "a node carrying no patch-group tag at all",
            instance_id: "i-0ddd4444",
            tags: vec![("Name", "ungrouped")],
            patch_group: "",
            baseline: fallback.clone(),
        },
    ];
    for row in &rows {
        register_node(&engine, node(row.instance_id, &row.tags));
        scan(&router, row.case, row.instance_id).await;
    }
    for row in &rows {
        let (case, instance_id, group, expected) =
            (row.case, row.instance_id, row.patch_group, &row.baseline);
        let reported = patch_state(&router, case, instance_id).await;
        assert_eq!(
            reported["BaselineId"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the run against the baseline {expected}, got {reported}"
        );
        assert_eq!(
            reported["PatchGroup"].as_str().unwrap_or_default(),
            group,
            "case {case:?}: expected the report to carry the group {group:?}, got {reported}"
        );
        assert_eq!(
            reported["Operation"].as_str(),
            Some("Scan"),
            "case {case:?}: expected the scan to be reported, got {reported}"
        );
    }

    // The group read reports only the nodes of that group, which is what a patch-group read
    // is for.
    let case = "the registered group's own nodes";
    let reported = call_ok(
        &router,
        case,
        "DescribeInstancePatchStatesForPatchGroup",
        &json!({ "PatchGroup": REGISTERED_GROUP }),
    )
    .await;
    let nodes: Vec<&str> = reported["InstancePatchStates"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|state| state["InstanceId"].as_str())
        .collect();
    assert_eq!(
        nodes,
        vec!["i-0aaa1111", "i-0bbb2222"],
        "case {case:?}: expected the group's two nodes, got {reported}"
    );
}

/// RULE: the compliance a node reports is what the run that applied the patch-baseline
/// document to it found — so a node a FINISHED run of a State Manager association applying
/// that document has reached holds that run's report, and a node no such run reached holds
/// none: the association's targets have to select it, the run has to have been dispatched at
/// all (an association that runs only at its cron interval dispatches none when it is
/// created), and the run has to have finished.
///
/// The rows differ along what the rule generalises over: whether the association's targets
/// select the node, whether its version asks to run only on its schedule, and whether the run
/// it dispatched has finished yet.
#[tokio::test]
async fn an_association_reports_the_patching_its_finished_run_applied() {
    let (router, engine) = service();
    let case = "the estate";
    let registered = baseline(&router, case, "Applied.Linux", LINUX_OPERATING_SYSTEM).await;
    register_for_group(&router, case, &registered, REGISTERED_GROUP).await;

    // One node per row, registered before any association is created: the nodes an
    // association's targets select are resolved against this store.
    let selected = "i-0fff6666";
    let cron_only = "i-0fff7777";
    let unselected = "i-0fff8888";
    for (instance_id, group) in [
        (selected, REGISTERED_GROUP),
        // Neither of the other two is in the group the first association's target names, so
        // the only association that could apply the document to the cron-only node is its
        // own, which dispatches nothing.
        (cron_only, UNREGISTERED_GROUP),
        (unselected, UNREGISTERED_GROUP),
    ] {
        register_node(&engine, node(instance_id, &[(PATCH_GROUP_TAG, group)]));
    }

    // The association a client deploys beside such an estate: the published patch-baseline
    // document, applied to the nodes a tag target selects.
    let case = "an association whose targets select the node";
    let created = call_ok(
        &router,
        case,
        "CreateAssociation",
        &json!({
            "Name": PATCH_DOCUMENT,
            "Targets": [{ "Key": format!("tag:{PATCH_GROUP_TAG}"), "Values": [REGISTERED_GROUP] }],
            "Parameters": { "Operation": ["Scan"] },
        }),
    )
    .await;
    let association_id = created["AssociationDescription"]["AssociationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports an AssociationId, got {created}"))
        .to_string();

    // One that asks to run only at its cron interval, which State Manager dispatches nothing
    // for when it is created — so nothing has applied the document to its node.
    let cron_case = "an association that runs only at its cron interval";
    call_ok(
        &router,
        cron_case,
        "CreateAssociation",
        &json!({
            "Name": PATCH_DOCUMENT,
            "Targets": [{ "Key": "InstanceIds", "Values": [cron_only] }],
            "Parameters": { "Operation": ["Scan"] },
            "ScheduleExpression": "rate(30 minutes)",
            "ApplyOnlyAtCronInterval": true,
        }),
    )
    .await;

    // Until the run the create dispatched has FINISHED it has brought nothing back, so the
    // node it reached holds no compliance yet.
    let reported = call_ok(
        &router,
        case,
        "DescribeInstancePatchStates",
        &json!({ "InstanceIds": [selected, cron_only, unselected] }),
    )
    .await;
    assert_eq!(
        reported["InstancePatchStates"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected no compliance while the run is still in progress, got \
         {reported}"
    );

    // Move the instant the run was dispatched back, so the case about WHAT it reported does
    // not wait out the span the clock owes it — the move this crate's other clock-owed suites
    // make through the store rather than sleeping for one.
    dispatched_before(&engine, &association_id);

    // The node the run reached holds its report, against the baseline its group holds.
    let state = patch_state(&router, case, selected).await;
    assert_eq!(
        (state["BaselineId"].as_str(), state["PatchGroup"].as_str()),
        (Some(registered.as_str()), Some(REGISTERED_GROUP)),
        "case {case:?}: expected the finished run's report against the group's baseline, got \
         {state}"
    );

    // And the two nodes no finished run reached hold none: one because its association
    // dispatched nothing, the other because no association's targets select it.
    for (row, instance_id) in [
        (cron_case, cron_only),
        ("a node no association's targets select", unselected),
    ] {
        let reported = call_ok(
            &router,
            row,
            "DescribeInstancePatchStates",
            &json!({ "InstanceIds": [instance_id] }),
        )
        .await;
        assert_eq!(
            reported["InstancePatchStates"].as_array().map(Vec::len),
            Some(0),
            "case {row:?}: expected no compliance for {instance_id}, got {reported}"
        );
    }

    // A second read reports the same compliance: the report of one run is written once, so a
    // read does not move what a later caller sees.
    let again = patch_state(&router, case, selected).await;
    assert_eq!(
        again, state,
        "case {case:?}: expected the same report on a second read, got {again}"
    );
}

/// Move the instant the current run of `association_id` was dispatched far enough back that
/// the clock owes its result, through the store this crate's other clock-owed suites move.
fn dispatched_before(engine: &Arc<engine::InMemoryEngine>, association_id: &str) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let run = store
        .association_mut(REGION, association_id)
        .and_then(|held| held.last_run_mut())
        .unwrap_or_else(|| panic!("the store must hold a run of {association_id}"));
    run.started_at -= ASSOCIATION_EXECUTION_DELAY + Duration::from_secs(1);
}

/// RULE: the compliance items a managed node holds are the patches its last patching
/// operation reported — one item per patch, of compliance type `Patch` on resource type
/// `ManagedInstance`, NON_COMPLIANT exactly for a patch the baseline wants on the node and is
/// not there, carrying the compliance level that baseline assigned it and the baseline the
/// run was performed against — and a filter narrows that list by one member of an item.
///
/// The rows differ along what the rule generalises over: the member a filter compares, the
/// comparison it uses, a filter that admits nothing, and the keys and resource types a read
/// is refused for.
#[tokio::test]
async fn the_compliance_items_of_a_node_are_the_patches_its_run_reported() {
    let (router, engine) = service();
    let case = "a scanned node";
    let instance_id = "i-0eee5555";
    let baseline_id = baseline(&router, case, "Compliance.Linux", LINUX_OPERATING_SYSTEM).await;
    register_for_group(&router, case, &baseline_id, REGISTERED_GROUP).await;
    register_node(
        &engine,
        node(instance_id, &[(PATCH_GROUP_TAG, REGISTERED_GROUP)]),
    );
    scan(&router, case, instance_id).await;

    let report = patch_state(&router, case, instance_id).await;
    let items = compliance_items(&router, case, instance_id, json!([])).await;
    assert!(
        !items.is_empty(),
        "case {case:?}: expected the scan's patches to be reported as compliance items, got \
         {items:?}"
    );
    for item in &items {
        assert_eq!(
            (
                item["ComplianceType"].as_str(),
                item["ResourceType"].as_str(),
                item["ResourceId"].as_str()
            ),
            (
                Some(COMPLIANCE_TYPE_PATCH),
                Some(RESOURCE_TYPE_MANAGED_INSTANCE),
                Some(instance_id)
            ),
            "case {case:?}: expected a patch item of this node, got {item}"
        );
        assert!(
            item["Id"].as_str().is_some_and(|id| !id.is_empty())
                && item["Title"].as_str().is_some(),
            "case {case:?}: expected the patch's own id and title, got {item}"
        );
        assert_eq!(
            item["Details"][
                // The baseline the run was performed against, which every item of this run
                // carries.
                "PatchBaselineId"
            ]
            .as_str(),
            Some(baseline_id.as_str()),
            "case {case:?}: expected the item to carry the baseline it was gathered against, \
             got {item}"
        );
        assert_eq!(
            item["ExecutionSummary"]["ExecutionType"].as_str(),
            Some("Command"),
            "case {case:?}: expected the run to be reported as a command, got {item}"
        );
    }

    // A patch the baseline wants on the node and is not there is what makes it
    // NON_COMPLIANT, and the counts of the high-level report are counted over those very
    // patches: the two readings agree.
    let non_compliant: Vec<&Value> = items
        .iter()
        .filter(|item| item["Status"].as_str() == Some("NON_COMPLIANT"))
        .collect();
    let missing = report["MissingCount"].as_i64().unwrap_or_default()
        + report["FailedCount"].as_i64().unwrap_or_default();
    assert_eq!(
        i64::try_from(non_compliant.len()).unwrap_or_default(),
        missing,
        "case {case:?}: expected as many non-compliant items as the report counts, got \
         {} against {report}",
        non_compliant.len()
    );
    assert!(
        non_compliant
            .iter()
            .all(|item| item["Severity"].as_str() == Some(COMPLIANCE_LEVEL)),
        "case {case:?}: expected every non-compliant item to carry the level the baseline \
         assigned, got {non_compliant:?}"
    );

    // (case, the filters, how many items they admit)
    let every = items.len();
    let rows: Vec<(&str, Value, usize)> = vec![
        (
            "a filter on the compliance type",
            json!([{ "Key": "ComplianceType", "Values": [COMPLIANCE_TYPE_PATCH] }]),
            every,
        ),
        (
            "a filter on the status, which admits the non-compliant items",
            json!([{ "Key": "Status", "Values": ["NON_COMPLIANT"] }]),
            non_compliant.len(),
        ),
        (
            "the same filter under NOT_EQUAL, which admits the rest",
            json!([{ "Key": "Status", "Values": ["NON_COMPLIANT"], "Type": "NOT_EQUAL" }]),
            every - non_compliant.len(),
        ),
        (
            "two filters, which narrow one another",
            json!([
                { "Key": "ComplianceType", "Values": [COMPLIANCE_TYPE_PATCH] },
                { "Key": "Status", "Values": ["NON_COMPLIANT"] },
            ]),
            non_compliant.len(),
        ),
        (
            "a filter on the severity the baseline assigned",
            json!([{ "Key": "Severity", "Values": [COMPLIANCE_LEVEL] }]),
            every,
        ),
        (
            "a filter naming a value no item carries",
            json!([{ "Key": "ComplianceType", "Values": ["Association"] }]),
            0,
        ),
        (
            "a LESS_THAN filter on an instant after the run",
            json!([{
                "Key": "ExecutionTime",
                "Values": ["2100-01-01T00:00:00Z"],
                "Type": "LESS_THAN",
            }]),
            every,
        ),
        (
            "a GREATER_THAN filter on that same instant",
            json!([{
                "Key": "ExecutionTime",
                "Values": ["2100-01-01T00:00:00Z"],
                "Type": "GREATER_THAN",
            }]),
            0,
        ),
    ];
    for (row, filters, expected) in &rows {
        let admitted = compliance_items(&router, row, instance_id, filters.clone()).await;
        assert_eq!(
            admitted.len(),
            *expected,
            "case {row:?}: expected {expected} items, got {}",
            admitted.len()
        );
    }

    // A BEGIN_WITH filter admits the items whose member starts with the value, which the
    // first item's own id is one of.
    let case = "a BEGIN_WITH filter on the item's own id";
    let first = items[0]["Id"]
        .as_str()
        .expect("every item carries the patch's own id")
        .to_string();
    let admitted = compliance_items(
        &router,
        case,
        instance_id,
        json!([{ "Key": "Id", "Values": [first], "Type": "BEGIN_WITH" }]),
    )
    .await;
    assert!(
        !admitted.is_empty()
            && admitted
                .iter()
                .all(|item| item["Id"].as_str().is_some_and(|id| id.starts_with(&first))),
        "case {case:?}: expected the items whose id starts with {first:?}, got {admitted:?}"
    );

    // (case, the request, the code it is refused with)
    let refusals: Vec<(&str, Value, &str)> = vec![
        (
            "a filter key that names no member of an item",
            json!({ "ResourceIds": [instance_id], "Filters": [{ "Key": "Unheard", "Values": ["x"] }] }),
            "InvalidFilter",
        ),
        (
            "a filter carrying no value to search for",
            json!({
                "ResourceIds": [instance_id],
                "Filters": [{ "Key": "Status", "Values": [""] }],
            }),
            "InvalidFilter",
        ),
        (
            // The comparison is a modelled enum, so a value outside it is refused by the
            // centralized boundary walk before this handler runs.
            "a comparison the model does not declare",
            json!({
                "ResourceIds": [instance_id],
                "Filters": [{ "Key": "Status", "Values": ["COMPLIANT"], "Type": "Equal" }],
            }),
            "ValidationException",
        ),
        (
            "a resource type other than the one supported",
            json!({ "ResourceIds": [instance_id], "ResourceTypes": ["Ec2Instance"] }),
            "InvalidResourceType",
        ),
        (
            "a resource id that is no managed-node id",
            json!({ "ResourceIds": ["not-a-node"] }),
            "InvalidResourceId",
        ),
        (
            "a page token this read never handed out",
            json!({ "ResourceIds": [instance_id], "NextToken": "halfway" }),
            "InvalidNextToken",
        ),
    ];
    for (row, request, expected) in &refusals {
        let (status, refused) = call(&router, "ListComplianceItems", request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, *expected),
            "case {row:?}: expected a {expected} refusal, got {status} with {refused}"
        );
    }

    // A page carries what was asked for and resumes where it stopped: the whole list read one
    // item at a time is the list read in one go.
    let case = "one item per page";
    let mut paged: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    // Bounded by the items there are, so a cursor that never advanced ends the walk instead
    // of looping.
    for _ in 0..=every {
        let mut body = json!({
            "ResourceIds": [instance_id],
            "MaxResults": 1,
        });
        if let Some(token) = token.as_deref() {
            body["NextToken"] = json!(token);
        }
        let page = call_ok(&router, case, "ListComplianceItems", &body).await;
        paged.extend(
            page["ComplianceItems"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
        token = page["NextToken"].as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        paged, items,
        "case {case:?}: expected the paged walk to report the same items in the same order, \
         got {paged:?}"
    );
}
